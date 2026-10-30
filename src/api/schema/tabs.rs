use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TabListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabRenameParams {
    pub tab_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabMoveParams {
    pub tab_id: String,
    pub insert_index: usize,
}

/// Move a whole tab to another workspace on the same server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabRelocateParams {
    /// Tab to relocate, in the workspace it currently lives in.
    pub tab_id: String,
    /// Workspace the tab moves to. It must differ from the tab's current
    /// workspace; reordering inside one workspace stays `tab.move`.
    pub workspace_id: String,
    /// Position among the target workspace's tabs, clamped to the end. Omit to
    /// append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_index: Option<usize>,
    /// Switch the view to the moved tab in its new workspace.
    #[serde(default)]
    pub focus: bool,
    /// Proceed although the relocation empties, and therefore closes, the
    /// workspace the tab leaves behind. Without it such a relocation is
    /// refused with `confirmation_required` while `ui.confirm_close` is on.
    #[serde(default)]
    pub confirm_source_close: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_status: AgentStatus,
}
