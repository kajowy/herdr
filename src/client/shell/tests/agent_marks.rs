use super::*;

fn marked_agent(pane_id: &str, agent: Option<&str>) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: agent.map(str::to_owned),
        display_agent: None,
        agent: agent.map(str::to_owned),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: pane_id == "pane_1",
    }
}

fn state_with(config: Config, agents: Vec<ClientShellAgent>) -> ClientShellState {
    let mut snapshot = snapshot();
    for agent in &agents {
        if agent.pane_id != "pane_1" {
            let mut pane = snapshot.panes[0].clone();
            pane.pane_id = agent.pane_id.clone();
            pane.focused = false;
            snapshot.panes.push(pane);
        }
    }
    snapshot.agents = agents;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state
}

/// Sidebar content rows (left of the sidebar separator), trailing blanks trimmed.
fn sidebar_rows(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(106, 30).expect("sidebar frame");
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

#[test]
fn claude_agent_row_shows_its_mark_instead_of_its_name() {
    let mut state = state_with(
        Config::default(),
        vec![marked_agent("pane_1", Some("claude"))],
    );

    let rows = sidebar_rows(&mut state);

    assert!(rows.iter().any(|row| row.trim() == "✦"), "rows: {rows:#?}");
    assert!(
        !rows.iter().any(|row| row.contains("claude")),
        "rows: {rows:#?}"
    );
}

#[test]
fn two_agents_in_one_panel_show_their_own_marks() {
    let mut state = state_with(
        Config::default(),
        vec![
            marked_agent("pane_1", Some("claude")),
            marked_agent("pane_2", Some("codex")),
        ],
    );

    let rows = sidebar_rows(&mut state);

    assert!(rows.iter().any(|row| row.trim() == "✦"), "rows: {rows:#?}");
    assert!(rows.iter().any(|row| row.trim() == "◆"), "rows: {rows:#?}");
}

#[test]
fn agent_marks_stay_apart_from_every_status_indicator() {
    use crate::config::StatusIndicatorStyle;

    for agent in crate::detect::Agent::ALL {
        let mark = crate::ui::agent_mark(agent);
        for indicators in [StatusIndicatorStyle::Dots, StatusIndicatorStyle::Symbols] {
            for status in [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ] {
                assert_ne!(
                    mark,
                    status_icon(status, indicators),
                    "{mark} collides with a status indicator"
                );
            }
        }
    }
}

#[test]
fn a_row_whose_only_label_was_the_agent_name_falls_back_to_its_space() {
    let mut config = Config::default();
    config.ui.sidebar.agents.rows = vec![vec![
        crate::config::AgentSidebarToken::StateIcon,
        crate::config::AgentSidebarToken::Agent,
    ]];
    let mut state = state_with(config, vec![marked_agent("pane_1", Some("claude"))]);

    let rows = sidebar_rows(&mut state);

    assert!(
        rows.iter().any(|row| row.trim() == "○ ✦ client-shell"),
        "rows: {rows:#?}"
    );
}

#[test]
fn an_unknown_agent_keeps_its_textual_name() {
    let mut state = state_with(
        Config::default(),
        vec![marked_agent("pane_1", Some("mystery-harness"))],
    );

    let rows = sidebar_rows(&mut state);

    assert!(
        rows.iter().any(|row| row.contains("mystery-harness")),
        "rows: {rows:#?}"
    );
}

#[test]
fn grouped_panel_rows_show_agent_marks() {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Grouped;
    let mut state = state_with(config, vec![marked_agent("pane_1", Some("claude"))]);

    let rows = sidebar_rows(&mut state);

    assert!(
        rows.iter().any(|row| row == "  ○ ✦ client-shell"),
        "rows: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("claude")),
        "rows: {rows:#?}"
    );
}

#[test]
fn endpoint_agent_panel_rows_show_agent_marks() {
    use crate::client::endpoint::ProfileId;

    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    let mut state = state_with(
        Config::default(),
        vec![marked_agent("pane_1", Some("codex"))],
    );
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online, None);
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.agents = vec![marked_agent("pane_9", Some("claude"))];
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));

    let rows = sidebar_rows(&mut state);

    assert!(rows.iter().any(|row| row.trim() == "◆"), "rows: {rows:#?}");
    assert!(rows.iter().any(|row| row.trim() == "✦"), "rows: {rows:#?}");
    assert!(
        !rows.iter().any(|row| row.contains("claude")),
        "rows: {rows:#?}"
    );
}
