//! Drives one file/directory selection over the endpoint lane: begin, chunk, commit, abort,
//! one request at a time. The endpoint lane is single-flight, so this never issues a second
//! request before the previous one's response comes back.

use std::path::PathBuf;

use base64::Engine as _;

use super::*;
use crate::client::file_collect;

// `ClientShellOverlay` derives Debug (src/client/shell/state.rs:583), so this and
// `file_collect::CollectedEntry` must derive it too.
//
// `skipped` and `total_bytes` are read by the overlay's render/summary, which a later task
// wires up alongside the picker keybinding that opens this overlay; this driver only writes
// them for now.
#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct ClientFileUploadOverlay {
    pub(super) entries: Vec<file_collect::CollectedEntry>,
    pub(super) skipped: Vec<String>,
    pub(super) index: usize,
    pub(super) destination: crate::api::schema::FilePutDestination,
    pub(super) pane_id: Option<String>,
    pub(super) total_bytes: u64,
    pub(super) sent_bytes: u64,
    pub(super) transfer_id: Option<String>,
    /// The size declared to the server in `file.put.begin` for the entry currently in flight
    /// (from `hash_file` at send time, not the walk-time `CollectedEntry::bytes`, which may be
    /// stale). The chunk loop must stop and commit based on this number, since it is the only
    /// size the server ever saw.
    pub(super) transfer_bytes: u64,
    pub(super) chunk_bytes: u32,
    pub(super) offset: u64,
    /// Final paths of committed files, in arrival order. Only these are pasted back.
    pub(super) committed: Vec<String>,
    pub(super) running: bool,
    pub(super) error: Option<String>,
    pub(super) done: bool,
}

const DEFAULT_CHUNK_BYTES: u32 = 700_000;
/// sha256 of the empty input, which is what a directory entry announces.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

impl ClientShellState {
    // `open_file_upload` is wired to the picker keybinding (prefix+u); `start_file_upload` and
    // `cancel_file_upload` are wired to the overlay's start/cancel controls.
    // `toggle_file_upload_destination` is wired to its toggle control by a later task, so nothing
    // calls it outside tests yet.
    pub(crate) fn open_file_upload(
        &mut self,
        selection: &[PathBuf],
        outcome: &mut ClientShellInput,
    ) {
        let collection = file_collect::collect(selection);
        if collection.entries.is_empty() {
            self.set_endpoint_error("Nothing in that selection can be sent.");
            outcome.repaint = true;
            return;
        }
        let total_bytes = collection.entries.iter().map(|entry| entry.bytes).sum();
        self.overlay = Some(ClientShellOverlay::FileUpload(ClientFileUploadOverlay {
            entries: collection.entries,
            skipped: collection.skipped,
            index: 0,
            destination: self.file_upload_destination,
            pane_id: self.focused_pane_id(),
            total_bytes,
            sent_bytes: 0,
            transfer_id: None,
            transfer_bytes: 0,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            offset: 0,
            committed: Vec::new(),
            running: false,
            error: None,
            done: false,
        }));
        outcome.repaint = true;
    }

