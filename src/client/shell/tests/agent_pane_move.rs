use super::tab_relocate::{mouse, relocate_snapshot, workspace_row};
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

fn agent_state(projected: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
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
fn a_shared_tabs_label_stays_with_the_panes_left_behind() {
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
