use super::tab_relocate::{
    carried_label, label_at, mouse, muted, relocate_snapshot, workspace_row,
};
use super::*;
use crate::api::schema::{Method, PaneMoveDestination};
use crossterm::event::{MouseButton, MouseEventKind};

fn agent(pane_id: &str, tab_id: &str, workspace_id: &str) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        name: Some(pane_id.into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: pane_id == "pane_1",
    }
}

/// The relocation snapshot with agents: `pane_1` and `pane_1b` share `tab_1`, `pane_2` sits
/// alone in `tab_2`, and `ws_2` stays empty so it can take a drop.
fn agent_snapshot() -> ClientShellSnapshot {
    let mut projected = relocate_snapshot();
    for (pane_id, tab_id) in [("pane_1b", "tab_1"), ("pane_2", "tab_2")] {
        let mut pane = projected.panes[0].clone();
        pane.pane_id = pane_id.into();
        pane.tab_id = tab_id.into();
        pane.focused = false;
        projected.panes.push(pane);
    }
    projected.agents = vec![
        agent("pane_1", "tab_1", "ws_1"),
        agent("pane_1b", "tab_1", "ws_1"),
        agent("pane_2", "tab_2", "ws_1"),
    ];
    projected
}

/// Every machine a current server runs advertises the agent-view projection capability, so a
/// panel gesture has to hold on the projection path the owner's client actually takes.
fn support_agent_view_projection(state: &mut ClientShellState, remote: &ClientEndpointId) {
    for endpoint_id in [&ClientEndpointId::Local, remote] {
        state.set_endpoint_agent_view_projection_supported(endpoint_id, true);
    }
}

fn agent_state(projected: ClientShellSnapshot) -> ClientShellState {
    agent_state_with_config(
        projected,
        ClientShellConfig::from_config(&Config::default()),
    )
}

fn agent_state_with_config(
    projected: ClientShellSnapshot,
    config: ClientShellConfig,
) -> ClientShellState {
    let mut state = ClientShellState::new(config);
    state.set_endpoint_agent_view_projection_supported(&ClientEndpointId::Local, true);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("sidebar with agent rows");
    state
}

fn agent_row(state: &ClientShellState, pane_id: &str) -> Rect {
    state
        .hits
        .agents
        .iter()
        .find(|(_, hit_pane_id, _)| hit_pane_id == pane_id)
        .unwrap_or_else(|| panic!("{pane_id} agent row"))
        .0
}

/// Press an agent row and take the pointer onto `target`, one cell at a time.
fn drag_agent_onto(state: &mut ClientShellState, source: Rect, target: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        source.x + 1,
        source.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        source.x + 2,
        source.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        target.x + 1,
        target.y,
    )])
}

fn release_at(state: &mut ClientShellState, target: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        target.x + 1,
        target.y,
    )])
}

#[test]
fn agent_dragged_onto_another_space_moves_its_pane_into_a_new_tab() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    let drag = drag_agent_onto(&mut state, source, target);
    assert!(drag.repaint);

    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request, got {:?}", release.actions.len());
    };
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if params.pane_id == "pane_1"
                && params.focus
                && matches!(
                    &params.destination,
                    PaneMoveDestination::NewTab { workspace_id, label }
                        if workspace_id.as_deref() == Some("ws_2") && label.is_none()
                )
    ));
}

#[test]
fn agent_clicked_without_movement_only_focuses_it() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1b");

    let press = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        source.x + 1,
        source.y,
    )]);
    let [ClientShellAction::Endpoint { request, .. }] = &press.actions[..] else {
        panic!("agent focus request");
    };
    assert!(matches!(
        &request.method,
        Method::PaneFocus(target) if target.pane_id == "pane_1b"
    ));
    assert!(state.chrome_drag.is_none());

    let release = release_at(&mut state, source);
    assert!(release.actions.is_empty());
}

#[test]
fn agent_dropped_on_its_own_space_does_nothing() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_1");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
}

#[test]
fn agent_dropped_on_a_foreign_machine_space_is_refused() {
    let (mut state, endpoint_id) = super::endpoints::state_with_remote();
    // Another machine's spaces are only reachable as rows when they nest under it.
    state.config.machines_nest_spaces = true;
    state.set_snapshot(Box::new(agent_snapshot()));
    state.compose(106, 34).expect("aggregate sidebar");
    let source = state
        .hits
        .endpoint_agents
        .iter()
        .find(|(_, hit_endpoint_id, pane_id, _)| hit_endpoint_id.is_local() && pane_id == "pane_1")
        .expect("local agent row")
        .0;
    let target = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote space row")
        .rect;

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
}

#[test]
fn moved_agent_pane_leaves_its_tab_siblings_behind() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    assert!(matches!(&request.method, Method::PaneMove(params) if params.pane_id == "pane_1"));
    // The sibling pane keeps the tab it shares, so the move names neither the tab nor the sibling.
    let projection = state.snapshot.as_deref().expect("projection");
    assert!(projection
        .panes
        .iter()
        .any(|pane| pane.pane_id == "pane_1b" && pane.tab_id == "tab_1"));
}

#[test]
fn a_solo_agent_pane_carries_its_custom_tab_label() {
    let mut projected = agent_snapshot();
    for tab in &mut projected.tabs {
        if tab.tab_id == "tab_2" {
            tab.label = "auto-impl TA-312".into();
            tab.custom_label = true;
        }
    }
    let mut state = agent_state(projected);
    let source = agent_row(&state, "pane_2");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if matches!(
                &params.destination,
                PaneMoveDestination::NewTab { label, .. }
                    if label.as_deref() == Some("auto-impl TA-312")
            )
    ));
}

