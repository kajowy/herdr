use serde::{Deserialize, Serialize};

/// Destination kind a client may name. The client never names an absolute path; `HomePath`
/// carries a client-typed path in `FilePutBeginParams::home_path`, which the server resolves
/// relative to the receiving machine's home directory and validates the same way as every other
/// destination.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum FilePutDestination {
    /// The server-configured inbox directory. The only destination that accepts subdirectories.
    Inbox,
    /// The named pane's launch working directory, resolved once at begin.
    #[default]
    PaneCwd,
    /// A client-typed path, resolved relative to the receiving machine's home directory.
    HomePath,
}

/// What one `file.put.begin` creates. A directory entry carries no bytes and finishes at begin,
/// which is how an empty directory survives without a marker file in the user's tree.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum FilePutEntryKind {
    #[default]
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilePutBeginParams {
    pub suggested_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub entry_kind: FilePutEntryKind,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub destination: FilePutDestination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The path typed by the client for `FilePutDestination::HomePath`, relative to the
    /// receiving machine's home directory. Ignored for every other destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilePutChunkParams {
    pub transfer_id: String,
    pub offset: u64,
    pub data_b64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilePutCommitParams {
    pub transfer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilePutAbortParams {
    pub transfer_id: String,
}
