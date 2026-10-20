use super::*;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn state_with_two_machines() -> (ClientShellState, ClientEndpointId) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile id"),
        label: "hellgate".into(),
        target: "dev@hellgate.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online, None);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.workspaces[0].label = "remote-space".into();
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    (state, endpoint_id)
}

/// The legacy nested layout with both machines folded, for the fitted-sizing tests.
fn state_with_two_collapsed_machines() -> ClientShellState {
    let (mut state, endpoint_id) = state_with_two_machines();
    state.config.machines_nest_spaces = true;
    state.collapsed_endpoints.insert(ClientEndpointId::Local);
    state.collapsed_endpoints.insert(endpoint_id);
    state
}

fn machine_rect(state: &ClientShellState, endpoint_id: &ClientEndpointId) -> Rect {
    state
        .hits
        .machines
        .iter()
        .find(|hit| &hit.endpoint_id == endpoint_id)
        .expect("machine hit")
        .rect
}

fn click_at(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn sidebar_rows(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(100, 30).expect("machines sidebar frame");
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .take(25)
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn collapsed_machines_shrink_the_section_to_its_content() {
    let mut state = state_with_two_collapsed_machines();
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[0], " ▾ machines");
    assert_eq!(rows[6], "─".repeat(25));
    assert!(rows[7].starts_with(" agents"));
    assert_eq!(state.hits.sidebar_section_divider.y, 6);
}

#[test]
fn expanding_a_machine_grows_the_fitted_section() {
    let mut state = state_with_two_collapsed_machines();
    sidebar_rows(&mut state);
    let fitted = state.hits.sidebar_section_divider.y;
    state.collapsed_endpoints.remove(&ClientEndpointId::Local);
    sidebar_rows(&mut state);
    assert!(
        state.hits.sidebar_section_divider.y > fitted,
        "expanded machine rows must claim more sidebar"
    );
}

#[test]
fn a_dragged_divider_still_wins_over_fitting() {
    let mut state = state_with_two_collapsed_machines();
    sidebar_rows(&mut state);
    let divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: divider.x + 2,
        row: divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: divider.x + 2,
        row: 15,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split_manual);
    sidebar_rows(&mut state);
    assert_eq!(state.hits.sidebar_section_divider.y, 15);
}

#[test]
fn clicking_the_machines_header_collapses_the_whole_section() {
    let mut state = state_with_two_collapsed_machines();
    sidebar_rows(&mut state);
    let header = state.hits.machines_section_toggle;
    assert_eq!(header.y, 0);
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: header.x + 3,
            row: header.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.repaint);
    assert!(outcome.actions.is_empty());
    assert!(state.machines_section_collapsed);

    let rows = sidebar_rows(&mut state);
    assert!(rows[0].starts_with(" ▸ machines"));
    assert_eq!(rows[1], "─".repeat(25));
    assert!(rows[2].starts_with(" agents"));
    assert_eq!(state.hits.sidebar_section_divider, Rect::default());
    assert!(state.hits.machines.is_empty());
    assert!(!state.hits.machines_section_toggle.is_empty());
}

#[test]
fn the_machines_keybinding_toggles_the_section() {
    let mut state = state_with_two_collapsed_machines();
    sidebar_rows(&mut state);
    for expected in [true, false] {
        for key in [
            crate::input::TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            crate::input::TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()),
        ] {
            state.handle_raw_events(vec![RawInputEvent::Key(key)]);
        }
        assert_eq!(state.machines_section_collapsed, expected);
    }
}