#[test]
fn a_shared_tabs_label_travels_with_the_pane_that_leaves() {
    let mut projected = agent_snapshot();
    for tab in &mut projected.tabs {
        if tab.tab_id == "tab_1" {
            tab.label = "auto-impl TA-312".into();
            tab.custom_label = true;
        }
    }
    let mut state = agent_state(projected);
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    // The name describes the work, which leaves with the pane; the siblings keep it too.
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if matches!(
                &params.destination,
                PaneMoveDestination::NewTab { label, .. }
                    if label.as_deref() == Some("auto-impl TA-312")
            )
    ));
}

#[test]
fn an_auto_named_tab_sends_no_label_because_it_only_has_a_number() {
    let mut projected = agent_snapshot();
    for tab in &mut projected.tabs {
        if tab.tab_id == "tab_1" {
            tab.label = "3".into();
            tab.custom_label = false;
        }
    }
    let mut state = agent_state(projected);
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if matches!(
                &params.destination,
                PaneMoveDestination::NewTab { label, .. } if label.is_none()
            )
    ));
}

#[test]
fn agent_pane_move_without_server_support_refuses_the_drop_with_a_notice() {
    let mut state = agent_state(agent_snapshot());
    state.set_endpoint_methods(Some(vec!["pane.focus".into()]));
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("unsupported pane move notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Unsupported);
    assert_eq!(notice.key.code, "pane.move");
}

#[test]
fn agent_drop_target_row_highlights_only_while_the_drag_is_over_it() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");
    let frame = state.compose(106, 30).expect("idle sidebar");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_ne!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );

    drag_agent_onto(&mut state, source, target);
    let frame = state.compose(106, 30).expect("drag over the space row");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );

    release_at(&mut state, target);
    let frame = state.compose(106, 30).expect("sidebar after the drop");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_ne!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );
}

#[test]
fn agent_drag_carries_its_label_and_mutes_the_row_it_leaves() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    let frame = state.compose(106, 30).expect("idle sidebar");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        carried_label(&buffer, &state.config.palette, (source.x + 1, source.y), 8),
        None
    );
    assert!(!muted(&buffer, source));

    drag_agent_onto(&mut state, source, target);
    let frame = state.compose(106, 30).expect("drag over the space row");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        carried_label(&buffer, &state.config.palette, (target.x + 1, target.y), 8),
        Some(" pane_1 ".into())
    );
    assert!(muted(&buffer, source));

    release_at(&mut state, target);
    let frame = state.compose(106, 30).expect("sidebar after the drop");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        carried_label(&buffer, &state.config.palette, (target.x + 1, target.y), 8),
        None
    );
    assert!(!muted(&buffer, source));
}

#[test]
fn agent_press_without_movement_carries_nothing() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1b");

    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        source.x + 1,
        source.y,
    )]);
    let frame = state.compose(106, 30).expect("pressed agent row");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        carried_label(&buffer, &state.config.palette, (source.x + 1, source.y), 9),
        None
    );
    assert!(!muted(&buffer, source));
}

#[test]
fn agent_drag_carry_stays_inside_the_frame_at_the_bottom_right_corner() {
    let mut state = agent_state(agent_snapshot());
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");
    drag_agent_onto(&mut state, source, target);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        105,
        29,
    )]);

    let frame = state.compose(106, 30).expect("drag at the frame corner");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        label_at(&buffer, &state.config.palette, 98, 28, 8),
        Some(" pane_1 ".into())
    );
}

#[test]
fn grouped_agent_row_drops_on_a_space_header_of_the_tree() {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Grouped;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    // A space only reaches the tree once it owns an agent, so `ws_2` gets one of its own.
    let mut projected = agent_snapshot();
    let mut pane = projected.panes[0].clone();
    pane.pane_id = "pane_3".into();
    pane.tab_id = "tab_3".into();
    pane.workspace_id = "ws_2".into();
    pane.focused = false;
    projected.panes.push(pane);
    projected.agents.push(agent("pane_3", "tab_3", "ws_2"));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("grouped agent tree");
    // The tree replaces the spaces list, so its space headers are the only space rows left.
    assert!(state.hits.workspaces.is_empty());
    let source = agent_row(&state, "pane_1");
    let target = state
        .hits
        .agent_groups
        .iter()
        .find(|(_, _, key)| key == "agents:ws_2")
        .map(|(rect, _, _)| *rect)
        .expect("ws_2 space header");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if params.pane_id == "pane_1"
                && matches!(
                    &params.destination,
                    PaneMoveDestination::NewTab { workspace_id, .. }
                        if workspace_id.as_deref() == Some("ws_2")
                )
    ));
}

#[test]
fn endpoint_agent_row_drops_on_a_nested_space_row_of_its_own_machine() {
    let (mut state, _) = super::endpoints::state_with_remote();
    state.config.machines_nest_spaces = true;
    state.set_snapshot(Box::new(agent_snapshot()));
    state.compose(106, 34).expect("aggregate sidebar");
    let source = state
        .hits
        .endpoint_agents
        .iter()
        .find(|(_, endpoint_id, pane_id, _)| endpoint_id.is_local() && pane_id == "pane_1")
        .expect("local agent row")
        .0;
    let target = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id.is_local() && hit.workspace_id == "ws_2")
        .expect("local ws_2 row")
        .rect;

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request");
    };
    assert!(matches!(
        &request.method,
        Method::PaneMove(params)
            if params.pane_id == "pane_1"
                && matches!(
                    &params.destination,
                    PaneMoveDestination::NewTab { workspace_id, .. }
                        if workspace_id.as_deref() == Some("ws_2")
                )
    ));
}

/// The owner's grouped view: two online machines, grouped sort (their config's retired
/// `agent_panel_scope` and their persisted chrome preference both select it), and the flat
/// sidebar -- a machines band, the active machine's spaces band, and the grouped agents panel
/// under the divider. `ws_2` owns an agent of its own so it also reaches the grouped tree.
fn grouped_flat_state() -> (ClientShellState, ClientEndpointId) {
    grouped_flat_state_with_preferences(&preferences_path("grouped-flat"))
}

