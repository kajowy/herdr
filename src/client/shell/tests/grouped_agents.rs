use super::*;
use crate::client::endpoint::ProfileId;

fn shell_agent(
    pane_id: &str,
    workspace_id: &str,
    tab_id: &str,
    status: AgentStatus,
) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        name: Some("pi".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: pane_id == "pane_1",
    }
}

/// Two spaces with agents plus one without: `client-shell` has one agent in its
/// only tab, `second` has a blocked and a working agent in two tabs.
fn grouped_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    for (workspace_id, label) in [("ws_2", "second"), ("ws_3", "empty")] {
        let mut workspace = snapshot.workspaces[0].clone();
        workspace.workspace_id = workspace_id.into();
        workspace.label = label.into();
        workspace.focused = false;
        snapshot.workspaces.push(workspace);
    }
    for (tab_id, label) in [("tab_2", "build"), ("tab_3", "review")] {
        let mut tab = snapshot.tabs[0].clone();
        tab.tab_id = tab_id.into();
        tab.workspace_id = "ws_2".into();
        tab.label = label.into();
        tab.focused = false;
        snapshot.tabs.push(tab);
    }
    snapshot.agents = vec![
        shell_agent("pane_1", "ws_1", "tab_1", AgentStatus::Idle),
        shell_agent("pane_2", "ws_2", "tab_2", AgentStatus::Blocked),
        shell_agent("pane_3", "ws_2", "tab_3", AgentStatus::Working),
    ];
    snapshot
}

fn grouped_state() -> ClientShellState {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Grouped;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(grouped_snapshot()));
    state.set_pane_surface(surface());
    state
}

/// Sidebar content rows (left of the sidebar separator), trailing blanks trimmed.
fn sidebar_rows(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(106, 30).expect("grouped sidebar frame");
    let width = state.hits.sidebar_divider.x as usize;
    frame_rows(&frame)
        .into_iter()
        .map(|row| {
            row.chars()
                .take(width)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn click(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

#[test]
fn grouped_agent_panel_renders_tree_and_hides_spaces_list() {
    let mut state = grouped_state();
    let rows = sidebar_rows(&mut state);

    assert!(rows[0].starts_with(" new"), "rows: {rows:#?}");
    assert!(rows[0].ends_with("menu"), "rows: {rows:#?}");
    assert!(rows[1].starts_with(" agents"), "rows: {rows:#?}");
    assert!(rows[1].ends_with("grouped"), "rows: {rows:#?}");
    assert_eq!(
        &rows[2..8],
        [
            "▾ client-shell (1) ○",
            "  ○ pi",
            "",
            "▾ second (2) ●",
            "  ● build · pi",
            "  ● review · pi",
        ],
        "rows: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("empty")),
        "rows: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("spaces")),
        "rows: {rows:#?}"
    );
    assert!(state.hits.workspaces.is_empty());
    assert_eq!(state.hits.sidebar_section_divider, Rect::default());
}

#[test]
fn grouped_space_rollup_prefers_working_over_blocked() {
    let mut state = grouped_state();
    let frame = state.compose(106, 30).expect("grouped sidebar frame");
    let header = state.hits.agent_groups[1].0;
    let (x, y) = cell_symbol_position(&frame, header, "●");
    let buffer = frame.to_ratatui_buffer().expect("frame should reconstruct");
    assert_eq!(
        buffer[(x, y)].fg,
        state.config.palette.yellow,
        "working (yellow) outranks blocked (red) in the space rollup"
    );
}

#[test]
fn grouped_header_new_and_menu_buttons_share_the_top_row() {
    let mut state = grouped_state();
    state.compose(106, 30).expect("grouped sidebar frame");
    let top = state.hits.sidebar_divider.y;
    assert_eq!(state.hits.new_workspace.y, top);
    assert_eq!(state.hits.global_launcher.y, top);
    assert!(state.hits.new_workspace.right() <= state.hits.global_launcher.x);

    state.config.prompt_new_workspace_name = false;
    let new = state.hits.new_workspace;
    let create = click(&mut state, new.x, new.y);
    assert!(matches!(
        create.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::WorkspaceCreate(_))
    ));

    let menu = state.hits.global_launcher;
    click(&mut state, menu.x, menu.y);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::GlobalMenu(_))
    ));
}

#[test]
fn clicking_grouped_space_header_toggles_collapse() {
    let mut state = grouped_state();
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[2], "▾ client-shell (1) ○");
    let header = state.hits.agent_groups[0].0;

    let collapse = click(&mut state, header.x, header.y);
    assert!(collapse.actions.is_empty());
    assert!(state.group_is_collapsed(
        &ClientEndpointId::Local,
        &super::super::agent_sidebar::agent_group_key("ws_1")
    ));
    let rows = sidebar_rows(&mut state);
    assert_eq!(
        &rows[2..5],
        ["▸ client-shell (1) ○", "", "▾ second (2) ●"],
        "rows: {rows:#?}"
    );

    let header = state.hits.agent_groups[0].0;
    click(&mut state, header.x, header.y);
    let rows = sidebar_rows(&mut state);
    assert_eq!(
        &rows[2..4],
        ["▾ client-shell (1) ○", "  ○ pi"],
        "rows: {rows:#?}"
    );
}

#[test]
fn clicking_grouped_agent_row_focuses_its_pane() {
    let mut state = grouped_state();
    state.compose(106, 30).expect("grouped sidebar frame");
    let (rect, _) = state
        .hits
        .agents
        .iter()
        .find(|(_, pane_id)| pane_id == "pane_3")
        .cloned()
        .expect("pane_3 agent row");

    let focus = click(&mut state, rect.x + 2, rect.y);
    let [ClientShellAction::Endpoint { request, .. }] = &focus.actions[..] else {
        panic!("agent row click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_3"
    ));
}

#[test]
fn grouped_mode_across_machines_labels_spaces_with_their_machine() {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Grouped;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
    let mut local = snapshot();
    local.workspaces[0].label = "app".into();
    local.agents = vec![shell_agent("pane_1", "ws_1", "tab_1", AgentStatus::Idle)];
    state.set_snapshot(Box::new(local));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.workspaces[0].label = "app".into();
    remote.agents = vec![shell_agent("pane_9", "ws_1", "tab_1", AgentStatus::Blocked)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));

    let rows = sidebar_rows(&mut state);
    assert!(
        rows.iter().any(|row| row == " \u{25be} machines"),
        "rows: {rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row == "▾ Local · app (1) ○"),
        "rows: {rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row == "▾ Build · app (1) ●"),
        "rows: {rows:#?}"
    );

    let (header, _, _) = state
        .hits
        .agent_groups
        .iter()
        .find(|(_, id, _)| *id == endpoint_id)
        .cloned()
        .expect("remote space header");
    click(&mut state, header.x, header.y);
    assert!(state.group_is_collapsed(
        &endpoint_id,
        &super::super::agent_sidebar::agent_group_key("ws_1")
    ));
    assert!(!state.group_is_collapsed(
        &ClientEndpointId::Local,
        &super::super::agent_sidebar::agent_group_key("ws_1")
    ));
}

#[test]
fn grouped_agent_reveal_scrolls_to_the_agent_line() {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Grouped;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(grouped_snapshot()));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.agents = vec![shell_agent("pane_9", "ws_1", "tab_1", AgentStatus::Blocked)];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));

    // Lines: client-shell header, pi; second header, two agents; Build header,
    // pane_9. Revealing pane_9 in two rows starts at the Build header.
    state.reveal_endpoint_agent(&endpoint_id, "pane_9", 2);
    assert_eq!(state.agent_scroll, 5);
}