#[test]
fn the_collapsed_machines_section_round_trips_through_preferences() {
    let path = std::env::temp_dir().join(format!(
        "herdr-machines-section-prefs-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.machines_section_collapsed = true;
    state.persist_chrome_preferences(&mut ClientShellInput::default());

    let reloaded = ClientShellState::new(
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone()),
    );
    assert!(reloaded.machines_section_collapsed);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn clicking_the_spaces_header_collapses_the_band() {
    let (mut state, _) = state_with_two_machines();
    sidebar_rows(&mut state);
    let header = state.hits.spaces_section_toggle;
    assert_eq!(header.y, 4);
    let outcome = click_at(&mut state, header.x + 3, header.y);
    assert!(outcome.repaint);
    assert!(outcome.actions.is_empty());
    assert!(state.spaces_section_collapsed);

    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[0], " ▾ machines");
    assert!(rows[1].contains("Local"), "{:?}", rows[1]);
    assert_eq!(rows[4], " ▸ spaces · Local");
    assert!(state.hits.workspaces.is_empty());
    assert!(!state.hits.machines.is_empty());
    assert!(!state.hits.spaces_section_toggle.is_empty());
    // The footer keeps the only new-space and menu entry points reachable while the band is folded.
    assert!(rows[5].starts_with(" new · Local"), "{:?}", rows[5]);
    assert_eq!(state.hits.new_workspace.y, 5);
    assert_eq!(state.hits.global_launcher.y, 5);
}

#[test]
fn the_spaces_keybinding_toggles_the_band() {
    let (mut state, _) = state_with_two_machines();
    sidebar_rows(&mut state);
    for expected in [true, false] {
        for key in [
            crate::input::TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            crate::input::TerminalKey::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        ] {
            state.handle_raw_events(vec![RawInputEvent::Key(key)]);
        }
        assert_eq!(state.spaces_section_collapsed, expected);
    }
}

#[test]
fn the_collapsed_spaces_band_round_trips_through_preferences() {
    let path = std::env::temp_dir().join(format!(
        "herdr-spaces-section-prefs-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.spaces_section_collapsed = true;
    state.persist_chrome_preferences(&mut ClientShellInput::default());

    let reloaded = ClientShellState::new(
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone()),
    );
    assert!(reloaded.spaces_section_collapsed);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn folding_both_bands_keeps_their_headers_and_the_footer() {
    let (mut state, _) = state_with_two_machines();
    state.machines_section_collapsed = true;
    state.spaces_section_collapsed = true;
    let rows = sidebar_rows(&mut state);
    assert!(rows[0].starts_with(" ▸ machines"), "{:?}", rows[0]);
    assert_eq!(rows[1], " ▸ spaces · Local");
    assert!(rows[2].starts_with(" new · Local"), "{:?}", rows[2]);
    assert_eq!(rows[3], "─".repeat(25));
    assert!(rows[4].starts_with(" agents"), "{:?}", rows[4]);
    assert!(state.hits.machines.is_empty());
    assert!(state.hits.workspaces.is_empty());
    assert_eq!(state.hits.new_workspace.y, 2);
    assert_eq!(state.hits.global_launcher.y, 2);
    assert_eq!(state.hits.sidebar_section_divider.y, 3);
}

#[test]
fn the_spaces_toggle_does_nothing_in_the_nested_layout() {
    let (mut state, _) = state_with_two_machines();
    state.config.machines_nest_spaces = true;
    let before = sidebar_rows(&mut state);
    assert!(state.hits.spaces_section_toggle.is_empty());
    let mut outcome = ClientShellInput::default();
    state.toggle_spaces_section(&mut outcome);
    assert!(!state.spaces_section_collapsed);
    assert!(!outcome.repaint);
    assert_eq!(sidebar_rows(&mut state), before);
}

fn state_with_one_machine(
    status: ClientEndpointStatus,
    next_attempt_at: Option<std::time::Instant>,
) -> (ClientShellState, ClientEndpointId) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile id"),
        label: "hellgate".into(),
        target: "dev@hellgate.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, status, next_attempt_at);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    (state, endpoint_id)
}

fn machine_row_text(state: &mut ClientShellState, endpoint_id: &ClientEndpointId) -> String {
    let frame = state.compose(100, 30).expect("machines sidebar frame");
    let buffer = frame.to_ratatui_buffer().expect("frame buffer");
    let rect = state
        .hits
        .machines
        .iter()
        .find(|hit| &hit.endpoint_id == endpoint_id)
        .expect("machine hit")
        .rect;
    (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn the_reconnect_glyph_marks_only_machines_with_a_pending_attempt() {
    for (status, expected) in [
        (ClientEndpointStatus::Online, false),
        (ClientEndpointStatus::Disabled, false),
        (ClientEndpointStatus::Attention, true),
        (ClientEndpointStatus::Reconnecting, true),
    ] {
        let (mut state, endpoint_id) = state_with_one_machine(status, None);
        let row = machine_row_text(&mut state, &endpoint_id);
        assert_eq!(row.contains('⟳'), expected, "{status:?}: {row}");
        assert_eq!(
            state
                .hits
                .machines
                .iter()
                .find(|hit| hit.endpoint_id == endpoint_id)
                .expect("machine hit")
                .reconnect_button
                .is_some(),
            expected,
            "{status:?}"
        );
    }
}

#[test]
fn a_parked_machine_row_counts_down_to_its_next_attempt() {
    let now = std::time::Instant::now();
    for (delay, expected) in [(240, "4m"), (45, "45s")] {
        let (mut state, endpoint_id) = state_with_one_machine(
            ClientEndpointStatus::Attention,
            Some(now + std::time::Duration::from_secs(delay)),
        );
        let row = machine_row_text(&mut state, &endpoint_id);
        assert!(
            row.contains(&format!("! attention · {expected}")),
            "{delay}s: {row}"
        );
    }
    let (mut state, endpoint_id) = state_with_one_machine(ClientEndpointStatus::Attention, None);
    let row = machine_row_text(&mut state, &endpoint_id);
    assert!(row.contains("! attention"), "{row}");
    assert!(!row.contains('·'), "{row}");
}

#[test]
fn the_machines_band_lists_machines_without_nesting_spaces() {
    let (mut state, remote_id) = state_with_two_machines();
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[0], " ▾ machines");
    assert!(rows[1].contains("Local"), "{:?}", rows[1]);
    assert!(rows[2].contains("hellgate"), "{:?}", rows[2]);
    for row in &rows[1..3] {
        assert!(!row.contains('▾') && !row.contains('▸'), "{row:?}");
    }
    assert_eq!(
        state
            .hits
            .machines
            .iter()
            .map(|hit| hit.rect.y)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(state
        .hits
        .machines
        .iter()
        .all(|hit| hit.collapse_toggle.is_empty()));
    assert!(state.hits.workspaces.iter().all(|hit| hit.rect.y > 2));
    assert!(!state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.endpoint_id == remote_id));
}

#[test]
fn the_spaces_band_follows_the_active_machine() {
    let (mut state, remote_id) = state_with_two_machines();
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[4], " ▾ spaces · Local");
    assert!(rows[5].contains("client-shell"), "{:?}", rows[5]);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.endpoint_id == ClientEndpointId::Local));
    assert!(!state.hits.workspaces.is_empty());

    assert!(state.activate_endpoint_projection(&remote_id));
    let mut remote_surface = surface();
    remote_surface.boot_id = "remote-boot".into();
    state.set_pane_surface(remote_surface);
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[4], " ▾ spaces · hellgate");
    assert!(rows[5].contains("remote-space"), "{:?}", rows[5]);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.endpoint_id == remote_id));
}