/// The owner's grouped view on a preferences file of its own, so an arrangement can be
/// dragged, written and read back in the layout they actually run. Every helper in this file
/// routes through here, which keeps the owner's configuration -- not a permissive default --
/// the one the panel gestures are proved against.
fn grouped_flat_state_with_preferences(
    path: &std::path::Path,
) -> (ClientShellState, ClientEndpointId) {
    aggregate_state_with_preferences(path, crate::config::AgentPanelSortConfig::Grouped)
}

/// The owner's two machines and flat sidebar on a chosen panel sort mode.
fn aggregate_state_with_preferences(
    path: &std::path::Path,
    sort: crate::config::AgentPanelSortConfig,
) -> (ClientShellState, ClientEndpointId) {
    let mut config = Config::default();
    config.ui.agent_panel_sort = sort;
    let (mut state, endpoint_id) = super::endpoints::state_with_remote_config(
        ClientShellConfig::from_config(&config).with_preferences_path(path.to_path_buf()),
    );
    // The live client only ever sends what the machine advertises, so the gesture has to be
    // exercised against the real client-shell method list.
    state.set_endpoint_methods(Some(
        crate::server::client_commands::supported_client_shell_method_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
    ));
    support_agent_view_projection(&mut state, &endpoint_id);
    let mut remote = agent_snapshot();
    remote.boot_id = "remote-boot".into();
    remote.workspaces[0].label = "remote-workspace".into();
    remote.agents = vec![
        agent("pane_r", "tab_1", "ws_1"),
        agent("pane_r2", "tab_1", "ws_1"),
    ];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    state.set_snapshot(Box::new(grouped_local_snapshot()));
    state
        .compose(106, 40)
        .expect("grouped agents under the spaces band");
    (state, endpoint_id)
}

/// The active machine's snapshot with an agent in `ws_2` as well, so both spaces reach the
/// grouped tree and either space can take a drop.
fn grouped_local_snapshot() -> ClientShellSnapshot {
    let mut projected = agent_snapshot();
    let mut pane = projected.panes[0].clone();
    pane.pane_id = "pane_3".into();
    pane.tab_id = "tab_3".into();
    pane.workspace_id = "ws_2".into();
    pane.focused = false;
    projected.panes.push(pane);
    projected.agents.push(agent("pane_3", "tab_3", "ws_2"));
    projected
}

fn grouped_agent_row(state: &ClientShellState, pane_id: &str) -> Rect {
    state
        .hits
        .endpoint_agents
        .iter()
        .find(|(_, endpoint_id, hit_pane_id, _)| endpoint_id.is_local() && hit_pane_id == pane_id)
        .unwrap_or_else(|| panic!("{pane_id} grouped agent row"))
        .0
}

fn local_workspace_row(state: &ClientShellState, workspace_id: &str) -> Rect {
    state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id.is_local() && hit.workspace_id == workspace_id)
        .unwrap_or_else(|| panic!("{workspace_id} row"))
        .rect
}

fn grouped_space_header(
    state: &ClientShellState,
    endpoint_id: &ClientEndpointId,
    workspace_id: &str,
) -> Rect {
    let key = format!("agents:{workspace_id}");
    state
        .hits
        .agent_groups
        .iter()
        .find(|(_, hit_endpoint_id, hit_key)| hit_endpoint_id == endpoint_id && *hit_key == key)
        .unwrap_or_else(|| panic!("{workspace_id} space header"))
        .0
}

fn moved_pane_target(release: &ClientShellInput) -> (String, Option<String>, Option<String>) {
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request, got {:?}", release.actions.len());
    };
    let Method::PaneMove(params) = &request.method else {
        panic!("pane move method");
    };
    assert!(params.focus);
    let PaneMoveDestination::NewTab {
        workspace_id,
        label,
    } = &params.destination
    else {
        panic!("new tab destination");
    };
    (params.pane_id.clone(), workspace_id.clone(), label.clone())
}

#[test]
fn grouped_panel_agent_drops_on_a_space_row_of_the_flat_sidebar() {
    let (mut state, _) = grouped_flat_state();
    let source = grouped_agent_row(&state, "pane_1");
    let target = local_workspace_row(&state, "ws_2");

    let drag = drag_agent_onto(&mut state, source, target);
    assert!(drag.repaint);
    let release = release_at(&mut state, target);
    assert_eq!(
        moved_pane_target(&release),
        ("pane_1".into(), Some("ws_2".into()), None)
    );
}

#[test]
fn grouped_panel_agent_drops_on_a_space_header_of_its_own_machine() {
    let (mut state, _) = grouped_flat_state();
    let source = grouped_agent_row(&state, "pane_1");
    let target = grouped_space_header(&state, &ClientEndpointId::Local, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert_eq!(
        moved_pane_target(&release),
        ("pane_1".into(), Some("ws_2".into()), None)
    );
}

#[test]
fn grouped_panel_agent_clicked_without_movement_only_focuses_it() {
    let (mut state, _) = grouped_flat_state();
    let source = grouped_agent_row(&state, "pane_1b");

    let press = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        source.x + 1,
        source.y,
    )]);
    // With more than one machine on screen the row selects its machine and the pane on it.
    let [ClientShellAction::ActivateEndpoint {
        endpoint_id,
        target,
    }] = &press.actions[..]
    else {
        panic!("agent focus request, got {:?}", press.actions);
    };
    assert!(endpoint_id.is_local());
    assert_eq!(
        target.as_ref(),
        Some(&ClientEndpointFocusTarget::Pane("pane_1b".into()))
    );
    assert!(state.chrome_drag.is_none());

    let release = release_at(&mut state, source);
    assert!(release.actions.is_empty());
}

