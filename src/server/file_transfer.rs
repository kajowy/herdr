//! Per-client file transfer registry and temp-file lifecycle.
//!
//! Error codes produced in this module:
//! - `transfer_busy` — the connection already has a transfer open.
//! - `transfer_not_found` — no active transfer matches this client and transfer id.
//! - `transfer_offset_mismatch` — a chunk's offset does not match bytes written so far, or
//!   `commit` was called before all announced bytes arrived.
//! - `transfer_checksum_mismatch` — the assembled bytes do not match the announced sha256.
//! - `transfer_too_large` — a file, or a connection's running total, exceeds the configured cap.
//! - `transfer_chunk_too_large` — a single chunk exceeds the configured `chunk_bytes`.
//! - `transfer_write_failed` — an I/O error while creating, writing, or placing the file.
//! - `transfer_name_collision` — placement lost every retry to a concurrently created file of
//!   the same generated name; extremely unlikely, but returned rather than clobbering.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest as _, Sha256};

pub(crate) mod destination;

const TEMP_SUFFIX: &str = ".herdr-part";
const STALE_TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Bound on retrying a collided no-replace placement with a freshly generated name. Each retry
/// calls `unique_target`, which itself scans up to 1000 numbered variants, so this only bounds
/// how many times a genuine concurrent-creation race can make us start that scan over.
const MAX_COMMIT_COLLISION_RETRIES: u32 = 8;
/// How deep the start-up temp sweep descends. Uploaded paths are capped at 32 components, so this
/// reaches everything an upload could have created.
const REAP_MAX_DEPTH: usize = 32;
/// Directory entries one sweep may examine. An inbox the user also keeps other things in must not
/// be able to turn a reap into an unbounded walk.
const REAP_ENTRY_BUDGET: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransferError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl TransferError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn from_io(code: &'static str, err: &io::Error) -> Self {
        Self::new(code, err.to_string())
    }
}

pub(crate) struct FileTransferConfig {
    pub(crate) inbox: PathBuf,
    pub(crate) max_file_bytes: u64,
    pub(crate) max_total_bytes: u64,
    pub(crate) chunk_bytes: u32,
}

/// One entry a `file.put.begin` asks for. Grouping the wire fields keeps `begin` readable.
pub(crate) struct BeginEntry<'a> {
    pub(crate) suggested_name: &'a str,
    pub(crate) relative_path: Option<&'a str>,
    pub(crate) kind: crate::api::schema::FilePutEntryKind,
    pub(crate) bytes: u64,
    pub(crate) sha256: &'a str,
}

struct ActiveTransfer {
    transfer_id: String,
    temp_path: PathBuf,
    /// Directory the final file lands in, kept so `commit` can re-run `unique_target` against
    /// it on a placement collision instead of trusting a name picked at `begin`.
    dir: PathBuf,
    /// Original (non-uniqified) leaf name, re-passed to `unique_target` on each retry.
    leaf_name: String,
    file: fs::File,
    expected_bytes: u64,
    written_bytes: u64,
    expected_sha256: String,
    hasher: Sha256,
}

pub(crate) struct FileTransferRegistry {
    config: FileTransferConfig,
    active: HashMap<u64, ActiveTransfer>,
    /// Per-client cumulative bytes committed so far, enforced against `max_total_bytes` in
    /// `begin`. Only `forget_client` clears an entry, and only a real connection teardown (clean
    /// disconnect, error, or timeout) may call it — otherwise a departed client's entry leaks for
    /// the life of the server, and a routine UI event that merely deactivates the surface would
    /// hand the connection a fresh budget and make the cap decorative.
    session_bytes: HashMap<u64, u64>,
    next_transfer_serial: u64,
}