#[test]
fn the_collapsed_machines_band_keeps_the_spaces_band_visible() {
    let (mut state, _) = state_with_two_machines();
    state.machines_section_collapsed = true;
    let rows = sidebar_rows(&mut state);
    assert!(rows[0].starts_with(" ▸ machines"), "{:?}", rows[0]);
    assert!(state.hits.machines.is_empty());
    assert_eq!(rows[1], " ▾ spaces · Local");
    assert!(!state.hits.workspaces.is_empty());
}

#[test]
fn clicking_the_active_machine_row_re_activates_it_without_collapsing() {
    let (mut state, _) = state_with_two_machines();
    sidebar_rows(&mut state);
    let rect = machine_rect(&state, &ClientEndpointId::Local);
    let outcome = click_at(&mut state, rect.x + 3, rect.y);
    assert!(outcome.repaint);
    // Re-activating the machine on display is what cancels a switch waiting on another one.
    assert!(
        matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::ActivateEndpoint { endpoint_id, .. }]
                if endpoint_id == &ClientEndpointId::Local
        ),
        "{:?}",
        outcome.actions
    );
    assert!(state.collapsed_endpoints.is_empty());
}

#[test]
fn clicking_another_machine_row_still_activates_it() {
    let (mut state, remote_id) = state_with_two_machines();
    sidebar_rows(&mut state);
    let rect = machine_rect(&state, &remote_id);
    let outcome = click_at(&mut state, rect.x + 3, rect.y);
    assert!(
        matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::ActivateEndpoint { endpoint_id, .. }] if endpoint_id == &remote_id
        ),
        "{:?}",
        outcome.actions
    );
    assert!(state.collapsed_endpoints.is_empty());
}

