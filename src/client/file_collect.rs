//! Walks a client-side file/directory selection into the flat entry list the upload driver
//! sends over `file.put.*`, and reads the bytes for one transfer at a time.

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

const HASH_BUFFER_BYTES: usize = 64 * 1024;
/// Bound on how deep a picked directory is walked. `resolve_relative_path` refuses more than 32
/// components anyway, so anything deeper could never be sent; the bound also stops a pathological
/// tree from recursing without limit.
const MAX_WALK_DEPTH: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct CollectedEntry {
    /// Empty for a directory entry: there is nothing local to read.
    pub(crate) path: PathBuf,
    /// `None` for a single picked file; `Some` for anything under a picked directory.
    pub(crate) relative_path: Option<String>,
    pub(crate) kind: crate::api::schema::FilePutEntryKind,
    pub(crate) bytes: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Collection {
    pub(crate) entries: Vec<CollectedEntry>,
    pub(crate) skipped: Vec<String>,
}

/// Walk the selection into entries, parent directories first.
///
/// A directory becomes its own entry rather than a marker file inside it, so an empty
/// directory arrives empty and nothing is left behind in the user's tree.
pub(crate) fn collect(selection: &[PathBuf]) -> Collection {
    let mut collection = Collection::default();
    for entry in selection {
        let Ok(metadata) = fs::symlink_metadata(entry) else {
            collection
                .skipped
                .push(format!("{}: unreadable", entry.display()));
            continue;
        };
        if metadata.file_type().is_symlink() {
            collection
                .skipped
                .push(format!("{}: symlink", entry.display()));
            continue;
        }
        if metadata.is_file() {
            if leading_dot(entry.file_name().and_then(|name| name.to_str())) {
                collection.skipped.push(hidden_note(entry));
                continue;
            }
            collection.entries.push(CollectedEntry {
                path: entry.clone(),
                relative_path: None,
                kind: crate::api::schema::FilePutEntryKind::File,
                bytes: metadata.len(),
            });
            continue;
        }
        if metadata.is_dir() {
            let Some(base) = entry.file_name().and_then(|name| name.to_str()) else {
                collection
                    .skipped
                    .push(format!("{}: unsupported directory name", entry.display()));
                continue;
            };
            if leading_dot(Some(base)) {
                collection.skipped.push(hidden_note(entry));
                continue;
            }
            collection.entries.push(directory_entry(base));
            walk(entry, base, 1, &mut collection);
            continue;
        }
        collection
            .skipped
            .push(format!("{}: not a regular file", entry.display()));
    }
    collection
}

fn directory_entry(relative: &str) -> CollectedEntry {
    CollectedEntry {
        path: PathBuf::new(),
        relative_path: Some(relative.to_owned()),
        kind: crate::api::schema::FilePutEntryKind::Directory,
        bytes: 0,
    }
}

/// Whether a name is one the server's component validator refuses for its leading dot.
fn leading_dot(name: Option<&str>) -> bool {
    name.is_some_and(|name| name.starts_with('.'))
}

/// The server refuses every leading-dot component (`destination::validate_component`), so a
/// `.DS_Store` beside the picked files, or a `.git` inside a picked repository, is reported here
/// rather than sent and refused one `begin` at a time.
fn hidden_note(path: &Path) -> String {
    format!("{}: hidden name, not sent", path.display())
}

fn walk(dir: &Path, prefix: &str, depth: usize, collection: &mut Collection) {
    if depth > MAX_WALK_DEPTH {
        collection.skipped.push(format!(
            "{}: nested deeper than {MAX_WALK_DEPTH} directories",
            dir.display()
        ));
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        collection
            .skipped
            .push(format!("{}: unreadable", dir.display()));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            collection
                .skipped
                .push(format!("{}: unsupported name", path.display()));
            continue;
        };
        if leading_dot(Some(&name)) {
            collection.skipped.push(hidden_note(&path));
            continue;
        }
        let relative = format!("{prefix}/{name}");
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            collection
                .skipped
                .push(format!("{}: unreadable", path.display()));
            continue;
        };
        if metadata.file_type().is_symlink() {
            collection
                .skipped
                .push(format!("{}: symlink", path.display()));
            continue;
        }
        if metadata.is_dir() {
            // Emit the directory before descending, so the server has it before its contents.
            collection.entries.push(directory_entry(&relative));
            walk(&path, &relative, depth.saturating_add(1), collection);
            continue;
        }
        if !metadata.is_file() {
            collection
                .skipped
                .push(format!("{}: not a regular file", path.display()));
            continue;
        }
        collection.entries.push(CollectedEntry {
            path,
            relative_path: Some(relative),
            kind: crate::api::schema::FilePutEntryKind::File,
            bytes: metadata.len(),
        });
    }
}

