use std::io;
use std::path::{Path, PathBuf};

use super::TransferError;

const MAX_COMPONENT_BYTES: usize = 255;
const MAX_COMPONENTS: usize = 32;

const ABSOLUTE_DENYLIST: &[&str] = &["/etc", "/usr", "/bin", "/sbin", "/var"];
const HOME_DENYLIST: &[&str] = &[".ssh", ".config", ".local/bin"];

/// Resolve a root to the destination actually checked and used, so a symlinked root cannot
/// defeat the home-prefix and denylist checks that follow.
///
/// The root itself usually does not exist yet (it is created on first upload), so this walks
/// up to the closest existing ancestor, canonicalizes that ancestor, and rejoins the remaining
/// components lexically; a fully missing root canonicalizes to itself unchanged.
fn canonicalize_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut suffix = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        let Some(name) = current.file_name() else {
            break;
        };
        suffix.push(name.to_owned());
        if let Ok(canonical) = parent.canonicalize() {
            let mut resolved = canonical;
            for component in suffix.into_iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        current = parent;
    }
    path.to_path_buf()
}

pub(crate) fn validate_root(root: &Path, home: &Path) -> Result<PathBuf, TransferError> {
    let refused = |reason: &str| {
        TransferError::new(
            "destination_refused",
            format!("this destination is not allowed: {reason}"),
        )
    };
    if !root.is_absolute() {
        return Err(refused("not an absolute path"));
    }
    // Compare canonicalized coordinates so a symlinked root (or a symlinked ancestor of it,
    // such as macOS's `/home` -> `/System/Volumes/Data/home`) cannot pass the home-prefix and
    // denylist checks by pointing lexically inside the home directory while really resolving
    // outside it. `home` is canonicalized the same way so the comparison stays meaningful.
    let canonical_home = canonicalize_prefix(home);
    let canonical_root = canonicalize_prefix(root);
    if !canonical_root.starts_with(&canonical_home) {
        return Err(refused("outside the home directory"));
    }
    for denied in ABSOLUTE_DENYLIST {
        if starts_with_denylisted(&canonical_root, Path::new(denied)) {
            return Err(refused("system directory"));
        }
    }
    for denied in HOME_DENYLIST {
        if starts_with_denylisted(&canonical_root, &canonical_home.join(denied)) {
            return Err(refused("protected configuration directory"));
        }
    }
    if canonical_root
        .components()
        .any(|component| component_matches_name(component, ".git"))
    {
        return Err(refused("inside a git directory"));
    }
    Ok(root.to_path_buf())
}

// macOS (APFS/HFS+) and Windows (NTFS/ReFS) default to case-insensitive, case-preserving
// filesystems, where `.SSH` and `.ssh` name the same directory regardless of which one (or
// neither) currently exists on disk; `canonicalize_prefix` only case-corrects a component once
// something real exists at that name, so a not-yet-created denylisted directory could otherwise
// be reached under a differently-cased path. Linux filesystems default to case-sensitive, so
// stay byte-exact there.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const CASE_INSENSITIVE_DENYLIST: bool = true;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const CASE_INSENSITIVE_DENYLIST: bool = false;

fn component_matches_name(component: std::path::Component, name: &str) -> bool {
    match component.as_os_str().to_str() {
        Some(text) if CASE_INSENSITIVE_DENYLIST => text.eq_ignore_ascii_case(name),
        Some(text) => text == name,
        None => false,
    }
}

/// Whether `path` begins with `prefix`, matching components case-insensitively on filesystems
/// that treat case as insignificant (see `CASE_INSENSITIVE_DENYLIST`).
fn starts_with_denylisted(path: &Path, prefix: &Path) -> bool {
    if !CASE_INSENSITIVE_DENYLIST {
        return path.starts_with(prefix);
    }
    let mut path_components = path.components();
    prefix.components().all(|prefix_component| {
        path_components.next().is_some_and(|component| {
            match (
                component.as_os_str().to_str(),
                prefix_component.as_os_str().to_str(),
            ) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => component == prefix_component,
            }
        })
    })
}

/// The reason a component is refused, shared by `validate_component` (generic message) and
/// `resolve_home_relative_components` (names the offending component, since the user typed it
/// deliberately and can act on being told which piece was wrong).
fn component_reason(component: &str) -> Result<(), &'static str> {
    if component.is_empty() {
        return Err("empty component");
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err("component is too long");
    }
    if component == "." || component == ".." {
        return Err("relative component");
    }
    if component.starts_with('.') {
        return Err("leading dot");
    }
    if component.contains('/') || component.contains('\\') {
        return Err("path separator inside a component");
    }
    if component.chars().any(char::is_control) {
        return Err("control character");
    }
    Ok(())
}

