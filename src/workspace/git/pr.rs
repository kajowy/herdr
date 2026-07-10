//! GitHub PR status polling via `gh`, run off the UI thread.
//!
//! Mirrors the git-status refresh pipeline in `src/app/runtime.rs`: a
//! background worker calls [`poll_pr_for_cwd`] per unique (cwd, branch) and
//! the main loop applies the guarded result.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GH_PR_VIEW_TIMEOUT: Duration = Duration::from_secs(5);
const GH_PR_VIEW_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Outcome of checking a repo's PR status via `gh pr view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrPollOutcome {
    /// An open or merged PR was found for the current branch.
    Found { number: u32, merged: bool },
    /// No active PR (closed, or none exists).
    None,
    /// Could not determine PR status (`gh` missing, spawn failed, or timed out).
    Unavailable,
}

/// A cached poll result, timestamped so callers can apply a TTL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrCacheEntry {
    pub(crate) polled_at: Instant,
    pub(crate) outcome: PrPollOutcome,
}

/// Parse `gh pr view --json number,state` stdout into a [`PrPollOutcome`].
pub(crate) fn parse_gh_pr_json(stdout: &str) -> PrPollOutcome {
    #[derive(serde::Deserialize)]
    struct GhPrView {
        number: u32,
        state: String,
    }

    let Ok(view) = serde_json::from_str::<GhPrView>(stdout) else {
        return PrPollOutcome::None;
    };

    match view.state.as_str() {
        "OPEN" => PrPollOutcome::Found {
            number: view.number,
            merged: false,
        },
        "MERGED" => PrPollOutcome::Found {
            number: view.number,
            merged: true,
        },
        _ => PrPollOutcome::None,
    }
}

/// Run `gh pr view` in `cwd` and report its outcome.
///
/// Blocks the calling thread for up to [`GH_PR_VIEW_TIMEOUT`] — callers MUST
/// invoke this from a spawned background thread, never the UI thread, since
/// `gh` hits the network. `gh` has no `-C` flag, so the working directory is
/// set via `current_dir` instead.
pub(crate) fn poll_pr_for_cwd(cwd: &Path) -> PrPollOutcome {
    let mut child = match Command::new("gh")
        .args(["pr", "view", "--json", "number,state"])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return PrPollOutcome::Unavailable,
    };

    let deadline = Instant::now() + GH_PR_VIEW_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(GH_PR_VIEW_POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };

    let Some(status) = status else {
        return PrPollOutcome::Unavailable;
    };

    if !status.success() {
        return PrPollOutcome::None;
    }

    let mut stdout = String::new();
    let Some(mut out) = child.stdout.take() else {
        return PrPollOutcome::None;
    };
    if out.read_to_string(&mut stdout).is_err() {
        return PrPollOutcome::None;
    }

    parse_gh_pr_json(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gh_pr_json_open_is_found_unmerged() {
        assert_eq!(
            parse_gh_pr_json(r#"{"number":42,"state":"OPEN"}"#),
            PrPollOutcome::Found {
                number: 42,
                merged: false
            }
        );
    }

    #[test]
    fn parse_gh_pr_json_merged_is_found_merged() {
        assert_eq!(
            parse_gh_pr_json(r#"{"number":42,"state":"MERGED"}"#),
            PrPollOutcome::Found {
                number: 42,
                merged: true
            }
        );
    }

    #[test]
    fn parse_gh_pr_json_closed_is_none() {
        assert_eq!(
            parse_gh_pr_json(r#"{"number":42,"state":"CLOSED"}"#),
            PrPollOutcome::None
        );
    }

    #[test]
    fn parse_gh_pr_json_malformed_is_none() {
        assert_eq!(parse_gh_pr_json("not json"), PrPollOutcome::None);
        assert_eq!(parse_gh_pr_json(""), PrPollOutcome::None);
        assert_eq!(
            parse_gh_pr_json(r#"{"number":"nope"}"#),
            PrPollOutcome::None
        );
    }
}
