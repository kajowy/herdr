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
    pub(super) chunk_bytes: u32,
    pub(super) offset: u64,
    /// Final paths of committed files, in arrival order. Only these are pasted back.
    pub(super) committed: Vec<String>,
    pub(super) running: bool,
    pub(super) error: Option<String>,
    pub(super) done: bool,
}

// Used by `open_file_upload` below, which a later task's picker keybinding calls.
#[allow(dead_code)]
const DEFAULT_CHUNK_BYTES: u32 = 700_000;
/// sha256 of the empty input, which is what a directory entry announces.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

impl ClientShellState {
    // `open_file_upload`, `toggle_file_upload_destination`, `start_file_upload`, and
    // `cancel_file_upload` are the overlay's entry points; a later task wires them to the
    // picker keybinding and the overlay's toggle/start/cancel controls, so nothing calls them
    // outside tests yet.
    #[allow(dead_code)]
    pub(super) fn open_file_upload(
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

    #[allow(dead_code)]
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
                entry.bytes.saturating_sub(upload.offset),
            )
        };
        if remaining == 0 {
            self.push_endpoint_method_with_kind(
                crate::api::schema::Method::FilePutCommit(
                    crate::api::schema::FilePutCommitParams { transfer_id },
                ),
                PendingEndpointKind::FilePutCommit,
                outcome,
            );
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
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::FilePutChunk(crate::api::schema::FilePutChunkParams {
                transfer_id,
                offset,
                data_b64,
            }),
            PendingEndpointKind::FilePutChunk,
            outcome,
        );
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

    #[allow(dead_code)]
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

    fn fail_file_upload(&mut self, message: impl Into<String>) -> (bool, Vec<ClientShellAction>) {
        if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
            upload.running = false;
            upload.error = Some(message.into());
        }
        (true, Vec::new())
    }

    fn finish_file_upload(&mut self) -> (bool, Vec<ClientShellAction>) {
        if let Some(ClientShellOverlay::FileUpload(upload)) = self.overlay.as_mut() {
            upload.running = false;
            upload.done = true;
        }
        (true, Vec::new())
    }
}
