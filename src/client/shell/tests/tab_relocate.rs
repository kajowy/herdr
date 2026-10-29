use super::*;
use crate::api::schema::{Method, TabInfo};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

/// Two local spaces: the focused `ws_1` carries `tab_1` and `tab_2`, `ws_2` carries `tab_3`.
pub(super) fn relocate_snapshot() -> ClientShellSnapshot {
    let mut projected = snapshot();
    let mut sibling_tab = projected.tabs[0].clone();
    sibling_tab.tab_id = "tab_2".into();
    sibling_tab.number = 2;
    sibling_tab.label = "2".into();
    sibling_tab.focused = false;
    projected.tabs.push(sibling_tab);
    let mut other_workspace = projected.workspaces[0].clone();
    other_workspace.workspace_id = "ws_2".into();
    other_workspace.active_tab_id = "tab_3".into();
    other_workspace.number = 2;
    other_workspace.label = "other".into();
    other_workspace.focused = false;
    projected.workspaces.push(other_workspace);
    let mut other_tab = projected.tabs[0].clone();
    other_tab.tab_id = "tab_3".into();
    other_tab.workspace_id = "ws_2".into();
    other_tab.number = 1;
    other_tab.label = "1".into();
    other_tab.focused = false;
    projected.tabs.push(other_tab);
    projected
}

fn relocate_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(relocate_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("two spaces and two tabs");
    state
}

pub(super) fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

pub(super) fn workspace_row(state: &ClientShellState, workspace_id: &str) -> Rect {
    state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == workspace_id)
        .unwrap_or_else(|| panic!("{workspace_id} row"))
        .rect
}

/// Press the first tab and take the pointer off the press position, still inside the bar.
fn begin_tab_drag(state: &mut ClientShellState) {
    let tab = state.hits.tabs[0].0;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        tab.x + 1,
        tab.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        tab.x + 2,
        tab.y,
    )]);
}

fn drag_onto(state: &mut ClientShellState, target: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        target.x + 1,
        target.y,
    )])
}

/// Press the first tab and drag the pointer onto the given sidebar row.
fn drag_first_tab_onto(state: &mut ClientShellState, target: Rect) -> ClientShellInput {
    begin_tab_drag(state);
    drag_onto(state, target)
}

fn release_at(state: &mut ClientShellState, target: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        target.x + 1,
        target.y,
    )])
}

#[test]
fn tab_dragged_onto_another_space_relocates_it_and_follows() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_2");
    let drag = drag_first_tab_onto(&mut state, target);

    assert!(drag.repaint);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            ref tab_id,
            ref workspace_target,
            insert_index: None,
            ..
        }) if tab_id == "tab_1" && workspace_target.as_deref() == Some("ws_2")
    ));

    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };
    assert!(matches!(
        &request.method,
        Method::TabRelocate(params)
            if params.tab_id == "tab_1"
                && params.workspace_id == "ws_2"
                && params.insert_index.is_none()
                && params.focus
    ));
}

#[test]
fn tab_dragged_inside_the_bar_still_reorders_in_place() {
    let mut state = relocate_state();
    let first = state.hits.tabs[0].0;
    let second = state.hits.tabs[1].0;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        first.x + 1,
        first.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        second.right().saturating_sub(1),
        second.y,
    )]);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            insert_index: Some(2),
            ref workspace_target,
            ..
        }) if workspace_target.is_none()
    ));

    let release = state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        second.right().saturating_sub(1),
        second.y,
    )]);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab move request");
    };
    assert!(matches!(
        &request.method,
        Method::TabMove(params) if params.tab_id == "tab_1" && params.insert_index == 2
    ));
}

#[test]
fn tab_dropped_on_its_own_space_does_nothing() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_1");
    drag_first_tab_onto(&mut state, target);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            ref workspace_target,
            insert_index: None,
            ..
        }) if workspace_target.is_none()
    ));

    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
}

