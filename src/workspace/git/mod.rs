mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod pr;
mod status;
#[cfg(test)]
mod test_support;

pub use self::{
    discovery::{derive_label_from_cwd, git_branch, git_space_metadata, GitSpaceMetadata},
    status::{git_status_cache_key, git_status_snapshot_for_cwd, GitStatusCacheEntry},
};

pub(crate) use self::pr::{poll_pr_for_cwd, PrCacheEntry, PrPollOutcome};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
