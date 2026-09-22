//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// kajowy fork identity. Rebases onto upstream only need to touch these two constants.
/// The marker stays out of `version()`, which protocol, handoff and remote install compare
/// against upstream release versions.
const FORK_BUILD: &str = "kajowy.1";
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

/// Human-facing version carrying the fork marker as semver build metadata.
pub fn display_version() -> String {
    format!("{}+{FORK_BUILD}", version())
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
    fn display_version_marks_the_fork_build() {
        assert_eq!(
            super::display_version(),
            format!("{}+kajowy.1", super::version())
        );
    }

    #[test]
    fn compatibility_version_stays_free_of_fork_marker() {
        assert!(!super::version().contains("kajowy"));
    }
}