pub(crate) fn validate_component(component: &str) -> Result<(), TransferError> {
    component_reason(component).map_err(|reason| {
        TransferError::new(
            "invalid_file_path",
            format!("this file name is not allowed: {reason}"),
        )
    })
}

/// Split and validate a client-typed home-relative destination path (`FilePutDestination::HomePath`).
/// This is attacker-influenced input in the same way every other destination is, so it goes
/// through the same per-component checks as `validate_component`, but names the offending
/// component in the refusal since the user typed this path themselves and can fix it.
pub(crate) fn resolve_home_relative_components(typed: &str) -> Result<Vec<String>, TransferError> {
    if typed.is_empty() {
        return Err(TransferError::new(
            "destination_refused",
            "type a folder path under your home directory; the home directory itself is not a valid destination",
        ));
    }
    if typed.starts_with('/') || typed.starts_with('\\') {
        return Err(TransferError::new(
            "invalid_file_path",
            "absolute paths are not accepted",
        ));
    }
    let components: Vec<&str> = typed.split('/').collect();
    if components.len() > MAX_COMPONENTS {
        return Err(TransferError::new(
            "invalid_file_path",
            "this path nests too deeply",
        ));
    }
    let mut resolved = Vec::with_capacity(components.len());
    for component in components {
        if let Err(reason) = component_reason(component) {
            return Err(TransferError::new(
                "invalid_file_path",
                format!("{component:?} is not allowed in this path: {reason}"),
            ));
        }
        resolved.push(component.to_owned());
    }
    Ok(resolved)
}

/// Resolve, validate, and create a client-typed home-relative destination, returning the final
/// directory. Runs the typed path through the same home-containment, denylist, and symlink checks
/// as every other destination (`validate_root`), then creates any missing directory with the same
/// symlink-safe, owner-only creation used for entry subdirectories (`ensure_directory_no_follow`).
pub(crate) fn ensure_home_relative_root(
    typed: &str,
    home: &Path,
) -> Result<PathBuf, TransferError> {
    let components = resolve_home_relative_components(typed)?;
    let candidate = components
        .iter()
        .fold(home.to_path_buf(), |mut path, component| {
            path.push(component);
            path
        });
    validate_root(&candidate, home)?;
    ensure_directory_no_follow(home, &components)
        .map_err(|err| TransferError::from_io("transfer_write_failed", &err))
}

pub(crate) fn resolve_relative_path(
    relative_path: Option<&str>,
    suggested_name: &str,
    allow_subdirectories: bool,
) -> Result<Vec<String>, TransferError> {
    let raw = relative_path.unwrap_or(suggested_name);
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err(TransferError::new(
            "invalid_file_path",
            "absolute paths are not accepted",
        ));
    }
    let components: Vec<String> = raw.split('/').map(str::to_owned).collect();
    if components.len() > MAX_COMPONENTS {
        return Err(TransferError::new(
            "invalid_file_path",
            "this path nests too deeply",
        ));
    }
    if components.len() > 1 && !allow_subdirectories {
        return Err(TransferError::new(
            "invalid_file_path",
            "this destination accepts single files only",
        ));
    }
    for component in &components {
        validate_component(component)?;
    }
    Ok(components)
}

/// Create the destination root and any missing ancestor, owner-only.
///
/// `create_dir_all` would leave the inbox at whatever the umask allows (0755 by default), while
/// the spec promises an owner-only inbox and every component created underneath already gets
/// 0700 from `restrict_dir_permissions`. An existing directory keeps the permissions it has: this
/// is the user's own tree and the root may legitimately be somewhere they already share.
pub(crate) fn create_root_private(root: &Path) -> io::Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent() {
        create_root_private(parent)?;
    }
    match std::fs::create_dir(root) {
        Ok(()) => restrict_dir_permissions(root),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) fn ensure_directory_no_follow(
    root: &Path,
    components: &[String],
) -> io::Result<PathBuf> {
    let mut current = root.to_path_buf();
    for component in components {
        current.push(component);
        create_directory_no_follow(&current)?;
    }
    Ok(current)
}

