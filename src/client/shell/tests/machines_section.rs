use super::*;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn state_with_two_collapsed_machines() -> ClientShellState {
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
    state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
    state.collapsed_endpoints.insert(ClientEndpointId::Local);
    state.collapsed_endpoints.insert(endpoint_id);
    state
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