#[derive(Debug)]
pub(crate) struct BeginAccepted {
    pub(crate) transfer_id: String,
    pub(crate) chunk_bytes: u32,
    pub(crate) destination_label: String,
    /// True for a directory entry: it is already on disk, so no chunk or commit follows.
    pub(crate) complete: bool,
    /// Final path of a completed entry; `None` while a file transfer stays open.
    pub(crate) path: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct CommittedFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

impl FileTransferRegistry {
    pub(crate) fn new(config: FileTransferConfig) -> Self {
        // Server start is the one moment a full sweep is affordable, and the only one that can
        // reach an orphan left inside an uploaded subdirectory by a server that died mid-transfer.
        // Bounded in depth and in entries examined so a large inbox cannot delay startup.
        reap_stale_temp_tree(&config.inbox);
        Self {
            config,
            active: HashMap::new(),
            session_bytes: HashMap::new(),
            next_transfer_serial: 0,
        }
    }

    pub(crate) fn config(&self) -> &FileTransferConfig {
        &self.config
    }

    pub(crate) fn begin(
        &mut self,
        client_id: u64,
        root: &Path,
        home: &Path,
        allow_subdirectories: bool,
        entry: BeginEntry<'_>,
    ) -> Result<BeginAccepted, TransferError> {
        if self.active.contains_key(&client_id) {
            return Err(TransferError::new(
                "transfer_busy",
                "this connection already has a transfer in flight",
            ));
        }
        if entry.bytes > self.config.max_file_bytes {
            return Err(TransferError::new(
                "transfer_too_large",
                format!(
                    "this file exceeds the {} byte limit",
                    self.config.max_file_bytes
                ),
            ));
        }
        let session_total = self.session_bytes.get(&client_id).copied().unwrap_or(0);
        if session_total.saturating_add(entry.bytes) > self.config.max_total_bytes {
            return Err(TransferError::new(
                "transfer_too_large",
                format!(
                    "this connection has reached the {} byte transfer limit",
                    self.config.max_total_bytes
                ),
            ));
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TransferError::new(
                "invalid_file_path",
                "the checksum is not a sha256 hex digest",
            ));
        }

        let root = destination::validate_root(root, home)?;
        destination::create_root_private(&root)
            .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?;

        let mut components = destination::resolve_relative_path(
            entry.relative_path,
            entry.suggested_name,
            allow_subdirectories,
        )?;
        let Some(leaf_name) = components.pop() else {
            return Err(TransferError::new(
                "invalid_file_path",
                "this path has no file name",
            ));
        };
        let dir = destination::ensure_directory_no_follow(&root, &components)
            .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?;
        // Reap in the directory this transfer's temp file will live in, not just the root: temps
        // are created beside the final file, so an orphan inside an uploaded subdirectory is only
        // visible from here. One non-recursive `read_dir` of the directory we are about to write
        // to; the recursive sweep happens once at server start.
        reap_stale_temp_files(&dir);
        let destination_label = root.to_string_lossy().into_owned();

        self.next_transfer_serial = self.next_transfer_serial.saturating_add(1);
        let transfer_id = format!("ft-{client_id}-{}", self.next_transfer_serial);

        // A directory entry is the whole transfer: create it and answer complete, so an empty
        // directory survives without a marker file in the user's tree.
        if matches!(entry.kind, crate::api::schema::FilePutEntryKind::Directory) {
            if entry.bytes != 0 {
                return Err(TransferError::new(
                    "invalid_file_path",
                    "a directory entry cannot carry bytes",
                ));
            }
            let created = match destination::existing_directory(&dir, &leaf_name) {
                Some(existing) => existing,
                None => {
                    let target = destination::unique_target(&dir, &leaf_name)?;
                    let Some(unique_name) = target
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                    else {
                        return Err(TransferError::new(
                            "invalid_file_path",
                            "this directory name is not representable",
                        ));
                    };
                    destination::ensure_directory_no_follow(&dir, &[unique_name])
                        .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?
                }
            };
            return Ok(BeginAccepted {
                transfer_id,
                chunk_bytes: self.config.chunk_bytes,
                destination_label,
                complete: true,
                path: Some(created),
            });
        }

        let temp_path = dir.join(format!("{transfer_id}{TEMP_SUFFIX}"));
        let file = create_private_new(&temp_path)
            .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?;

