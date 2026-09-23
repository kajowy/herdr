//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// kajowy fork identity. The marker itself is the `+kajowy.1` build metadata in the
/// Cargo version, so a rebase onto upstream only has to keep that suffix and this message.
/// Everything that must stay compatible with upstream compares through `upstream_version()`
/// or `version_matches()`, both of which drop the metadata.
const FORK_UPDATE_REFUSAL: &str =
    "self-update is disabled for kajowy fork builds; update by rebuilding from kajowy/herdr master";

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn version() -> String {
    match channel() {
        "stable" => BASE_VERSION.to_string(),
        channel => match build_id() {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

/// Version without fork build metadata: the identity upstream releases, manifests and peers use.
pub fn upstream_version() -> String {
    strip_build_metadata(&version()).to_string()
}

/// Compares two herdr versions ignoring semver build metadata.
#[cfg(not(windows))]
pub fn version_matches(left: &str, right: &str) -> bool {
    strip_build_metadata(left) == strip_build_metadata(right)
}

fn strip_build_metadata(version: &str) -> &str {
    match version.split_once('+') {
        Some((base, _)) => base,
        None => version,
    }
}

/// Reason this build must not replace itself with an upstream herdr.dev release.
pub fn fork_update_refusal() -> Option<&'static str> {
    Some(FORK_UPDATE_REFUSAL)
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }

    #[test]
    fn version_carries_the_fork_marker() {
        assert!(
            super::version().ends_with("+kajowy.1"),
            "version was {}",
            super::version()
        );
    }

    #[test]
    fn upstream_version_drops_the_fork_marker() {
        assert_eq!(
            super::upstream_version(),
            super::version().split('+').next().expect("base version")
        );
        assert!(!super::upstream_version().contains('+'));
    }

    #[test]
    #[cfg(not(windows))]
    fn version_matches_ignores_build_metadata() {
        assert!(super::version_matches("0.9.1+kajowy.1", "0.9.1"));
        assert!(super::version_matches("0.9.1", "0.9.1+kajowy.1"));
        assert!(!super::version_matches("0.9.2", "0.9.1+kajowy.1"));
    }
}