#[test]
fn grouped_panel_agent_dropped_on_its_own_space_does_nothing() {
    let (mut state, _) = grouped_flat_state();
    let source = grouped_agent_row(&state, "pane_1");
    for target in [
        local_workspace_row(&state, "ws_1"),
        grouped_space_header(&state, &ClientEndpointId::Local, "ws_1"),
    ] {
        drag_agent_onto(&mut state, source, target);
        let release = release_at(&mut state, target);
        assert!(release.actions.is_empty());
    }
}

#[test]
fn grouped_panel_agent_dropped_on_a_foreign_machine_space_is_refused() {
    let (mut state, endpoint_id) = grouped_flat_state();
    let source = grouped_agent_row(&state, "pane_1");
    let target = grouped_space_header(&state, &endpoint_id, "ws_1");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
}

#[test]
fn grouped_panel_agent_of_a_foreign_machine_is_not_draggable() {
    let (mut state, endpoint_id) = grouped_flat_state();
    let source = state
        .hits
        .endpoint_agents
        .iter()
        .find(|(_, hit_endpoint_id, _, _)| *hit_endpoint_id == endpoint_id)
        .expect("remote agent row")
        .0;
    let target = local_workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    assert!(state.chrome_drag.is_none());
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
}

#[test]
fn grouped_panel_drop_target_rows_highlight_only_while_the_drag_is_over_them() {
    for header in [false, true] {
        let (mut state, _) = grouped_flat_state();
        let source = grouped_agent_row(&state, "pane_1");
        let target = if header {
            grouped_space_header(&state, &ClientEndpointId::Local, "ws_2")
        } else {
            local_workspace_row(&state, "ws_2")
        };
        let frame = state.compose(106, 40).expect("idle sidebar");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        assert_ne!(
            buffer[(target.x, target.y)].bg,
            state.config.palette.surface1
        );

        drag_agent_onto(&mut state, source, target);
        let frame = state.compose(106, 40).expect("drag over the target row");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        assert_eq!(
            buffer[(target.x, target.y)].bg,
            state.config.palette.surface1
        );

        release_at(&mut state, target);
        let frame = state.compose(106, 40).expect("sidebar after the drop");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        assert_ne!(
            buffer[(target.x, target.y)].bg,
            state.config.palette.surface1
        );
    }
}

#[test]
fn grouped_panel_solo_agent_pane_carries_its_custom_tab_label() {
    let (mut state, endpoint_id) = grouped_flat_state();
    let mut projected = grouped_local_snapshot();
    for tab in &mut projected.tabs {
        if tab.tab_id == "tab_2" {
            tab.label = "auto-impl TA-312".into();
            tab.custom_label = true;
        }
    }
    let _ = endpoint_id;
    state.set_snapshot(Box::new(projected));
    state.compose(106, 40).expect("grouped sidebar");
    let source = grouped_agent_row(&state, "pane_2");
    let target = local_workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert_eq!(
        moved_pane_target(&release),
        (
            "pane_2".into(),
            Some("ws_2".into()),
            Some("auto-impl TA-312".into())
        )
    );
}