/// Stream one file's sha256 at send time, so a file changed during the walk fails its own
/// transfer instead of poisoning the manifest. Never called for a directory entry.
pub(crate) fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total = total.saturating_add(read as u64);
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

pub(crate) fn read_chunk(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        let read = file.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buffer.truncate(filled);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-collect-{label}-{}-{}",
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
    fn a_single_file_has_no_relative_path() {
        let dir = scratch("single");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"abc").unwrap();
        let collection = collect(std::slice::from_ref(&file));
        assert_eq!(collection.entries.len(), 1);
        assert_eq!(collection.entries[0].relative_path, None);
        assert_eq!(collection.entries[0].bytes, 3);
        assert_eq!(
            collection.entries[0].kind,
            crate::api::schema::FilePutEntryKind::File
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_walk_emits_parents_first_and_skips_symlinks() {
        let dir = scratch("walk");
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::create_dir_all(tree.join("empty")).unwrap();
        std::fs::write(tree.join("nested/b.txt"), b"bb").unwrap();
        std::os::unix::fs::symlink(tree.join("nested/b.txt"), tree.join("link.txt")).unwrap();

        let collection = collect(std::slice::from_ref(&tree));
        let listed: Vec<(String, crate::api::schema::FilePutEntryKind)> = collection
            .entries
            .iter()
            .filter_map(|entry| {
                entry
                    .relative_path
                    .clone()
                    .map(|relative| (relative, entry.kind))
            })
            .collect();

        use crate::api::schema::FilePutEntryKind::{Directory, File};
        // Every directory precedes anything under it, so the server never writes into a
        // directory it has not been asked to create yet.
        assert_eq!(listed.first(), Some(&("tree".to_owned(), Directory)));
        let position = |needle: &str| {
            listed
                .iter()
                .position(|(relative, _)| relative == needle)
                .unwrap_or_else(|| panic!("{needle} missing from {listed:?}"))
        };
        assert!(position("tree/nested") < position("tree/nested/b.txt"));
        assert_eq!(listed[position("tree/empty")].1, Directory);
        assert_eq!(listed[position("tree/nested/b.txt")].1, File);
        // The empty directory is represented by the directory entry alone: no marker file.
        assert!(
            !listed
                .iter()
                .any(|(relative, _)| relative.starts_with("tree/empty/")),
            "an empty directory must not gain contents: {listed:?}"
        );
        assert!(collection
            .skipped
            .iter()
            .any(|entry| entry.contains("link.txt")));
    }

    #[test]
    fn leading_dot_names_are_skipped_rather_than_sent() {
        // Any Finder-browsed folder has `.DS_Store` and any repository has `.git/`. The server
        // refuses every leading-dot component, so sending them would abort the selection partway.
        let dir = scratch("hidden");
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        std::fs::write(tree.join(".git/config"), b"[core]").unwrap();
        std::fs::write(tree.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(tree.join("keep.txt"), b"keep").unwrap();
        let hidden_pick = dir.join(".hidden-pick");
        std::fs::write(&hidden_pick, b"nope").unwrap();

        let collection = collect(&[tree.clone(), hidden_pick]);
        let listed: Vec<String> = collection
            .entries
            .iter()
            .filter_map(|entry| entry.relative_path.clone())
            .collect();
        assert_eq!(listed, vec!["tree".to_owned(), "tree/keep.txt".to_owned()]);
        for hidden in [".git", ".DS_Store", ".hidden-pick"] {
            assert!(
                collection
                    .skipped
                    .iter()
                    .any(|entry| entry.contains(hidden)),
                "{hidden} missing from {:?}",
                collection.skipped
            );
        }
    }

    #[test]
    fn a_tree_deeper_than_the_walk_bound_is_reported_and_not_descended() {
        let dir = scratch("deep");
        let mut deep = dir.join("tree");
        for index in 0..(MAX_WALK_DEPTH + 2) {
            deep = deep.join(format!("d{index}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("too-deep.txt"), b"x").unwrap();

        let collection = collect(&[dir.join("tree")]);
        assert!(
            !collection
                .entries
                .iter()
                .any(|entry| entry.path.ends_with("too-deep.txt")),
            "the walk must stop at the depth bound"
        );
        assert!(collection
            .skipped
            .iter()
            .any(|entry| entry.contains("nested deeper than")));
    }

    #[test]
    fn hash_and_chunk_read_agree_with_the_file() {
        let dir = scratch("hash");
        let file = dir.join("c.bin");
        std::fs::write(&file, b"0123456789").unwrap();
        let (digest, bytes) = hash_file(&file).unwrap();
        assert_eq!(bytes, 10);
        assert_eq!(digest.len(), 64);
        assert_eq!(read_chunk(&file, 4, 3).unwrap(), b"456".to_vec());
        assert_eq!(read_chunk(&file, 8, 8).unwrap(), b"89".to_vec());
    }
}