#[test]
fn clicking_a_space_reaches_the_same_action_in_both_layouts() {
    for nested in [false, true] {
        let (mut state, _) = state_with_two_machines();
        state.config.machines_nest_spaces = nested;
        sidebar_rows(&mut state);
        let rect = state
            .hits
            .workspaces
            .iter()
            .find(|hit| hit.workspace_id == "ws_1" && hit.endpoint_id == ClientEndpointId::Local)
            .expect("local space hit")
            .rect;
        click_at(&mut state, rect.x + 3, rect.y);
        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: rect.x + 3,
                row: rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(
            matches!(
                outcome.actions.as_slice(),
                [ClientShellAction::ActivateEndpoint {
                    endpoint_id: ClientEndpointId::Local,
                    target: Some(ClientEndpointFocusTarget::Workspace(workspace_id)),
                }] if workspace_id == "ws_1"
            ),
            "nested {nested}: {:?}",
            outcome.actions
        );
    }
}

#[test]
fn nesting_spaces_under_machines_restores_the_legacy_layout() {
    let (mut state, remote_id) = state_with_two_machines();
    state.config.machines_nest_spaces = true;
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[0], " ▾ machines");
    assert!(rows[2].contains("▾ Local"), "{:?}", rows[2]);
    assert!(rows.iter().any(|row| row.contains("remote-space")));
    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.endpoint_id == remote_id));
    assert!(state
        .hits
        .machines
        .iter()
        .all(|hit| !hit.collapse_toggle.is_empty()));
}

/// Drag the section divider to `row` so the manual split ratio becomes active.
fn drag_section_divider(state: &mut ClientShellState, row: u16) {
    sidebar_rows(state);
    let divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: divider.x + 2,
        row: divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: divider.x + 2,
        row,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split_manual);
}

#[test]
fn folding_both_bands_frees_the_rows_a_dragged_divider_claimed() {
    let (mut state, _) = state_with_two_machines();
    drag_section_divider(&mut state, 15);
    state.machines_section_collapsed = true;
    state.spaces_section_collapsed = true;
    let rows = sidebar_rows(&mut state);
    assert!(rows[0].starts_with(" ▸ machines"), "{:?}", rows[0]);
    assert_eq!(rows[1], " ▸ spaces · Local");
    assert!(rows[2].starts_with(" new · Local"), "{:?}", rows[2]);
    assert_eq!(rows[3], "─".repeat(25));
    assert!(rows[4].starts_with(" agents"), "{:?}", rows[4]);
    assert_eq!(state.hits.sidebar_section_divider.y, 3);
}

#[test]
fn folding_only_the_spaces_band_frees_rows_despite_a_dragged_divider() {
    let (mut state, _) = state_with_two_machines();
    drag_section_divider(&mut state, 15);
    state.spaces_section_collapsed = true;
    let rows = sidebar_rows(&mut state);
    assert_eq!(rows[0], " ▾ machines");
    assert!(rows[1].contains("Local"), "{:?}", rows[1]);
    assert!(rows[2].contains("hellgate"), "{:?}", rows[2]);
    assert_eq!(rows[4], " ▸ spaces · Local");
    assert!(rows[5].starts_with(" new · Local"), "{:?}", rows[5]);
    assert_eq!(rows[6], "─".repeat(25));
    assert!(state.hits.workspaces.is_empty());
    assert_eq!(state.hits.sidebar_section_divider.y, 6);
}

#[test]
fn unfolding_both_bands_restores_the_dragged_divider() {
    let (mut state, _) = state_with_two_machines();
    drag_section_divider(&mut state, 15);
    sidebar_rows(&mut state);
    assert_eq!(state.hits.sidebar_section_divider.y, 15);
    let ratio = state.sidebar_section_split;

    state.machines_section_collapsed = true;
    state.spaces_section_collapsed = true;
    sidebar_rows(&mut state);
    assert!(state.hits.sidebar_section_divider.y < 15);
    assert_eq!(state.sidebar_section_split, ratio);

    state.machines_section_collapsed = false;
    state.spaces_section_collapsed = false;
    sidebar_rows(&mut state);
    assert!(state.sidebar_section_split_manual);
    assert_eq!(state.sidebar_section_split, ratio);
    assert_eq!(state.hits.sidebar_section_divider.y, 15);
}

#[test]
fn the_band_spacer_follows_the_machines_fold() {
    let (mut state, _) = state_with_two_machines();
    let rows = sidebar_rows(&mut state);
    // The spacer separates the machine rows from the spaces header while the band is unfolded.
    assert_eq!(rows[3], "");
    assert_eq!(rows[4], " ▾ spaces · Local");

    state.machines_section_collapsed = true;
    let rows = sidebar_rows(&mut state);
    assert!(rows[0].starts_with(" ▸ machines"), "{:?}", rows[0]);
    assert_eq!(rows[1], " ▾ spaces · Local");
}