#[test]
fn tab_dropped_on_a_foreign_machine_space_is_refused() {
    let (mut state, endpoint_id) = super::endpoints::state_with_remote();
    // Another machine's spaces are only reachable as rows when they nest under it.
    state.config.machines_nest_spaces = true;
    state.set_snapshot(Box::new(relocate_snapshot()));
    state.compose(106, 28).expect("aggregate sidebar");
    let remote = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.endpoint_id == endpoint_id)
        .expect("remote space row")
        .rect;

    let drag = drag_first_tab_onto(&mut state, remote);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            insert_index: None,
            workspace_target: None,
            ..
        })
    ));
    assert!(drag.actions.is_empty());
    assert!(release_at(&mut state, remote).actions.is_empty());
}

#[test]
fn tab_relocate_without_server_support_refuses_the_drop_with_a_notice() {
    let mut state = relocate_state();
    state.set_endpoint_methods(Some(vec!["tab.move".into()]));
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);

    let release = release_at(&mut state, target);
    assert!(release.actions.is_empty());
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("unsupported relocate notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Unsupported);
    assert_eq!(notice.key.code, "tab.relocate");
}

#[test]
fn tab_relocate_rejection_names_the_failed_action() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };

    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Err(ClientShellEndpointError {
            code: Some("tab_relocate_failed".into()),
            message: "relocated tab is unavailable".into(),
        }),
    );

    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("rejection notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Rejected);
    assert_eq!(notice.key.code, "tab.relocate:tab_relocate_failed");
    assert_eq!(notice.body, "relocated tab is unavailable");
}

#[test]
fn tab_relocate_confirmation_asks_before_it_closes_the_source_space() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };
    assert!(matches!(
        &request.method,
        Method::TabRelocate(params) if !params.confirm_source_close
    ));

    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Err(ClientShellEndpointError {
            code: Some("confirmation_required".into()),
            message: "relocating this tab closes the workspace it leaves behind".into(),
        }),
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(_))
    ));

    let accepted = state.handle_input_bytes(b"\r");
    assert!(matches!(
        accepted.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, Method::TabRelocate(params)
                if params.tab_id == "tab_1"
                    && params.workspace_id == "ws_2"
                    && params.focus
                    && params.confirm_source_close)
    ));
}

/// The confirmation is about a move, not about closing a tab or a worktree group.
#[test]
fn tab_relocate_confirmation_reads_as_a_move() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };

    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Err(ClientShellEndpointError {
            code: Some("confirmation_required".into()),
            message: "relocating this tab closes the workspace it leaves behind".into(),
        }),
    );

    let Some(ClientShellOverlay::ConfirmClose(confirm)) = state.overlay.as_ref() else {
        panic!("relocation confirmation");
    };
    assert_eq!(confirm.title, "Move tab and close workspace?");
    assert!(
        confirm.detail.contains("moving this tab empties it"),
        "detail: {}",
        confirm.detail
    );
}

#[test]
fn confirmed_tab_relocate_completes_and_drops_the_source_space() {
    let mut state = relocate_state();
    let mut projected = relocate_snapshot();
    // The focused space holds the dragged tab alone, so relocating it closes the space.
    projected.tabs.retain(|tab| tab.tab_id != "tab_2");
    state.set_snapshot(Box::new(projected));
    state
        .compose(106, 20)
        .expect("one tab in the focused space");
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };
    state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Err(ClientShellEndpointError {
            code: Some("confirmation_required".into()),
            message: "relocating this tab closes the workspace it leaves behind".into(),
        }),
    );
    let accepted = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &accepted.actions[..] else {
        panic!("confirmed tab relocate request");
    };
    assert!(matches!(
        &request.method,
        Method::TabRelocate(params) if params.confirm_source_close
    ));

    let moved = TabInfo {
        tab_id: "tab_1_2".into(),
        workspace_id: "ws_2".into(),
        number: 1,
        label: "1".into(),
        focused: true,
        pane_count: 1,
        agent_status: crate::api::schema::AgentStatus::Idle,
    };
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(crate::api::schema::ResponseResult::TabRelocated {
            tab: moved.clone(),
            tabs: vec![moved],
            insert_index: 0,
            closed_workspace_id: Some("ws_1".into()),
        }),
    );

    assert!(repaint);
    assert!(actions.is_empty());
    assert!(state.overlay.is_none());
    assert!(state.visible_endpoint_notice.is_none());
    let projection = state.snapshot.as_deref().expect("projection");
    assert!(projection
        .workspaces
        .iter()
        .all(|workspace| workspace.workspace_id == "ws_2"));
    assert_eq!(projection.focused_tab_id.as_deref(), Some("tab_1_2"));
}