    #[allow(dead_code)]
    pub(super) fn toggle_file_upload_destination(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
            return;
        };
        if upload.running {
            return;
        }
        upload.destination = match upload.destination {
            crate::api::schema::FilePutDestination::Inbox => {
                crate::api::schema::FilePutDestination::PaneCwd
            }
            crate::api::schema::FilePutDestination::PaneCwd => {
                crate::api::schema::FilePutDestination::Inbox
            }
        };
        self.file_upload_destination = upload.destination;
        outcome.repaint = true;
    }

    pub(super) fn start_file_upload(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
            return;
        };
        if upload.running || upload.done {
            return;
        }
        upload.running = true;
        upload.error = None;
        self.send_file_put_begin(outcome);
    }

    fn send_file_put_begin(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
            return;
        };
        let Some(entry) = upload.entries.get(upload.index) else {
            upload.running = false;
            upload.done = true;
            outcome.repaint = true;
            return;
        };
        // A directory entry has nothing to read: it carries the empty digest and zero bytes,
        // and the server finishes it at begin.
        let is_directory = matches!(entry.kind, crate::api::schema::FilePutEntryKind::Directory);
        let (sha256, bytes) = if is_directory {
            (EMPTY_SHA256.to_owned(), 0)
        } else {
            match file_collect::hash_file(&entry.path) {
                Ok(hashed) => hashed,
                Err(err) => {
                    upload.running = false;
                    upload.error = Some(format!("could not read this file: {err}"));
                    outcome.repaint = true;
                    return;
                }
            }
        };
        let suggested_name = entry
            .relative_path
            .as_deref()
            .and_then(|relative| relative.rsplit('/').next())
            .map(str::to_owned)
            .or_else(|| {
                entry
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "upload".to_owned());
        let params = crate::api::schema::FilePutBeginParams {
            suggested_name,
            relative_path: entry.relative_path.clone(),
            entry_kind: entry.kind,
            bytes,
            sha256,
            destination: upload.destination,
            pane_id: upload.pane_id.clone(),
        };
        upload.offset = 0;
        upload.transfer_id = None;
        // Drive the chunk loop from the size just declared to the server, not the walk-time
        // `entry.bytes`, which may be stale if the file changed size since the walk.
        upload.transfer_bytes = bytes;
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::FilePutBegin(params),
            PendingEndpointKind::FilePutBegin,
            outcome,
        ) {
            if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
                upload.running = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn complete_file_put_begin(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let complete = match result {
            Ok(crate::api::schema::ResponseResult::FilePutBegan {
                transfer_id,
                chunk_bytes,
                complete,
                ..
            }) => {
                let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
                    return (false, Vec::new());
                };
                upload.chunk_bytes = chunk_bytes.max(1);
                if complete {
                    // A directory entry is already on disk; move on to the next entry.
                    upload.transfer_id = None;
                    upload.offset = 0;
                    upload.index = upload.index.saturating_add(1);
                } else {
                    upload.transfer_id = Some(transfer_id);
                }
                complete
            }
            Ok(_) => return self.fail_file_upload("this server sent an unexpected response"),
            // A name this server will not accept is this entry's problem, not the selection's:
            // skip it and keep going, so one refused name cannot leave a half-populated tree
            // behind with no way to continue. Every other `begin` error still fails the upload.
            Err(error) if error.code.as_deref() == Some("invalid_file_path") => {
                return self.skip_file_upload_entry(error.message);
            }
            Err(error) => return self.fail_file_upload(error.message),
        };
        let mut outcome = ClientShellInput::default();
        if complete {
            let finished = self
                .overlay
                .as_ref()
                .and_then(|overlay| match overlay {
                    ClientShellOverlay::FileUpload(upload) => {
                        Some(upload.index >= upload.entries.len())
                    }
                    _ => None,
                })
                .unwrap_or(true);
            if finished {
                return self.finish_file_upload();
            }
            self.send_file_put_begin(&mut outcome);
            return (true, outcome.actions);
        }
        self.send_next_file_put_chunk(&mut outcome);
        (true, outcome.actions)
    }

    fn send_next_file_put_chunk(&mut self, outcome: &mut ClientShellInput) {
        let (path, offset, chunk_bytes, transfer_id, remaining) = {
            let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_ref() else {
                return;
            };
            let Some(entry) = upload.entries.get(upload.index) else {
                return;
            };
            let Some(transfer_id) = upload.transfer_id.clone() else {
                return;
            };
            (
                entry.path.clone(),
                upload.offset,
                upload.chunk_bytes as usize,
                transfer_id,
                // The declared size, not `entry.bytes`: the server only ever saw the former.
                upload.transfer_bytes.saturating_sub(upload.offset),
            )
        };
        if remaining == 0 {
            // Unlike `send_file_put_begin`, a refused send here has nothing else pending: no
            // request was queued, so nothing will ever time out to unstick the overlay. Fail it
            // now rather than leave `running: true` forever.
            if !self.push_endpoint_method_with_kind(
                crate::api::schema::Method::FilePutCommit(
                    crate::api::schema::FilePutCommitParams { transfer_id },
                ),
                PendingEndpointKind::FilePutCommit,
                outcome,
            ) {
                self.mark_file_upload_send_refused();
            }
            return;
        }
        let take = chunk_bytes.min(remaining as usize);
        let data = match file_collect::read_chunk(&path, offset, take) {
            Ok(data) => data,
            Err(err) => {
                let (_, actions) =
                    self.fail_file_upload(format!("could not read this file: {err}"));
                outcome.actions.extend(actions);
                return;
            }
        };
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::FilePutChunk(crate::api::schema::FilePutChunkParams {
                transfer_id,
                offset,
                data_b64,
            }),
            PendingEndpointKind::FilePutChunk,
            outcome,
        ) {
            self.mark_file_upload_send_refused();
        }
    }

    /// The lane refused to carry a chunk or commit send (the endpoint is offline, or its surface
    /// is no longer active): `push_endpoint_method_with_kind` already surfaced its own notice, but
    /// nothing was queued, so nothing will ever time out to move this transfer forward or fail it
    /// on its own. Fail it here instead of leaving the overlay `running` with no pending activity
    /// and no way out but manual cancel.
    fn mark_file_upload_send_refused(&mut self) {
        if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
            upload.running = false;
            upload.error = Some("Lost the connection to the server mid-transfer.".to_owned());
        }
    }

    pub(super) fn complete_file_put_chunk(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match result {
            Ok(crate::api::schema::ResponseResult::FilePutChunkAccepted {
                next_offset, ..
            }) => {
                let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
                    return (false, Vec::new());
                };
                upload.sent_bytes = upload
                    .sent_bytes
                    .saturating_add(next_offset.saturating_sub(upload.offset));
                upload.offset = next_offset;
            }
            Ok(_) => return self.fail_file_upload("this server sent an unexpected response"),
            Err(error) => return self.fail_file_upload(error.message),
        }
        let mut outcome = ClientShellInput::default();
        self.send_next_file_put_chunk(&mut outcome);
        (true, outcome.actions)
    }

    pub(super) fn complete_file_put_commit(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let path = match result {
            Ok(crate::api::schema::ResponseResult::FilePutCommitted { path, .. }) => path,
            Ok(_) => return self.fail_file_upload("this server sent an unexpected response"),
            Err(error) => return self.fail_file_upload(error.message),
        };
        let finished = {
            let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
                return (false, Vec::new());
            };
            upload.committed.push(path);
            upload.transfer_id = None;
            upload.offset = 0;
            upload.index = upload.index.saturating_add(1);
            upload.index >= upload.entries.len()
        };
        let mut outcome = ClientShellInput::default();
        if finished {
            return self.finish_file_upload();
        }
        self.send_file_put_begin(&mut outcome);
        (true, outcome.actions)
    }

    pub(super) fn cancel_file_upload(&mut self, outcome: &mut ClientShellInput) {
        let pending = match self.overlay.take() {
            Some(ClientShellOverlay::FileUpload(upload)) => upload.transfer_id,
            other => {
                self.overlay = other;
                return;
            }
        };
        if let Some(transfer_id) = pending {
            self.push_endpoint_method_with_kind(
                crate::api::schema::Method::FilePutAbort(crate::api::schema::FilePutAbortParams {
                    transfer_id,
                }),
                PendingEndpointKind::FilePutAbort,
                outcome,
            );
        }
        outcome.repaint = true;
    }

    /// Record the entry currently in flight as skipped and advance to the next one. Mirrors
    /// `complete_file_put_commit`'s advance, minus the committed path.
    fn skip_file_upload_entry(&mut self, reason: String) -> (bool, Vec<ClientShellAction>) {
        let finished = {
            let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
                return (false, Vec::new());
            };
            let label = upload
                .entries
                .get(upload.index)
                .map(entry_label)
                .unwrap_or_else(|| "this entry".to_owned());
            upload.skipped.push(format!("{label}: {reason}"));
            upload.transfer_id = None;
            upload.offset = 0;
            upload.transfer_bytes = 0;
            upload.index = upload.index.saturating_add(1);
            upload.index >= upload.entries.len()
        };
        let mut outcome = ClientShellInput::default();
        if finished {
            return self.finish_file_upload();
        }
        self.send_file_put_begin(&mut outcome);
        (true, outcome.actions)
    }

    fn fail_file_upload(&mut self, message: impl Into<String>) -> (bool, Vec<ClientShellAction>) {
        if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
            upload.running = false;
            upload.error = Some(message.into());
        }
        (true, Vec::new())
    }

    fn finish_file_upload(&mut self) -> (bool, Vec<ClientShellAction>) {
        let (committed, pane_id) = {
            let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() else {
                return (false, Vec::new());
            };
            upload.running = false;
            upload.done = true;
            (upload.committed.clone(), upload.pane_id.clone())
        };
        // A directory-only selection commits no files, so there is nothing to hand back.
        if committed.is_empty() {
            return (true, Vec::new());
        }
        let Some(quoted) = committed
            .iter()
            .map(|path| quote_upload_path(path))
            .collect::<Option<Vec<String>>>()
        else {
            if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
                upload.error =
                    Some("the server reported a path this client will not repeat".to_owned());
            }
            return (true, Vec::new());
        };
        let text = quoted.join(" ");
        let pane_runs_agent = pane_id.as_deref().is_some_and(|pane_id| {
            self.snapshot.as_deref().is_some_and(|snapshot| {
                snapshot.agents.iter().any(|agent| agent.pane_id == pane_id)
            })
        });
        match pane_id.filter(|_| !pane_runs_agent) {
            Some(pane_id) => (true, vec![ClientShellAction::PastePane { pane_id, text }]),
            None => (
                true,
                vec![ClientShellAction::ClipboardWrite(text.into_bytes())],
            ),
        }
    }
}

/// How one entry is named to the user: its path relative to the picked directory, or the picked
/// file's own name.
fn entry_label(entry: &file_collect::CollectedEntry) -> String {
    entry
        .relative_path
        .clone()
        .or_else(|| {
            entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "upload".to_owned())
}

/// Characters a herdr-written path may contain. Everything else, including every control
/// character and every bracketed-paste terminator byte, disqualifies the path from the pane.
pub(super) fn quote_upload_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let acceptable = path.chars().all(|ch| {
        ch == ' '
            || ch == '\''
            || (ch.is_ascii_graphic() && ch != '\u{7f}')
            || (!ch.is_control() && ch.is_alphanumeric())
    });
    if !acceptable {
        return None;
    }
    if path.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
            )
    }) {
        return Some(path.to_owned());
    }
    Some(format!("'{}'", path.replace('\'', "'\\''")))
}