#[test]
fn grouped_panel_agent_pane_move_a_machine_cannot_take_is_refused_with_a_notice() {
    let (mut state, _) = grouped_flat_state();
    // A machine whose command lane does not carry `pane.move` has to say so, not swallow the drop.
    state.set_endpoint_methods(Some(vec!["pane.focus".into()]));
    let source = grouped_agent_row(&state, "pane_1");
    let target = local_workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("unsupported pane move notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Unsupported);
    assert_eq!(notice.key.code, "pane.move");
}

/// `agent_snapshot()` plus a second group: `pane_3` and `pane_4` run in `ws_2`.
fn two_group_snapshot() -> ClientShellSnapshot {
    let mut projected = agent_snapshot();
    for pane_id in ["pane_3", "pane_4"] {
        let mut pane = projected.panes[0].clone();
        pane.pane_id = pane_id.into();
        pane.tab_id = "tab_3".into();
        pane.workspace_id = "ws_2".into();
        pane.focused = false;
        projected.panes.push(pane);
        projected.agents.push(agent(pane_id, "tab_3", "ws_2"));
    }
    projected
}

/// The panel's rows in the order it draws them.
fn panel_pane_ids(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .agents
        .iter()
        .map(|(_, pane_id, _)| pane_id.clone())
        .collect()
}

fn preferences_path(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "herdr-agent-reorder-{name}-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn agent_state_with_preferences(
    projected: ClientShellSnapshot,
    path: &std::path::Path,
) -> ClientShellState {
    agent_state_with_config(
        projected,
        ClientShellConfig::from_config(&Config::default())
            .with_preferences_path(path.to_path_buf()),
    )
}

/// Drag the row of `pane_id` onto the row of `onto` and drop it there.
fn reorder_agent(state: &mut ClientShellState, pane_id: &str, onto: &str) -> ClientShellInput {
    let source = agent_row(state, pane_id);
    let target = agent_row(state, onto);
    drag_agent_onto(state, source, target);
    let release = release_at(state, target);
    state.compose(106, 30).expect("sidebar after the reorder");
    release
}

#[test]
fn agent_dragged_up_inside_its_group_reorders_the_panel() {
    let path = preferences_path("up");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    assert_eq!(panel_pane_ids(&state), ["pane_1", "pane_1b", "pane_2"]);

    let release = reorder_agent(&mut state, "pane_2", "pane_1");
    assert!(release.actions.is_empty(), "{:?}", release.actions);
    assert_eq!(panel_pane_ids(&state), ["pane_2", "pane_1", "pane_1b"]);

    let reloaded = agent_state_with_preferences(agent_snapshot(), &path);
    assert_eq!(panel_pane_ids(&reloaded), ["pane_2", "pane_1", "pane_1b"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn agent_dragged_down_inside_its_group_reorders_the_panel() {
    let path = preferences_path("down");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);

    reorder_agent(&mut state, "pane_1", "pane_2");
    assert_eq!(panel_pane_ids(&state), ["pane_1b", "pane_2", "pane_1"]);

    let reloaded = agent_state_with_preferences(agent_snapshot(), &path);
    assert_eq!(panel_pane_ids(&reloaded), ["pane_1b", "pane_2", "pane_1"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_manual_order_outlives_agents_arriving_and_leaving() {
    let path = preferences_path("arrivals");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    reorder_agent(&mut state, "pane_2", "pane_1");

    let mut projected = agent_snapshot();
    projected.agents.retain(|agent| agent.pane_id != "pane_1b");
    let mut arrival = projected.panes[0].clone();
    arrival.pane_id = "pane_5".into();
    arrival.focused = false;
    projected.panes.push(arrival);
    projected.agents.push(agent("pane_5", "tab_1", "ws_1"));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("sidebar after the snapshot");

    // The arrangement holds, the agent that left drops out, and the new one lands at the end.
    assert_eq!(panel_pane_ids(&state), ["pane_2", "pane_1", "pane_5"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_group_never_reordered_still_follows_the_sort_mode() {
    let path = preferences_path("untouched-group");
    let mut state = agent_state_with_preferences(two_group_snapshot(), &path);
    reorder_agent(&mut state, "pane_2", "pane_1");

    let mut projected = two_group_snapshot();
    projected.agents.swap(3, 4);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("sidebar after the snapshot");

    assert_eq!(
        panel_pane_ids(&state),
        ["pane_2", "pane_1", "pane_1b", "pane_4", "pane_3"]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn switching_the_sort_mode_clears_every_manual_order() {
    let path = preferences_path("sort-escape");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    reorder_agent(&mut state, "pane_2", "pane_1");
    let toggle = state.hits.agent_sort_toggle;
    assert!(!toggle.is_empty(), "the sort label stays reachable");

    state.handle_raw_events(vec![
        mouse(MouseEventKind::Down(MouseButton::Left), toggle.x, toggle.y),
        mouse(MouseEventKind::Up(MouseButton::Left), toggle.x, toggle.y),
    ]);
    state
        .compose(106, 30)
        .expect("sidebar after the sort change");
    assert_eq!(panel_pane_ids(&state), ["pane_1", "pane_1b", "pane_2"]);

    let reloaded = agent_state_with_preferences(agent_snapshot(), &path);
    assert_eq!(panel_pane_ids(&reloaded), ["pane_1", "pane_1b", "pane_2"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_sort_label_names_the_manual_order_that_replaced_it() {
    let path = preferences_path("manual-label");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    let header = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).expect("agents header");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        let row = state.hits.agent_sort_toggle.y;
        (0..state.hits.agent_body.width)
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>()
    };
    assert!(header(&mut state).contains("spaces"));

    reorder_agent(&mut state, "pane_2", "pane_1");
    assert!(header(&mut state).contains("manual"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_agent_dropped_on_a_space_row_moves_its_pane_and_does_not_reorder() {
    let path = preferences_path("space-row");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    let source = agent_row(&state, "pane_1");
    let target = workspace_row(&state, "ws_2");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("pane move request, got {:?}", release.actions.len());
    };
    assert!(matches!(&request.method, Method::PaneMove(params) if params.pane_id == "pane_1"));
    state.compose(106, 30).expect("sidebar after the drop");
    assert_eq!(panel_pane_ids(&state), ["pane_1", "pane_1b", "pane_2"]);
    assert!(!path.exists(), "a move writes no panel order");
}

#[test]
fn an_agent_dragged_over_another_groups_rows_has_no_target() {
    let path = preferences_path("cross-group");
    let mut state = agent_state_with_preferences(two_group_snapshot(), &path);
    let source = agent_row(&state, "pane_1");
    let target = agent_row(&state, "pane_3");

    drag_agent_onto(&mut state, source, target);
    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
    state.compose(106, 30).expect("sidebar after the drop");
    assert_eq!(
        panel_pane_ids(&state),
        ["pane_1", "pane_1b", "pane_2", "pane_3", "pane_4"]
    );
    assert!(!path.exists(), "no group takes a row from another");
}

#[test]
fn the_insertion_indicator_shows_only_while_a_reorder_drag_is_live() {
    let path = preferences_path("indicator");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    let source = agent_row(&state, "pane_2");
    let target = agent_row(&state, "pane_1");
    let accent = state.config.palette.accent;
    let indicator = |state: &mut ClientShellState, row: u16| {
        let frame = state.compose(106, 30).expect("sidebar frame");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        buffer[(state.hits.agent_body.x, row)].fg == accent
    };
    assert!(!indicator(&mut state, target.y));

    drag_agent_onto(&mut state, source, target);
    assert!(
        indicator(&mut state, target.y),
        "the insertion line is drawn"
    );

    release_at(&mut state, target);
    assert!(!indicator(&mut state, target.y));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_agent_dropped_where_it_already_sits_writes_nothing() {
    let path = preferences_path("in-place");
    let mut state = agent_state_with_preferences(agent_snapshot(), &path);
    let source = agent_row(&state, "pane_1b");

    let release = reorder_agent(&mut state, "pane_1b", "pane_1b");
    assert!(release.actions.is_empty());
    assert_eq!(panel_pane_ids(&state), ["pane_1", "pane_1b", "pane_2"]);
    assert!(!muted(
        &state
            .compose(106, 30)
            .expect("sidebar after the drop")
            .to_ratatui_buffer()
            .expect("frame buffer"),
        source
    ));
    assert!(!path.exists(), "a drop in place keeps the panel unwritten");
}

/// The aggregate panel's agent rows for one machine, in the order it draws them.
fn panel_pane_ids_for(state: &ClientShellState, endpoint_id: &ClientEndpointId) -> Vec<String> {
    state
        .hits
        .endpoint_agents
        .iter()
        .filter(|(_, hit_endpoint_id, _, _)| hit_endpoint_id == endpoint_id)
        .map(|(_, _, pane_id, _)| pane_id.clone())
        .collect()
}

fn grouped_panel_pane_ids(state: &ClientShellState) -> Vec<String> {
    panel_pane_ids_for(state, &ClientEndpointId::Local)
}

fn reorder_grouped_agent(
    state: &mut ClientShellState,
    pane_id: &str,
    onto: &str,
) -> ClientShellInput {
    let source = grouped_agent_row(state, pane_id);
    let target = grouped_agent_row(state, onto);
    drag_agent_onto(state, source, target);
    let release = release_at(state, target);
    state
        .compose(106, 40)
        .expect("grouped sidebar after the reorder");
    release
}

#[test]
fn grouped_panel_agent_dragged_inside_its_group_reorders_the_rows_on_screen() {
    let path = preferences_path("grouped-reorder");
    let (mut state, _) = grouped_flat_state_with_preferences(&path);
    assert_eq!(
        grouped_panel_pane_ids(&state),
        ["pane_1", "pane_1b", "pane_2", "pane_3"]
    );

    let release = reorder_grouped_agent(&mut state, "pane_2", "pane_1");
    assert!(release.actions.is_empty(), "{:?}", release.actions);
    assert_eq!(
        grouped_panel_pane_ids(&state),
        ["pane_2", "pane_1", "pane_1b", "pane_3"]
    );

    let (reloaded, _) = grouped_flat_state_with_preferences(&path);
    assert_eq!(
        grouped_panel_pane_ids(&reloaded),
        ["pane_2", "pane_1", "pane_1b", "pane_3"]
    );
    let _ = std::fs::remove_file(&path);
}

/// The owner's own preferences file: one entry without a `profile_id` for the local machine
/// and one carrying the SSH profile, so both keys have to reach their own machine's rows.
fn write_arrangements(path: &std::path::Path, profile_id: &str) {
    std::fs::write(
        path,
        format!(
            r#"{{
  "agent_panel_sort": "grouped",
  "agent_group_order": [
    {{ "workspace_id": "ws_1", "pane_ids": ["pane_2", "pane_1", "pane_1b"] }},
    {{ "profile_id": "{profile_id}", "workspace_id": "ws_1", "pane_ids": ["pane_r2", "pane_r"] }}
  ]
}}"#
        ),
    )
    .expect("preferences file");
}

#[test]
fn a_stored_arrangement_orders_the_grouped_rows_of_every_machine() {
    let path = preferences_path("grouped-stored");
    let profile_id = {
        let (_, endpoint_id) = grouped_flat_state();
        match &endpoint_id {
            ClientEndpointId::Ssh(profile_id) => profile_id.to_string(),
            ClientEndpointId::Local => panic!("remote machine"),
        }
    };
    write_arrangements(&path, &profile_id);

    let (state, endpoint_id) = grouped_flat_state_with_preferences(&path);
    assert_eq!(
        grouped_panel_pane_ids(&state),
        ["pane_2", "pane_1", "pane_1b", "pane_3"]
    );
    assert_eq!(
        panel_pane_ids_for(&state, &endpoint_id),
        ["pane_r2", "pane_r"]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_flat_list_of_two_machines_follows_the_arrangement_over_its_sort_mode() {
    let path = preferences_path("flat-aggregate");
    let (_, endpoint_id) = grouped_flat_state();
    let ClientEndpointId::Ssh(profile_id) = &endpoint_id else {
        panic!("remote machine");
    };
    write_arrangements(&path, &profile_id.to_string());

    // Priority is the mode that reorders hardest, so an arranged group has to win over it
    // while the machine it does not name keeps following it.
    let (mut state, endpoint_id) =
        aggregate_state_with_preferences(&path, crate::config::AgentPanelSortConfig::Priority);
    state.compose(106, 60).expect("flat aggregate panel");
    assert_eq!(
        panel_pane_ids_for(&state, &ClientEndpointId::Local)[..3],
        ["pane_2", "pane_1", "pane_1b"]
    );
    assert_eq!(
        panel_pane_ids_for(&state, &endpoint_id),
        ["pane_r2", "pane_r"]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_grouped_machine_without_an_arrangement_still_follows_the_sort_mode() {
    let path = preferences_path("grouped-untouched");
    let (mut state, endpoint_id) = grouped_flat_state_with_preferences(&path);
    reorder_grouped_agent(&mut state, "pane_2", "pane_1");

    // The machine the arrangement never named keeps the order its snapshot arrives in.
    assert_eq!(
        panel_pane_ids_for(&state, &endpoint_id),
        ["pane_r", "pane_r2"]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_grouped_arrangement_outlives_agents_arriving_and_leaving() {
    let path = preferences_path("grouped-arrivals");
    let (mut state, _) = grouped_flat_state_with_preferences(&path);
    reorder_grouped_agent(&mut state, "pane_2", "pane_1");

    let mut projected = grouped_local_snapshot();
    projected.agents.retain(|agent| agent.pane_id != "pane_1b");
    let mut arrival = projected.panes[0].clone();
    arrival.pane_id = "pane_5".into();
    arrival.focused = false;
    projected.panes.push(arrival);
    projected.agents.push(agent("pane_5", "tab_1", "ws_1"));
    state.set_snapshot(Box::new(projected));
    state
        .compose(106, 40)
        .expect("grouped sidebar after the snapshot");

    assert_eq!(
        grouped_panel_pane_ids(&state),
        ["pane_2", "pane_1", "pane_5", "pane_3"]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_grouped_header_names_the_manual_order_and_clearing_it_drops_every_arrangement() {
    let path = preferences_path("grouped-label");
    let profile_id = {
        let (_, endpoint_id) = grouped_flat_state();
        match &endpoint_id {
            ClientEndpointId::Ssh(profile_id) => profile_id.to_string(),
            ClientEndpointId::Local => panic!("remote machine"),
        }
    };
    write_arrangements(&path, &profile_id);
    let (mut state, endpoint_id) = grouped_flat_state_with_preferences(&path);
    let header = |state: &mut ClientShellState| {
        let frame = state.compose(106, 40).expect("agents header");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        let row = state.hits.agent_sort_toggle.y;
        (0..state.hits.agent_body.width)
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>()
    };
    assert!(header(&mut state).contains("manual"));

    let toggle = state.hits.agent_sort_toggle;
    assert!(!toggle.is_empty(), "the sort label stays reachable");
    state.handle_raw_events(vec![
        mouse(MouseEventKind::Down(MouseButton::Left), toggle.x, toggle.y),
        mouse(MouseEventKind::Up(MouseButton::Left), toggle.x, toggle.y),
    ]);
    state
        .compose(106, 40)
        .expect("sidebar after the sort change");
    assert!(!header(&mut state).contains("manual"));
    state
        .compose(106, 40)
        .expect("sidebar after the header read");
    assert_eq!(
        panel_pane_ids_for(&state, &endpoint_id),
        ["pane_r", "pane_r2"],
        "the other machine's arrangement goes with it"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_grouped_insertion_line_marks_the_edge_the_row_lands_against() {
    let path = preferences_path("grouped-indicator");
    let (mut state, _) = grouped_flat_state_with_preferences(&path);
    let accent = state.config.palette.accent;
    let source = grouped_agent_row(&state, "pane_1");
    let target = grouped_agent_row(&state, "pane_2");
    // The space headers draw an accent chevron of their own, so the line is read on the row
    // it is expected on: the edge below the row the drag reached.
    let landing = target.bottom();
    let indicator = |state: &mut ClientShellState| {
        let frame = state.compose(106, 40).expect("grouped sidebar frame");
        let buffer = frame.to_ratatui_buffer().expect("frame buffer");
        buffer[(state.hits.agent_body.x, landing)].fg == accent
    };
    assert!(!indicator(&mut state));

    drag_agent_onto(&mut state, source, target);
    assert!(indicator(&mut state), "the insertion line is drawn");

    release_at(&mut state, target);
    state
        .compose(106, 40)
        .expect("grouped sidebar after the drop");
    assert!(!indicator(&mut state));
    assert_eq!(
        grouped_panel_pane_ids(&state),
        ["pane_1b", "pane_2", "pane_1", "pane_3"]
    );
    let _ = std::fs::remove_file(&path);
}

/// Right-click a cell in the owner's sidebar.
fn right_click(state: &mut ClientShellState, point: (u16, u16)) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Right),
        point.0,
        point.1,
    )])
}

/// Labels the open context menu offers, in the order it draws them.
fn menu_labels(state: &ClientShellState) -> Vec<&'static str> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.label).collect()
        }
        _ => Vec::new(),
    }
}

/// Click the menu row carrying `label`, the way the pointer reaches it on screen.
fn choose_menu_item(state: &mut ClientShellState, label: &str) -> ClientShellInput {
    let index = menu_labels(state)
        .iter()
        .position(|item| *item == label)
        .unwrap_or_else(|| panic!("{label} item in {:?}", menu_labels(state)));
    state.compose(106, 40).expect("context menu frame");
    let row = state.hits.context_menu_rows[index].0;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )])
}

/// A cell of the agents panel no row occupies, so a right-click there has no target.
fn empty_panel_cell(state: &ClientShellState) -> (u16, u16) {
    let body = state.hits.agent_body;
    let occupied = state
        .hits
        .endpoint_agents
        .iter()
        .map(|(rect, _, _, _)| rect.bottom())
        .chain(
            state
                .hits
                .agent_groups
                .iter()
                .map(|(rect, _, _)| rect.bottom()),
        )
        .max()
        .expect("grouped rows");
    assert!(occupied < body.bottom(), "the panel has a free row");
    (body.x + 1, occupied)
}

/// The machines band row of a machine, which is where its menu has to open.
fn machine_row(state: &ClientShellState, endpoint_id: &ClientEndpointId) -> Rect {
    state
        .hits
        .machines
        .iter()
        .find(|hit| &hit.endpoint_id == endpoint_id)
        .expect("machine row")
        .rect
}

#[test]
fn right_clicking_a_grouped_agent_row_offers_its_tab_and_its_pane() {
    let (mut state, _) = grouped_flat_state();
    let row = grouped_agent_row(&state, "pane_1");

    let outcome = right_click(&mut state, (row.x + 2, row.y));
    assert!(outcome.repaint);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    assert_eq!(
        menu_labels(&state),
        ["Rename tab", "Rename pane", "Close tab"]
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Agent {
                ref pane_id,
                ref tab_id,
            },
            ..
        })) if pane_id == "pane_1" && tab_id == "tab_1"
    ));
}

#[test]
fn renaming_a_tab_from_an_agent_row_sends_the_tab_rename() {
    let (mut state, _) = grouped_flat_state();
    let row = grouped_agent_row(&state, "pane_1");
    right_click(&mut state, (row.x + 2, row.y));

    choose_menu_item(&mut state, "Rename tab");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Tab { ref tab_id, .. },
            ..
        })) if tab_id == "tab_1"
    ));

    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "auto-impl TA-312",
    ))]);
    let accept = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))]);
    let [ClientShellAction::Endpoint { request, .. }] = &accept.actions[..] else {
        panic!("tab rename request, got {:?}", accept.actions);
    };
    assert!(matches!(
        &request.method,
        Method::TabRename(params)
            if params.tab_id == "tab_1" && params.label == "1auto-impl TA-312"
    ));
}

