use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateParams, TabListParams,
    TabMoveParams, TabRelocateParams, TabRenameParams, TabTarget,
};
use crate::app::actions::{TabRelocateError, TabRelocation};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(ws_idx))
        });
        let (rows, cols) = self.state.new_pane_size(crate::ui::NewPanePlacement::Alone);
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let extra_env = match super::env::normalize_launch_env(env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            });
        match result {
            Ok((tab_idx, terminal, runtime)) => {
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
                        crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
                    });
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.state.mode = Mode::Terminal;
                }
                self.schedule_session_save();
                self.emit_tab_created_events(ws_idx, tab_idx);
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
            crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
        });
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > ws.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&ws.id, tab_idx + 1));
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info(ws_idx);
        if moved {
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_relocate(&mut self, id: String, params: TabRelocateParams) -> String {
        let TabRelocateParams {
            tab_id,
            workspace_id,
            insert_index,
            focus,
            confirm_source_close,
        } = params;
        let Some((source_ws_idx, source_tab_idx)) = self.parse_tab_id(&tab_id) else {
            return tab_not_found(id, &tab_id);
        };
        let Some(target_ws_idx) = self.parse_workspace_id(&workspace_id) else {
            return workspace_not_found(id, &workspace_id);
        };
        // Relocating the last tab out of a workspace closes it, so it has to
        // clear the same guards `tab.close` clears before closing one. Unlike a
        // close it removes only that workspace, so the owner's confirmation lets
        // it through and the rest of the worktree group keeps running.
        let closes_source_workspace = source_ws_idx != target_ws_idx
            && self
                .state
                .workspaces
                .get(source_ws_idx)
                .is_some_and(|ws| ws.tabs.len() <= 1);
        if closes_source_workspace {
            if let Err(response) = self.require_restored_group_close_ready(
                &id,
                &self.state.workspace_close_indices(source_ws_idx),
            ) {
                return response;
            }
            if !confirm_source_close
                && self
                    .state
                    .confirm_implicit_worktree_group_close(source_ws_idx)
            {
                return encode_error(
                    id,
                    "confirmation_required",
                    "relocating this tab closes the workspace it leaves behind",
                );
            }
        }

        let relocation = match self.state.relocate_tab(
            source_ws_idx,
            source_tab_idx,
            target_ws_idx,
            insert_index,
        ) {
            Ok(relocation) => relocation,
            Err(TabRelocateError::TabNotFound) => return tab_not_found(id, &tab_id),
            Err(TabRelocateError::TargetWorkspaceNotFound) => {
                return workspace_not_found(id, &workspace_id)
            }
            Err(TabRelocateError::SameWorkspace) => {
                return encode_error(
                    id,
                    "tab_already_in_workspace",
                    format!("tab {tab_id} already belongs to workspace {workspace_id}"),
                )
            }
        };
        // The relocation result carries the only indices corrected for a closed
        // source workspace; the ones resolved before the call may have shifted.
        let TabRelocation {
            target_workspace_idx,
            target_tab_idx,
            closed_source_workspace_id,
            ..
        } = relocation;
        if focus {
            self.state
                .switch_workspace_tab(target_workspace_idx, target_tab_idx);
        }
        self.schedule_session_save();

        let Some(tab) = self.tab_info(target_workspace_idx, target_tab_idx) else {
            return encode_error(id, "tab_relocate_failed", "relocated tab is unavailable");
        };
        let tabs = self.tab_list_info(target_workspace_idx);
        self.emit_event(EventEnvelope {
            event: EventKind::TabMoved,
            data: EventData::TabMoved {
                tab_id: tab.tab_id.clone(),
                workspace_id: tab.workspace_id.clone(),
                insert_index: target_tab_idx,
                tabs: tabs.clone(),
            },
        });
        if let Some(closed_workspace_id) = &closed_source_workspace_id {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id: closed_workspace_id.clone(),
                    workspace: None,
                },
            });
        }

        encode_success(
            id,
            ResponseResult::TabRelocated {
                tab,
                tabs,
                insert_index: target_tab_idx,
                closed_workspace_id: closed_source_workspace_id,
            },
        )
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let closes_workspace = ws.tabs.len() <= 1;
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids = ws
            .tabs
            .get(tab_idx)
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default();

        if closes_workspace {
            if let Err(response) = self.require_restored_group_close_ready(
                &id,
                &self.state.workspace_close_indices(ws_idx),
            ) {
                return response;
            }
            if self.state.confirm_implicit_worktree_group_close(ws_idx) {
                return encode_error(
                    id,
                    "confirmation_required",
                    "closing this tab would close a worktree group",
                );
            }
            let workspace = self.workspace_info(ws_idx);
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.state.remove_plugin_pane_records(pane_ids);
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace),
                },
            });
            return encode_success(id, ResponseResult::Ok {});
        }

        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        if !ws.close_tab(tab_idx) {
            return encode_error(
                id,
                "tab_close_failed",
                format!("tab {} could not be closed", target.tab_id),
            );
        }
        self.state.remove_plugin_pane_records(pane_ids);
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id,
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                (0..ws.tabs.len())
                    .filter_map(|idx| self.tab_info(ws_idx, idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, SuccessResponse},
        config::{Config, ShellModeConfig},
        workspace::Workspace,
    };

    #[test]
    fn api_tab_close_last_tab_closes_workspace_and_emits_both_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.active.is_none());
        let events = event_hub.events_after(0);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            [EventKind::TabClosed, EventKind::WorkspaceClosed]
        );
        assert!(matches!(
            &events[0].1.data,
            EventData::TabClosed {
                tab_id: closed_tab_id,
                workspace_id: closed_workspace_id,
            } if closed_tab_id == &tab_id && closed_workspace_id == &workspace_id
        ));
        assert!(matches!(
            &events[1].1.data,
            EventData::WorkspaceClosed {
                workspace_id: closed_workspace_id,
                workspace: Some(workspace),
            } if closed_workspace_id == &workspace_id
                && workspace.workspace_id == workspace_id
        ));
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_root = app.state.workspaces[0].tabs[0].root_pane;
        let moved_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(tabs[2].tab_id, app.public_tab_id(0, 2).unwrap());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let workspace = Workspace::test_new("tabs");
        let focused_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        let created = &app.state.workspaces[0].tabs[1];
        let created_terminal_id = created.terminal_id(created.root_pane).unwrap();
        let created_cwd = &app.state.terminals.get(created_terminal_id).unwrap().cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    fn relocate_test_app(event_hub: &crate::api::EventHub) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        )
    }

    /// Two workspaces where the source holds a second, split tab to relocate.
    fn relocate_fixture(event_hub: &crate::api::EventHub) -> App {
        let mut app = relocate_test_app(event_hub);
        let mut source = Workspace::test_new("source");
        let moved_idx = source.test_add_tab(Some("work"));
        source.switch_tab(moved_idx);
        source.test_split(ratatui::layout::Direction::Horizontal);
        source.switch_tab(0);
        let mut target = Workspace::test_new("target");
        target.test_add_tab(Some("other"));
        app.state.workspaces = vec![source, target];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    fn relocate_params(tab_id: &str, workspace_id: &str) -> TabRelocateParams {
        TabRelocateParams {
            tab_id: tab_id.into(),
            workspace_id: workspace_id.into(),
            insert_index: None,
            focus: false,
            confirm_source_close: false,
        }
    }

    #[test]
    fn api_tab_relocate_moves_the_tab_and_its_panes_to_the_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let moved_panes = app.state.workspaces[0].tabs[1].layout.pane_ids();
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let target_workspace_id = app.public_workspace_id(1);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &target_workspace_id));

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated {
            tab,
            tabs,
            insert_index,
            closed_workspace_id,
        } = success.result
        else {
            panic!("expected a relocated tab");
        };
        assert_eq!(tab.workspace_id, target_workspace_id);
        assert_eq!(insert_index, 2);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[2].tab_id, tab.tab_id);
        assert!(closed_workspace_id.is_none());
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[1].tabs.len(), 3);
        assert_eq!(
            app.state.workspaces[1].tabs[2].layout.pane_ids(),
            moved_panes
        );
        assert_eq!(tab.pane_count, moved_panes.len());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            &event.data,
            EventData::TabMoved {
                tab_id: moved_tab_id,
                workspace_id,
                insert_index: 2,
                tabs,
            } if moved_tab_id == &tab.tab_id
                && workspace_id == &target_workspace_id
                && tabs.len() == 3
        )));
    }

    #[test]
    fn api_tab_relocate_follows_the_tab_when_focus_is_requested() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let target_workspace_id = app.public_workspace_id(1);

        let response = app.handle_tab_relocate(
            "req".into(),
            TabRelocateParams {
                focus: true,
                ..relocate_params(&tab_id, &target_workspace_id)
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated { tab, .. } = success.result else {
            panic!("expected a relocated tab");
        };
        assert!(tab.focused);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab, 2);
    }

    #[test]
    fn api_tab_relocate_leaves_the_view_alone_without_focus() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let target_workspace_id = app.public_workspace_id(1);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &target_workspace_id));

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated { tab, .. } = success.result else {
            panic!("expected a relocated tab");
        };
        assert!(!tab.focused);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[1].active_tab, 0);
    }

    #[test]
    fn api_tab_relocate_inserts_at_the_requested_index() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let moved_root = app.state.workspaces[0].tabs[1].root_pane;
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let target_workspace_id = app.public_workspace_id(1);

        let response = app.handle_tab_relocate(
            "req".into(),
            TabRelocateParams {
                insert_index: Some(1),
                ..relocate_params(&tab_id, &target_workspace_id)
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated { insert_index, .. } = success.result else {
            panic!("expected a relocated tab");
        };
        assert_eq!(insert_index, 1);
        assert_eq!(app.state.workspaces[1].tabs[1].root_pane, moved_root);
    }

    #[test]
    fn api_tab_relocate_clamps_an_out_of_range_insert_index() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let moved_root = app.state.workspaces[0].tabs[1].root_pane;
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let target_workspace_id = app.public_workspace_id(1);

        let response = app.handle_tab_relocate(
            "req".into(),
            TabRelocateParams {
                insert_index: Some(99),
                ..relocate_params(&tab_id, &target_workspace_id)
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated { insert_index, .. } = success.result else {
            panic!("expected a relocated tab");
        };
        assert_eq!(insert_index, 2);
        assert_eq!(app.state.workspaces[1].tabs[2].root_pane, moved_root);
    }

    #[test]
    fn api_tab_relocate_rejects_an_unknown_tab() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let target_workspace_id = app.public_workspace_id(1);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params("w9:t9", &target_workspace_id));

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "tab_not_found");
    }

    #[test]
    fn api_tab_relocate_rejects_an_unknown_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 1).unwrap();

        let response = app.handle_tab_relocate("req".into(), relocate_params(&tab_id, "w9"));

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "workspace_not_found");
    }

    #[test]
    fn api_tab_relocate_rejects_the_tabs_current_workspace() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let source_workspace_id = app.public_workspace_id(0);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &source_workspace_id));

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "tab_already_in_workspace");
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
    }

    #[test]
    fn api_tab_relocate_reports_a_closed_source_workspace() {
        let event_hub = crate::api::EventHub::default();
        let mut app = relocate_test_app(&event_hub);
        app.state.workspaces = vec![Workspace::test_new("source"), Workspace::test_new("target")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let source_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &target_workspace_id));

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated {
            closed_workspace_id,
            insert_index,
            ..
        } = success.result
        else {
            panic!("expected a relocated tab");
        };
        assert_eq!(
            closed_workspace_id.as_deref(),
            Some(source_workspace_id.as_str())
        );
        assert_eq!(insert_index, 1);
        assert_eq!(app.state.workspaces.len(), 1);
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            &event.data,
            EventData::WorkspaceClosed {
                workspace_id,
                workspace: None,
            } if workspace_id == &source_workspace_id
        )));
    }

    fn worktree_space(
        checkout_path: &str,
        is_linked_worktree: bool,
    ) -> crate::workspace::WorktreeSpaceMembership {
        crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: checkout_path.into(),
            is_linked_worktree,
        }
    }

    /// A worktree group whose parent holds a single tab, next to an unrelated
    /// workspace the tab can move to.
    fn worktree_group_fixture(event_hub: &crate::api::EventHub) -> App {
        let mut app = relocate_test_app(event_hub);
        let mut parent = Workspace::test_new("parent");
        parent.worktree_space = Some(worktree_space("/repo/herdr", false));
        let mut child = Workspace::test_new("child");
        child.worktree_space = Some(worktree_space("/repo/herdr-child", true));
        app.state.workspaces = vec![parent, child, Workspace::test_new("target")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    #[test]
    fn api_tab_relocate_requires_confirmation_before_closing_a_worktree_group() {
        let event_hub = crate::api::EventHub::default();
        let mut app = worktree_group_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let target_workspace_id = app.public_workspace_id(2);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &target_workspace_id));

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "confirmation_required");
        assert_eq!(app.state.workspaces.len(), 3);
        assert!(app.state.workspaces.iter().all(|ws| ws.tabs.len() == 1));
    }

    #[test]
    fn api_tab_relocate_completes_once_the_owner_confirms() {
        let event_hub = crate::api::EventHub::default();
        let mut app = worktree_group_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let source_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(2);

        let response = app.handle_tab_relocate(
            "req".into(),
            TabRelocateParams {
                confirm_source_close: true,
                ..relocate_params(&tab_id, &target_workspace_id)
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabRelocated {
            tab,
            tabs,
            closed_workspace_id,
            ..
        } = success.result
        else {
            panic!("expected a relocated tab");
        };
        assert_eq!(tab.workspace_id, target_workspace_id);
        assert_eq!(tabs.len(), 2);
        assert_eq!(
            closed_workspace_id.as_deref(),
            Some(source_workspace_id.as_str())
        );
        assert!(!app
            .state
            .workspaces
            .iter()
            .any(|ws| ws.id == source_workspace_id));
    }

    /// The relocation removes the emptied source workspace alone, so a confirmed
    /// one must leave the rest of its worktree group running.
    #[test]
    fn api_tab_relocate_confirmed_keeps_the_rest_of_the_worktree_group() {
        let event_hub = crate::api::EventHub::default();
        let mut app = worktree_group_fixture(&event_hub);
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let child_workspace_id = app.public_workspace_id(1);
        let target_workspace_id = app.public_workspace_id(2);

        app.handle_tab_relocate(
            "req".into(),
            TabRelocateParams {
                confirm_source_close: true,
                ..relocate_params(&tab_id, &target_workspace_id)
            },
        );

        assert_eq!(app.state.workspaces.len(), 2);
        let child = app
            .state
            .workspaces
            .iter()
            .find(|ws| ws.id == child_workspace_id)
            .expect("the group's linked worktree workspace stays open");
        assert_eq!(child.tabs.len(), 1);
    }

    #[test]
    fn api_tab_relocate_does_not_ask_when_the_source_workspace_survives() {
        let event_hub = crate::api::EventHub::default();
        let mut app = worktree_group_fixture(&event_hub);
        app.state.workspaces[0].test_add_tab(Some("second"));
        app.state.ensure_test_terminals();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let target_workspace_id = app.public_workspace_id(2);

        let response =
            app.handle_tab_relocate("req".into(), relocate_params(&tab_id, &target_workspace_id));

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::TabRelocated {
                closed_workspace_id: None,
                ..
            }
        ));
        assert_eq!(app.state.workspaces.len(), 3);
    }
}
