use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest as _, Sha256};

pub(crate) mod destination;

#[allow(dead_code)] // wired to the upload request handler in a later task
const TEMP_SUFFIX: &str = ".herdr-part";
#[allow(dead_code)] // wired to the upload request handler in a later task
const STALE_TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

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

    #[allow(dead_code)] // wired to the upload request handler in a later task
    pub(crate) fn from_io(code: &'static str, err: &io::Error) -> Self {
        Self::new(code, err.to_string())
    }
}

#[allow(dead_code)] // wired to the upload request handler in a later task
pub(crate) struct FileTransferConfig {
    pub(crate) inbox: PathBuf,
    pub(crate) max_file_bytes: u64,
    pub(crate) max_total_bytes: u64,
    pub(crate) chunk_bytes: u32,
}

/// One entry a `file.put.begin` asks for. Grouping the wire fields keeps `begin` readable.
#[allow(dead_code)] // wired to the upload request handler in a later task
pub(crate) struct BeginEntry<'a> {
    pub(crate) suggested_name: &'a str,
    pub(crate) relative_path: Option<&'a str>,
    pub(crate) kind: crate::api::schema::FilePutEntryKind,
    pub(crate) bytes: u64,
    pub(crate) sha256: &'a str,
}

#[allow(dead_code)] // wired to the upload request handler in a later task
struct ActiveTransfer {
    transfer_id: String,
    temp_path: PathBuf,
    final_path: PathBuf,
    file: fs::File,
    expected_bytes: u64,
    written_bytes: u64,
    expected_sha256: String,
    hasher: Sha256,
}

#[allow(dead_code)] // wired to the upload request handler in a later task
pub(crate) struct FileTransferRegistry {
    config: FileTransferConfig,
    active: HashMap<u64, ActiveTransfer>,
    session_bytes: HashMap<u64, u64>,
    next_transfer_serial: u64,
}

#[derive(Debug)]
#[allow(dead_code)] // wired to the upload request handler in a later task
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
#[allow(dead_code)] // wired to the upload request handler in a later task
pub(crate) struct CommittedFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

#[allow(dead_code)] // wired to the upload request handler in a later task
impl FileTransferRegistry {
    pub(crate) fn new(config: FileTransferConfig) -> Self {
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
        fs::create_dir_all(&root)
            .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?;
        reap_stale_temp_files(&root);

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

        let final_path = destination::unique_target(&dir, &leaf_name)?;
        let temp_path = dir.join(format!("{transfer_id}{TEMP_SUFFIX}"));
        let file = create_private_new(&temp_path)
            .map_err(|err| TransferError::from_io("transfer_write_failed", &err))?;

        self.active.insert(
            client_id,
            ActiveTransfer {
                transfer_id: transfer_id.clone(),
                temp_path,
                final_path,
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
        if let Err(err) = fs::rename(&transfer.temp_path, &transfer.final_path) {
            let _ = fs::remove_file(&transfer.temp_path);
            return Err(TransferError::from_io("transfer_write_failed", &err));
        }
        *self.session_bytes.entry(client_id).or_default() += transfer.written_bytes;
        Ok(CommittedFile {
            path: transfer.final_path,
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

    pub(crate) fn abort_client(&mut self, client_id: u64) {
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

#[allow(dead_code)] // wired to the upload request handler in a later task
fn not_found() -> TransferError {
    TransferError::new("transfer_not_found", "this transfer is no longer active")
}

#[allow(dead_code)] // wired to the upload request handler in a later task
fn create_private_new(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    restrict_file_options(&mut options);
    options.open(path)
}

#[allow(dead_code)] // called by create_private_new, wired in a later task
#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
    options.custom_flags(libc::O_NOFOLLOW);
}

#[allow(dead_code)] // called by create_private_new, wired in a later task
#[cfg(windows)]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

#[allow(dead_code)] // wired to the upload request handler in a later task
fn reap_stale_temp_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().ends_with(TEMP_SUFFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().unwrap_or_default() > STALE_TEMP_MAX_AGE {
            let _ = fs::remove_file(entry.path());
        }
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
        registry.abort_client(2);
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().count(), 0);
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
}