#[test]
fn renaming_a_pane_from_an_agent_row_targets_that_pane() {
    let (mut state, _) = grouped_flat_state();
    let row = grouped_agent_row(&state, "pane_1b");
    right_click(&mut state, (row.x + 2, row.y));

    choose_menu_item(&mut state, "Rename pane");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Pane { ref pane_id },
            ..
        })) if pane_id == "pane_1b"
    ));
}

#[test]
fn closing_a_tab_from_an_agent_row_closes_the_tab_the_row_names() {
    let (mut state, _) = grouped_flat_state();
    let row = grouped_agent_row(&state, "pane_2");
    right_click(&mut state, (row.x + 2, row.y));

    let outcome = choose_menu_item(&mut state, "Close tab");
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("tab close request, got {:?}", outcome.actions);
    };
    assert!(matches!(
        &request.method,
        Method::TabClose(target) if target.tab_id == "tab_2"
    ));
}

#[test]
fn right_clicking_a_saved_machine_row_renames_it_through_the_catalog() {
    let (mut state, endpoint_id) = grouped_flat_state();
    let row = machine_row(&state, &endpoint_id);

    right_click(&mut state, (row.x + 3, row.y));
    assert_eq!(menu_labels(&state), ["Rename"]);

    choose_menu_item(&mut state, "Rename");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Machine { endpoint_id: ref renamed },
            ..
        })) if renamed == &endpoint_id
    ));

    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('X'),
        KeyModifiers::NONE,
    ))]);
    let accept = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))]);
    assert!(
        matches!(
            accept.actions.as_slice(),
            [ClientShellAction::RenameEndpoint { endpoint_id: renamed, label }]
                if renamed == &endpoint_id && label.ends_with('X')
        ),
        "{:?}",
        accept.actions
    );
}