/// Create `path` as a directory, or accept it if already present, without ever following a
/// symlink. A `symlink_metadata` check followed by a separate create/open call would leave a
/// window between the check and the use; a symlink planted in that window would be followed.
/// Opening with `O_NOFOLLOW | O_DIRECTORY` closes that window by making the kernel refuse a
/// symlinked or non-directory path atomically, so a hostile component is refused rather than
/// followed.
#[cfg(unix)]
fn create_directory_no_follow(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::create_dir(path) {
        Ok(()) => {
            restrict_dir_permissions(path)?;
            return Ok(());
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
        .map(|_| ())
        .map_err(|err| {
            // Only the component name: this message reaches the client, which already knows the
            // relative path it asked for, and the server's absolute layout is not its business.
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("this path");
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} is not a real directory: {err}"),
            )
        })
}

/// This fork does not target Windows (CI is ubuntu + macOS only). The unix branch's fail-closed
/// guarantee relies on `O_NOFOLLOW | O_DIRECTORY`, which has no direct Windows equivalent
/// without extra reparse-point handling; rather than fall back to the disclosed-unsafe
/// symlink_metadata-then-create pattern this refuses outright, so an unreviewed Windows path
/// cannot silently ship with a weaker guarantee than unix.
#[cfg(windows)]
fn create_directory_no_follow(path: &Path) -> io::Result<()> {
    let _ = path;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file upload destinations are not supported on this platform",
    ))
}

pub(crate) fn unique_target(dir: &Path, file_name: &str) -> Result<PathBuf, TransferError> {
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (file_name, None),
    };
    for attempt in 1..=1000u32 {
        let candidate = if attempt == 1 {
            file_name.to_owned()
        } else if let Some(extension) = extension {
            format!("{stem}-{attempt}.{extension}")
        } else {
            format!("{stem}-{attempt}")
        };
        let path = dir.join(&candidate);
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(path);
        }
    }
    Err(TransferError::new(
        "invalid_file_path",
        "could not find a free name for this file",
    ))
}

/// A real directory already at `dir/name`, so a nested tree reuses it instead of getting a
/// numbered sibling for every entry. A symlink is never reused.
pub(crate) fn existing_directory(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    (metadata.is_dir() && !metadata.file_type().is_symlink()).then_some(path)
}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