        self.active.insert(
            client_id,
            ActiveTransfer {
                transfer_id: transfer_id.clone(),
                temp_path,
                dir,
                leaf_name,
                file,
                expected_bytes: entry.bytes,
                written_bytes: 0,
                expected_sha256: entry.sha256.to_ascii_lowercase(),
                hasher: Sha256::new(),
            },
        );
        Ok(BeginAccepted {
            transfer_id,
            chunk_bytes: self.config.chunk_bytes,
            destination_label,
            complete: false,
            path: None,
        })
    }

    pub(crate) fn chunk(
        &mut self,
        client_id: u64,
        transfer_id: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, TransferError> {
        if data.len() as u64 > u64::from(self.config.chunk_bytes) {
            return Err(TransferError::new(
                "transfer_chunk_too_large",
                format!(
                    "this chunk exceeds the configured {} byte chunk size",
                    self.config.chunk_bytes
                ),
            ));
        }
        let transfer = self.lookup_mut(client_id, transfer_id)?;
        if offset != transfer.written_bytes {
            return Err(TransferError::new(
                "transfer_offset_mismatch",
                format!("expected offset {}", transfer.written_bytes),
            ));
        }
        let next = transfer.written_bytes.saturating_add(data.len() as u64);
        if next > transfer.expected_bytes {
            return Err(TransferError::new(
                "transfer_too_large",
                "this chunk exceeds the announced file size",
            ));
        }
        if let Err(err) = transfer.file.write_all(data) {
            let error = TransferError::from_io("transfer_write_failed", &err);
            self.discard(client_id);
            return Err(error);
        }
        transfer.hasher.update(data);
        transfer.written_bytes = next;
        Ok(next)
    }

    pub(crate) fn commit(
        &mut self,
        client_id: u64,
        transfer_id: &str,
    ) -> Result<CommittedFile, TransferError> {
        {
            let transfer = self.lookup_mut(client_id, transfer_id)?;
            if transfer.written_bytes != transfer.expected_bytes {
                return Err(TransferError::new(
                    "transfer_offset_mismatch",
                    format!(
                        "expected {} bytes, received {}",
                        transfer.expected_bytes, transfer.written_bytes
                    ),
                ));
            }
        }
        let Some(transfer) = self.active.remove(&client_id) else {
            return Err(not_found());
        };
        let digest = format!("{:x}", transfer.hasher.clone().finalize());
        if digest != transfer.expected_sha256 {
            let _ = fs::remove_file(&transfer.temp_path);
            return Err(TransferError::new(
                "transfer_checksum_mismatch",
                "the received bytes do not match the announced checksum",
            ));
        }
        if let Err(err) = transfer.file.sync_all() {
            let _ = fs::remove_file(&transfer.temp_path);
            return Err(TransferError::from_io("transfer_write_failed", &err));
        }
        drop(transfer.file);
        let final_path =
            match place_committed_file(&transfer.temp_path, &transfer.dir, &transfer.leaf_name) {
                Ok(path) => path,
                Err(err) => {
                    let _ = fs::remove_file(&transfer.temp_path);
                    return Err(err);
                }
            };
        *self.session_bytes.entry(client_id).or_default() += transfer.written_bytes;
        Ok(CommittedFile {
            path: final_path,
            bytes: transfer.written_bytes,
        })
    }

    pub(crate) fn abort(&mut self, client_id: u64, transfer_id: &str) -> Result<(), TransferError> {
        let matches = self
            .active
            .get(&client_id)
            .is_some_and(|transfer| transfer.transfer_id == transfer_id);
        if !matches {
            return Err(not_found());
        }
        self.discard(client_id);
        Ok(())
    }

    /// Drop this client's in-flight transfer and its temp file, keeping its byte budget. Used
    /// where the connection survives but cannot carry a transfer any more, such as a surface
    /// deactivation.
    pub(crate) fn abort_client_transfer(&mut self, client_id: u64) {
        self.discard(client_id);
    }

    /// Forget this client entirely, budget included. Only for a real connection teardown.
    pub(crate) fn forget_client(&mut self, client_id: u64) {
        self.discard(client_id);
        self.session_bytes.remove(&client_id);
    }

    fn discard(&mut self, client_id: u64) {
        if let Some(transfer) = self.active.remove(&client_id) {
            drop(transfer.file);
            let _ = fs::remove_file(&transfer.temp_path);
        }
    }

    fn lookup_mut(
        &mut self,
        client_id: u64,
        transfer_id: &str,
    ) -> Result<&mut ActiveTransfer, TransferError> {
        match self.active.get_mut(&client_id) {
            Some(transfer) if transfer.transfer_id == transfer_id => Ok(transfer),
            _ => Err(not_found()),
        }
    }
}

fn not_found() -> TransferError {
    TransferError::new("transfer_not_found", "this transfer is no longer active")
}

/// Place a committed temp file under `dir` using `leaf_name`, atomically refusing to replace an
/// existing file. `unique_target` alone cannot guarantee this: it only checks that a name is
/// free at the moment it runs, which leaves a window between that check and the eventual
/// `rename` where a second transfer of the same name can land first. Each attempt here re-runs
/// `unique_target` for a fresh candidate and then performs the placement with a rename that
/// fails closed (rather than silently replacing) when the candidate is taken, so the window
/// only costs a retry rather than data loss.
fn place_committed_file(
    temp_path: &Path,
    dir: &Path,
    leaf_name: &str,
) -> Result<PathBuf, TransferError> {
    for _ in 0..MAX_COMMIT_COLLISION_RETRIES {
        let candidate = destination::unique_target(dir, leaf_name)?;
        match rename_no_replace(temp_path, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(TransferError::from_io("transfer_write_failed", &err)),
        }
    }
    Err(TransferError::new(
        "transfer_name_collision",
        "could not place this file after repeated name collisions",
    ))
}

/// Rename `from` to `to`, failing with `io::ErrorKind::AlreadyExists` instead of replacing `to`
/// when it already exists.
#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a nul byte"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a nul byte"))?;
    // SAFETY: `from` and `to` are valid nul-terminated C strings for the duration of this call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Rename `from` to `to`, failing with `io::ErrorKind::AlreadyExists` instead of replacing `to`
/// when it already exists.
#[cfg(target_os = "macos")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a nul byte"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a nul byte"))?;
    // SAFETY: `from` and `to` are valid nul-terminated C strings for the duration of this call.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Portable fallback for platforms without an atomic no-replace rename syscall: a hard link
/// fails with `AlreadyExists` rather than replacing `to`, and removing `from` afterward leaves
/// exactly one directory entry pointing at the data, matching a successful rename's outcome. A
/// failure between the two steps can leave both entries; that residue is a correctness gap this
/// fork accepts on platforms outside its Linux/macOS CI, in exchange for never clobbering.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

fn create_private_new(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    restrict_file_options(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

fn reap_stale_temp_files(dir: &Path) {
    let mut budget = REAP_ENTRY_BUDGET;
    reap_stale_temp_files_bounded(dir, 0, &mut budget);
}

/// Sweep `root` and everything under it once, for a server that died mid-transfer and left temp
/// files inside uploaded subdirectories. Never called on a request path.
fn reap_stale_temp_tree(root: &Path) {
    let mut budget = REAP_ENTRY_BUDGET;
    reap_stale_temp_files_bounded(root, REAP_MAX_DEPTH, &mut budget);
}

/// Remove stale temp files in `dir`, descending at most `depth` levels further. `budget` bounds
/// the total number of directory entries examined across the whole sweep, so neither the start-up
/// sweep nor a per-transfer reap can walk an unbounded tree.
fn reap_stale_temp_files_bounded(dir: &Path, depth: usize, budget: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut nested = Vec::new();
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if depth > 0 {
                nested.push(entry.path());
            }
            continue;
        }
        if !entry.file_name().to_string_lossy().ends_with(TEMP_SUFFIX) {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().unwrap_or_default() > STALE_TEMP_MAX_AGE {
            let _ = fs::remove_file(entry.path());
        }
    }
    for path in nested {
        reap_stale_temp_files_bounded(&path, depth - 1, budget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(root: &Path) -> FileTransferRegistry {
        FileTransferRegistry::new(FileTransferConfig {
            inbox: root.join("inbox"),
            max_file_bytes: 1024,
            max_total_bytes: 4096,
            chunk_bytes: 8,
        })
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};

        format!("{:x}", Sha256::digest(data))
    }

    fn file_entry<'a>(name: &'a str, sha256: &'a str, bytes: u64) -> BeginEntry<'a> {
        BeginEntry {
            suggested_name: name,
            relative_path: None,
            kind: crate::api::schema::FilePutEntryKind::File,
            bytes,
            sha256,
        }
    }

    fn dir_entry<'a>(relative: &'a str, name: &'a str) -> BeginEntry<'a> {
        BeginEntry {
            suggested_name: name,
            relative_path: Some(relative),
            kind: crate::api::schema::FilePutEntryKind::Directory,
            bytes: 0,
            sha256: EMPTY_SHA256,
        }
    }

    /// sha256 of the empty input; a directory entry carries no bytes.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-transfer-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_full_transfer_lands_atomically() {
        let home = scratch("full");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let data = b"hello upload".to_vec();
        let digest = sha256_hex(&data);
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("note.txt", &digest, data.len() as u64),
            )
            .unwrap();
        assert!(!accepted.complete);
        let mut offset = 0u64;
        for chunk in data.chunks(8) {
            offset = registry
                .chunk(1, &accepted.transfer_id, offset, chunk)
                .unwrap();
        }
        let committed = registry.commit(1, &accepted.transfer_id).unwrap();
        assert_eq!(committed.path, root.join("note.txt"));
        assert_eq!(std::fs::read(&committed.path).unwrap(), data);
        assert!(std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".part")));
    }

    #[test]
    fn a_directory_entry_completes_at_begin_and_leaves_no_marker() {
        let home = scratch("dir");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();

        let accepted = registry
            .begin(1, &root, &home, true, dir_entry("tree/empty", "empty"))
            .unwrap();
        assert!(accepted.complete);
        assert_eq!(
            accepted.path.as_deref(),
            Some(root.join("tree/empty").as_path())
        );
        assert!(root.join("tree/empty").is_dir());
        assert_eq!(
            std::fs::read_dir(root.join("tree/empty")).unwrap().count(),
            0,
            "an empty directory must arrive empty"
        );

        // Completing at begin frees the lane, so the next entry is accepted immediately.
        let digest = sha256_hex(b"abcd");
        registry
            .begin(
                1,
                &root,
                &home,
                true,
                BeginEntry {
                    suggested_name: "b.txt",
                    relative_path: Some("tree/empty/b.txt"),
                    kind: crate::api::schema::FilePutEntryKind::File,
                    bytes: 4,
                    sha256: &digest,
                },
            )
            .unwrap();
    }

    #[test]
    fn a_directory_entry_is_refused_outside_the_inbox() {
        let home = scratch("dir-single");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let err = registry
            .begin(1, &root, &home, false, dir_entry("tree/empty", "empty"))
            .unwrap_err();
        assert_eq!(err.code, "invalid_file_path");
    }

    #[test]
    fn a_directory_entry_never_follows_a_symlinked_component() {
        let home = scratch("dir-symlink");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(root.join("real")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        #[cfg(unix)]
        {
            let err = registry
                .begin(1, &root, &home, true, dir_entry("link/nested", "nested"))
                .unwrap_err();
            assert_eq!(err.code, "transfer_write_failed");
            assert_eq!(std::fs::read_dir(root.join("real")).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_second_transfer_for_one_client_is_refused() {
        let home = scratch("busy");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let digest_a = sha256_hex(b"a");
        let digest_b = sha256_hex(b"b");
        registry
            .begin(1, &root, &home, true, file_entry("a.txt", &digest_a, 1))
            .unwrap();
        let err = registry
            .begin(1, &root, &home, true, file_entry("b.txt", &digest_b, 1))
            .unwrap_err();
        assert_eq!(err.code, "transfer_busy");
    }

    #[test]
    fn a_wrong_offset_is_refused_without_appending() {
        let home = scratch("offset");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry.chunk(1, &accepted.transfer_id, 0, b"ab").unwrap();
        let err = registry
            .chunk(1, &accepted.transfer_id, 0, b"ab")
            .unwrap_err();
        assert_eq!(err.code, "transfer_offset_mismatch");
        assert_eq!(
            registry.chunk(1, &accepted.transfer_id, 2, b"cd").unwrap(),
            4
        );
    }

    #[test]
    fn a_checksum_mismatch_removes_the_temp_file() {
        let home = scratch("checksum");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry
            .chunk(1, &accepted.transfer_id, 0, b"abce")
            .unwrap();
        let err = registry.commit(1, &accepted.transfer_id).unwrap_err();
        assert_eq!(err.code, "transfer_checksum_mismatch");
        assert!(!root.join("a.txt").exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().count(), 0);
    }

    #[test]
    fn an_oversized_file_is_refused_before_any_bytes_move() {
        let home = scratch("limit");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let digest = sha256_hex(b"a");
        let err = registry
            .begin(1, &root, &home, true, file_entry("a.txt", &digest, 4096))
            .unwrap_err();
        assert_eq!(err.code, "transfer_too_large");
    }

    #[test]
    fn abort_and_client_teardown_remove_temp_files() {
        let home = scratch("abort");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry.chunk(1, &accepted.transfer_id, 0, b"ab").unwrap();
        registry.abort(1, &accepted.transfer_id).unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().count(), 0);
        assert_eq!(
            registry.abort(1, &accepted.transfer_id).unwrap_err().code,
            "transfer_not_found"
        );

        let accepted = registry
            .begin(
                2,
                &root,
                &home,
                true,
                file_entry("b.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry.chunk(2, &accepted.transfer_id, 0, b"ab").unwrap();
        registry.forget_client(2);
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_inbox_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = scratch("inbox-mode");
        let root = home.join("nested/herdr-inbox");
        let mut registry = FileTransferRegistry::new(FileTransferConfig {
            inbox: root.clone(),
            max_file_bytes: 1024,
            max_total_bytes: 4096,
            chunk_bytes: 8,
        });
        registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();

        for created in [home.join("nested"), root] {
            let mode = std::fs::metadata(&created).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} must not be group- or world-readable",
                created.display()
            );
        }
    }

    #[test]
    fn stale_temp_files_are_reaped_at_start_including_inside_subdirectories() {
        let home = scratch("reap");
        let root = home.join("inbox");
        std::fs::create_dir_all(root.join("tree/nested")).unwrap();
        let stale_root = root.join(format!("ft-1-1{TEMP_SUFFIX}"));
        let stale_nested = root.join(format!("tree/nested/ft-1-2{TEMP_SUFFIX}"));
        let fresh = root.join(format!("tree/ft-1-3{TEMP_SUFFIX}"));
        let keeper = root.join("tree/nested/real.txt");
        for path in [&stale_root, &stale_nested, &fresh, &keeper] {
            std::fs::write(path, b"x").unwrap();
        }
        let old = std::time::SystemTime::now() - STALE_TEMP_MAX_AGE - Duration::from_secs(60);
        for path in [&stale_root, &stale_nested] {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(old).unwrap();
        }

        let _registry = registry(&home);

        assert!(!stale_root.exists(), "a stale temp in the root must go");
        assert!(
            !stale_nested.exists(),
            "a stale temp inside an uploaded subdirectory must go too"
        );
        assert!(fresh.exists(), "a temp younger than the age bound stays");
        assert!(keeper.exists(), "a real file is never touched");
    }

    /// The total-bytes cap is per connection. A surface deactivation is a routine UI event (the
    /// user toggles machines), so it must drop the in-flight transfer without handing the
    /// connection a fresh budget; only a real disconnect forgets the budget.
    #[test]
    fn deactivation_drops_the_transfer_but_keeps_the_byte_budget() {
        let home = scratch("budget");
        let mut registry = FileTransferRegistry::new(FileTransferConfig {
            inbox: home.join("inbox"),
            max_file_bytes: 1024,
            max_total_bytes: 6,
            chunk_bytes: 8,
        });
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();

        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry
            .chunk(1, &accepted.transfer_id, 0, b"abcd")
            .unwrap();
        registry.commit(1, &accepted.transfer_id).unwrap();

        // 4 of the 6 allowed bytes are spent. A deactivation must not refund them.
        registry.abort_client_transfer(1);
        let err = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("b.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap_err();
        assert_eq!(
            err.code, "transfer_too_large",
            "a surface deactivation must not reset the connection's byte budget"
        );

        // A real disconnect does forget it.
        registry.forget_client(1);
        registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("c.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
    }

    #[test]
    fn a_transfer_id_from_another_client_is_not_found() {
        let home = scratch("isolation");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        let err = registry
            .chunk(2, &accepted.transfer_id, 0, b"ab")
            .unwrap_err();
        assert_eq!(err.code, "transfer_not_found");
    }

    /// Regression test for the never-overwrite guarantee: a second transfer landing under the
    /// same name after the first has already committed must get a numbered suffix, and the
    /// first file's contents must survive untouched.
    #[test]
    fn a_same_name_transfer_commits_under_a_numbered_suffix_without_clobbering() {
        let home = scratch("collision");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();

        let first_data = b"first upload";
        let first = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("note.txt", &sha256_hex(first_data), first_data.len() as u64),
            )
            .unwrap();
        registry
            .chunk(1, &first.transfer_id, 0, &first_data[..8])
            .unwrap();
        registry
            .chunk(1, &first.transfer_id, 8, &first_data[8..])
            .unwrap();
        let first_committed = registry.commit(1, &first.transfer_id).unwrap();
        assert_eq!(first_committed.path, root.join("note.txt"));

        let second_data = b"second uplo";
        let second = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry(
                    "note.txt",
                    &sha256_hex(second_data),
                    second_data.len() as u64,
                ),
            )
            .unwrap();
        registry
            .chunk(1, &second.transfer_id, 0, &second_data[..8])
            .unwrap();
        registry
            .chunk(1, &second.transfer_id, 8, &second_data[8..])
            .unwrap();
        let second_committed = registry.commit(1, &second.transfer_id).unwrap();

        assert_eq!(second_committed.path, root.join("note-2.txt"));
        assert_eq!(std::fs::read(root.join("note.txt")).unwrap(), first_data);
        assert_eq!(std::fs::read(root.join("note-2.txt")).unwrap(), second_data);
    }

    #[test]
    fn committing_the_same_transfer_twice_is_not_found_the_second_time() {
        let home = scratch("double-commit");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        registry
            .chunk(1, &accepted.transfer_id, 0, b"abcd")
            .unwrap();
        registry.commit(1, &accepted.transfer_id).unwrap();
        let err = registry.commit(1, &accepted.transfer_id).unwrap_err();
        assert_eq!(err.code, "transfer_not_found");
    }

    #[test]
    fn committing_with_no_chunks_on_a_nonzero_entry_is_refused() {
        let home = scratch("no-chunks");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"abcd"), 4),
            )
            .unwrap();
        let err = registry.commit(1, &accepted.transfer_id).unwrap_err();
        assert_eq!(err.code, "transfer_offset_mismatch");
    }

    #[test]
    fn a_chunk_larger_than_the_configured_size_is_refused() {
        let home = scratch("chunk-too-large");
        let mut registry = registry(&home);
        let root = home.join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let accepted = registry
            .begin(
                1,
                &root,
                &home,
                true,
                file_entry("a.txt", &sha256_hex(b"123456789"), 9),
            )
            .unwrap();
        let err = registry
            .chunk(1, &accepted.transfer_id, 0, b"123456789")
            .unwrap_err();
        assert_eq!(err.code, "transfer_chunk_too_large");
    }
}