#[test]
fn the_local_machine_row_has_no_saved_label_to_rename() {
    let (mut state, _) = grouped_flat_state();
    let row = machine_row(&state, &ClientEndpointId::Local);

    right_click(&mut state, (row.x + 3, row.y));
    assert!(state.overlay.is_none(), "{:?}", menu_labels(&state));
}

#[test]
fn only_a_parked_machine_row_offers_a_reconnect() {
    let (mut state, endpoint_id) = grouped_flat_state();
    let row = machine_row(&state, &endpoint_id);
    right_click(&mut state, (row.x + 3, row.y));
    assert_eq!(menu_labels(&state), ["Rename"]);
    state.overlay = None;

    for status in [
        ClientEndpointStatus::Attention,
        ClientEndpointStatus::Reconnecting,
    ] {
        state.set_endpoint_status(&endpoint_id, status, None);
        state.compose(106, 40).expect("parked machine band");
        let row = machine_row(&state, &endpoint_id);
        right_click(&mut state, (row.x + 3, row.y));
        assert_eq!(
            menu_labels(&state),
            ["Rename", "Reconnect now"],
            "{status:?}"
        );

        let outcome = choose_menu_item(&mut state, "Reconnect now");
        assert!(
            matches!(
                outcome.actions.as_slice(),
                [ClientShellAction::ReconnectEndpoint { endpoint_id: requested }]
                    if requested == &endpoint_id
            ),
            "{status:?}: {:?}",
            outcome.actions
        );
    }
}