#[test]
fn tab_drop_target_row_highlights_only_while_a_tab_drag_is_over_it() {
    let mut state = relocate_state();
    let target = workspace_row(&state, "ws_2");
    // The sidebar divider also draws a bar on the tab bar row, so count the ones already there.
    let bars = |buffer: &Buffer| {
        buffer
            .content()
            .iter()
            .take(buffer.area.width as usize)
            .filter(|cell| cell.symbol() == "│")
            .count()
    };
    let frame = state.compose(106, 20).expect("idle sidebar");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    let idle_bars = bars(&buffer);
    assert_ne!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );

    begin_tab_drag(&mut state);
    let frame = state.compose(106, 20).expect("drag inside the bar");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(bars(&buffer), idle_bars + 1, "in-bar insert indicator");

    drag_onto(&mut state, target);
    let frame = state.compose(106, 20).expect("drag over the space row");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_eq!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );
    // The in-bar insert indicator must not compete with the sidebar drop target.
    assert_eq!(bars(&buffer), idle_bars);

    release_at(&mut state, target);
    let frame = state.compose(106, 20).expect("sidebar after the drop");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    assert_ne!(
        buffer[(target.x, target.y)].bg,
        state.config.palette.surface1
    );
}

#[test]
fn relocated_tab_response_repoints_the_projection_and_drops_a_closed_space() {
    let mut state = relocate_state();
    let mut projected = relocate_snapshot();
    // The focused space holds the dragged tab alone, so relocating it closes the space.
    projected.tabs.retain(|tab| tab.tab_id != "tab_2");
    state.set_snapshot(Box::new(projected));
    state
        .compose(106, 20)
        .expect("one tab in the focused space");
    let target = workspace_row(&state, "ws_2");
    drag_first_tab_onto(&mut state, target);
    let release = release_at(&mut state, target);
    let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
        panic!("tab relocate request");
    };

    // The relocation renumbers the target workspace, so every public tab id there changes.
    let moved = TabInfo {
        tab_id: "tab_1_2".into(),
        workspace_id: "ws_2".into(),
        number: 1,
        label: "1".into(),
        focused: true,
        pane_count: 1,
        agent_status: crate::api::schema::AgentStatus::Idle,
    };
    let shifted = TabInfo {
        tab_id: "tab_2_2".into(),
        number: 2,
        focused: false,
        ..moved.clone()
    };
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(crate::api::schema::ResponseResult::TabRelocated {
            tab: moved.clone(),
            tabs: vec![moved, shifted],
            insert_index: 0,
            closed_workspace_id: Some("ws_1".into()),
        }),
    );

    assert!(repaint);
    assert!(actions.is_empty());
    let projection = state.snapshot.as_deref().expect("projection");
    assert!(projection
        .workspaces
        .iter()
        .all(|workspace| workspace.workspace_id == "ws_2"));
    assert_eq!(projection.focused_workspace_id.as_deref(), Some("ws_2"));
    assert_eq!(projection.focused_tab_id.as_deref(), Some("tab_1_2"));
    let tabs = projection
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.workspace_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(tabs, vec![("tab_1_2", "ws_2"), ("tab_2_2", "ws_2")]);
    assert!(projection
        .panes
        .iter()
        .all(|pane| pane.workspace_id == "ws_2"));
}