// Windows has no mode bits to set here. `create_root_private` calls this on every platform, so
// this is a real no-op rather than dead code, and a future Windows implementation has the hook
// ready.
#[cfg(windows)]
fn restrict_dir_permissions(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn root_outside_home_is_refused() {
        let err = validate_root(Path::new("/tmp/elsewhere"), &home()).unwrap_err();
        assert_eq!(err.code, "destination_refused");
    }

    #[test]
    fn denylisted_roots_are_refused() {
        for root in [
            "/etc",
            "/usr/local",
            "/home/tester/.ssh",
            "/home/tester/.config/herdr",
            "/home/tester/.local/bin",
            "/home/tester/repo/.git",
            "/home/tester/repo/.git/hooks",
        ] {
            let err = validate_root(Path::new(root), &home()).unwrap_err();
            assert_eq!(err.code, "destination_refused", "{root} was not refused");
        }
    }

    #[test]
    fn root_inside_home_is_accepted() {
        assert_eq!(
            validate_root(Path::new("/home/tester/herdr-inbox"), &home()).unwrap(),
            PathBuf::from("/home/tester/herdr-inbox")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_root_symlinked_outside_home_is_refused() {
        let base = tempdir();
        let home = base.join("home-fixture");
        std::fs::create_dir(&home).unwrap();
        let outside = base.join("outside-fixture");
        std::fs::create_dir(&outside).unwrap();
        let linked_root = home.join("escape-link");
        std::os::unix::fs::symlink(&outside, &linked_root).unwrap();

        let err = validate_root(&linked_root, &home).unwrap_err();
        assert_eq!(err.code, "destination_refused");
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn case_folded_denylisted_root_is_refused_on_case_insensitive_filesystems() {
        // Empirical finding (see task-1-report.md, finding 3): std::fs::canonicalize on macOS
        // APFS case-corrects a component once something real exists at that name, but a
        // not-yet-created denylisted directory (here `.ssh` never gets created) keeps whatever
        // case the caller supplied, so the denylist comparison itself must fold case.
        let base = tempdir();
        let err = validate_root(&base.join(".SSH"), &base).unwrap_err();
        assert_eq!(err.code, "destination_refused");
    }

    #[test]
    fn hostile_components_are_rejected() {
        for component in [
            "..", ".", "", "a/b", "a\\b", "a\u{0}b", "a\nb", "a\u{7}b", ".hidden",
        ] {
            assert!(
                validate_component(component).is_err(),
                "{component:?} was accepted"
            );
        }
        assert!(validate_component(&"n".repeat(256)).is_err());
        assert!(validate_component("report 2026.txt").is_ok());
    }

    #[test]
    fn absolute_and_escaping_relative_paths_are_rejected() {
        for relative in [
            "/etc/passwd",
            "../outside/x.txt",
            "a/../../x.txt",
            "a//b.txt",
        ] {
            let err = resolve_relative_path(Some(relative), "x.txt", true).unwrap_err();
            assert_eq!(err.code, "invalid_file_path", "{relative} was accepted");
        }
    }

    #[test]
    fn subdirectories_are_only_allowed_when_permitted() {
        assert_eq!(
            resolve_relative_path(Some("docs/report.txt"), "report.txt", true).unwrap(),
            vec!["docs".to_owned(), "report.txt".to_owned()]
        );
        let err = resolve_relative_path(Some("docs/report.txt"), "report.txt", false).unwrap_err();
        assert_eq!(err.code, "invalid_file_path");
    }

    #[test]
    fn suggested_name_is_used_when_no_relative_path_is_given() {
        assert_eq!(
            resolve_relative_path(None, "report.txt", false).unwrap(),
            vec!["report.txt".to_owned()]
        );
    }

    #[test]
    fn collisions_get_a_numbered_suffix() {
        let dir = tempdir();
        std::fs::write(dir.join("report.txt"), b"a").unwrap();
        std::fs::write(dir.join("report-2.txt"), b"a").unwrap();
        assert_eq!(
            unique_target(&dir, "report.txt").unwrap(),
            dir.join("report-3.txt")
        );
    }

    #[test]
    fn an_existing_real_directory_is_reused_and_a_symlinked_one_is_not() {
        let dir = tempdir();
        std::fs::create_dir(dir.join("tree")).unwrap();
        assert_eq!(existing_directory(&dir, "tree"), Some(dir.join("tree")));
        assert_eq!(existing_directory(&dir, "absent"), None);

        std::fs::write(dir.join("file"), b"x").unwrap();
        assert_eq!(existing_directory(&dir, "file"), None);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("tree"), dir.join("linked")).unwrap();
            assert_eq!(existing_directory(&dir, "linked"), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_component_is_refused() {
        let dir = tempdir();
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        let err = ensure_directory_no_follow(&dir, &["link".to_owned()]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // The refusal travels to the client, so it names the component, not the server's layout.
        let message = err.to_string();
        assert!(message.contains("link"), "{message}");
        assert!(
            !message.contains(&dir.display().to_string()),
            "the server's absolute path leaked to the client: {message}"
        );
    }

    #[test]
    fn an_empty_typed_path_is_refused_because_home_itself_is_not_a_valid_target() {
        let err = resolve_home_relative_components("").unwrap_err();
        assert_eq!(err.code, "destination_refused");
    }

    #[test]
    fn a_typed_absolute_path_is_refused() {
        for typed in ["/etc/passwd", "\\Windows\\System32"] {
            let err = resolve_home_relative_components(typed).unwrap_err();
            assert_eq!(err.code, "invalid_file_path", "{typed} was accepted");
        }
    }

    #[test]
    fn a_typed_escaping_relative_path_is_refused_and_names_the_component() {
        let err = resolve_home_relative_components("../../etc").unwrap_err();
        assert_eq!(err.code, "invalid_file_path");
        assert!(err.message.contains(".."), "{}", err.message);
    }

    #[test]
    fn a_typed_leading_dot_component_is_refused_and_names_the_component() {
        for (typed, offending) in [
            (".ssh", ".ssh"),
            ("projects/.config", ".config"),
            (".local/bin", ".local"),
        ] {
            let err = resolve_home_relative_components(typed).unwrap_err();
            assert_eq!(err.code, "invalid_file_path", "{typed} was accepted");
            assert!(
                err.message.contains(offending),
                "{typed} refusal {:?} did not name {offending}",
                err.message
            );
        }
    }

    #[test]
    fn a_valid_typed_path_creates_missing_components_owner_only() {
        let home = tempdir();
        let root = ensure_home_relative_root("projects/demo/assets", &home).unwrap();
        assert_eq!(root, home.join("projects/demo/assets"));
        assert!(root.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for created in [
                home.join("projects"),
                home.join("projects/demo"),
                home.join("projects/demo/assets"),
            ] {
                let mode = std::fs::metadata(&created).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{created:?} was not created owner-only");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_typed_path_through_a_symlinked_component_is_refused() {
        let home = tempdir();
        let outside = tempdir();
        std::os::unix::fs::symlink(&outside, home.join("escape")).unwrap();
        let err = ensure_home_relative_root("escape/payload", &home).unwrap_err();
        assert_eq!(err.code, "destination_refused");
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-destination-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