#[test]
fn right_clicking_a_space_row_opens_the_workspace_menu_in_both_layouts() {
    let (mut state, _) = grouped_flat_state();
    for target in [
        workspace_row(&state, "ws_2"),
        grouped_space_header(&state, &ClientEndpointId::Local, "ws_2"),
    ] {
        right_click(&mut state, (target.x + 3, target.y));
        assert!(
            matches!(
                state.overlay,
                Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                    target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
                    ..
                })) if workspace_id == "ws_2"
            ),
            "{target:?}: {:?}",
            menu_labels(&state)
        );
        state.overlay = None;
    }
}

#[test]
fn right_clicking_empty_panel_space_opens_nothing() {
    let (mut state, _) = grouped_flat_state();
    let point = empty_panel_cell(&state);

    let outcome = right_click(&mut state, point);
    assert!(state.overlay.is_none(), "{:?}", menu_labels(&state));
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
}

#[test]
fn left_clicking_a_grouped_agent_row_still_focuses_it_and_arms_a_drag() {
    let (mut state, _) = grouped_flat_state();
    let row = grouped_agent_row(&state, "pane_1b");

    let press = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x + 1,
        row.y,
    )]);
    assert!(
        matches!(
            press.actions.as_slice(),
            [ClientShellAction::ActivateEndpoint { endpoint_id, target: Some(ClientEndpointFocusTarget::Pane(pane_id)) }]
                if endpoint_id.is_local() && pane_id == "pane_1b"
        ),
        "{:?}",
        press.actions
    );
    assert!(state.agent_press.is_some(), "the drag stays armed");
    assert!(state.overlay.is_none());
}

#[test]
fn left_clicking_a_grouped_space_header_still_folds_it() {
    let (mut state, _) = grouped_flat_state();
    let header = grouped_space_header(&state, &ClientEndpointId::Local, "ws_2");

    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        header.x + 3,
        header.y,
    )]);
    state
        .compose(106, 40)
        .expect("grouped sidebar after the fold");
    assert!(state.overlay.is_none());
    assert!(
        grouped_panel_pane_ids(&state)
            .iter()
            .all(|pane_id| pane_id != "pane_3"),
        "{:?}",
        grouped_panel_pane_ids(&state)
    );
}
