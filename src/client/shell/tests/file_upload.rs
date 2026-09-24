use super::*;
use crate::client::endpoint::{ClientEndpointId, ClientEndpointStatus};
use crate::client::file_collect::collect;
use crate::client::shell::file_upload::quote_upload_path;

fn scratch_file(label: &str, data: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-upload-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("payload.bin");
    std::fs::write(&path, data).unwrap();
    path
}

fn shell_with_selection(path: &std::path::Path) -> (ClientShellState, String) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let boot_id = state.snapshot.as_ref().unwrap().boot_id.clone();
    let mut outcome = ClientShellInput::default();
    state.open_file_upload(collect(&[path.to_path_buf()]), &mut outcome);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileUpload(_))
    ));
    (state, boot_id)
}

#[test]
fn a_directory_entry_needs_no_chunk_and_the_next_entry_starts_immediately() {
    // tree/ with one file inside: entries are [tree (dir), tree/a.txt (file)].
    let root = scratch_file("tree", b"x")
        .parent()
        .expect("scratch parent")
        .join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"abcd").unwrap();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let boot_id = state.snapshot.as_ref().unwrap().boot_id.clone();
    let mut outcome = ClientShellInput::default();
    state.open_file_upload(collect(std::slice::from_ref(&root)), &mut outcome);

    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected the directory begin");
    };
    let crate::api::schema::Method::FilePutBegin(params) = &request.method else {
        panic!("expected file.put.begin");
    };
    assert_eq!(
        params.entry_kind,
        crate::api::schema::FilePutEntryKind::Directory
    );
    assert_eq!(params.bytes, 0);
    assert_eq!(params.relative_path.as_deref(), Some("tree"));
    let directory_request_id = request.id.clone();

    // A complete answer must produce the next entry's begin, not a chunk.
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &directory_request_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: true,
            path: "/home/tester/herdr-inbox/tree".into(),
        }),
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected the next entry's begin");
    };
    let crate::api::schema::Method::FilePutBegin(params) = &request.method else {
        panic!("expected file.put.begin, not a chunk");
    };
    assert_eq!(
        params.entry_kind,
        crate::api::schema::FilePutEntryKind::File
    );
    assert_eq!(params.relative_path.as_deref(), Some("tree/a.txt"));
    assert_eq!(params.bytes, 4);
}

#[test]
fn a_transfer_walks_begin_chunk_commit_in_order() {
    let path = scratch_file("order", &[7u8; 20]);
    let (mut state, boot_id) = shell_with_selection(&path);

    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected file.put.begin");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::FilePutBegin(_)
    ));
    let begin_id = request.id.clone();

    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected the first chunk");
    };
    let crate::api::schema::Method::FilePutChunk(params) = &request.method else {
        panic!("expected file.put.chunk");
    };
    assert_eq!(params.offset, 0);
    assert_eq!(params.transfer_id, "ft-1-1");
    let mut request_id = request.id.clone();

    for expected_next in [8u64, 16, 20] {
        let (_, actions) = state.handle_endpoint_result(
            &boot_id,
            &request_id,
            Ok(crate::api::schema::ResponseResult::FilePutChunkAccepted {
                transfer_id: "ft-1-1".into(),
                next_offset: expected_next,
            }),
        );
        let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
            panic!("expected another endpoint request");
        };
        request_id = request.id.clone();
        if expected_next == 20 {
            assert!(matches!(
                request.method,
                crate::api::schema::Method::FilePutCommit(_)
            ));
        } else {
            let crate::api::schema::Method::FilePutChunk(params) = &request.method else {
                panic!("expected file.put.chunk");
            };
            assert_eq!(params.offset, expected_next);
        }
    }
}

#[test]
fn the_chunk_loop_commits_at_the_size_declared_at_begin_not_the_walk_time_size() {
    // Walked at 20 bytes, then shrunk to 8 before `start_file_upload` reads it for begin: the
    // declared size (8, from `hash_file` at send time) must drive the loop, not the stale
    // walk-time `CollectedEntry::bytes` (20).
    let path = scratch_file("resized", &[9u8; 20]);
    let (mut state, boot_id) = shell_with_selection(&path);
    std::fs::write(&path, [3u8; 8]).unwrap();

    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected file.put.begin");
    };
    let crate::api::schema::Method::FilePutBegin(params) = &request.method else {
        panic!("expected file.put.begin");
    };
    assert_eq!(
        params.bytes, 8,
        "begin must declare the size read at send time, not the walk-time size"
    );
    let begin_id = request.id.clone();

    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected the first chunk");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::FilePutChunk(_)
    ));
    let request_id = request.id.clone();

    // next_offset lands exactly on the declared size: the loop must commit here. With the
    // walk-time size (20) driving it instead, this would send another chunk toward 20.
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &request_id,
        Ok(crate::api::schema::ResponseResult::FilePutChunkAccepted {
            transfer_id: "ft-1-1".into(),
            next_offset: 8,
        }),
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected exactly one more endpoint request, the commit");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::FilePutCommit(_)
    ));
}

#[test]
fn an_absurd_server_chunk_size_is_clamped_before_the_client_allocates_it() {
    // `read_chunk` allocates a `chunk_bytes`-sized buffer and base64-encodes a copy of it, so an
    // unclamped `u32::MAX` here is a remote out-of-memory for this client.
    let path = scratch_file("chunk-clamp", &[4u8; 16]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: u32::MAX,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_ref() else {
        panic!("expected the upload overlay");
    };
    assert_eq!(
        upload.chunk_bytes, 700_000,
        "a server-named chunk size must be clamped to what a request line can carry"
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected the first chunk");
    };
    let crate::api::schema::Method::FilePutChunk(params) = &request.method else {
        panic!("expected file.put.chunk");
    };
    // The file is 16 bytes, so the clamp is invisible in the payload; the field is the evidence.
    assert_eq!(params.offset, 0);
}

#[test]
fn a_name_this_server_refuses_skips_that_entry_and_keeps_going() {
    // The server refuses a name (leading dot, too long, a component it will not write) with
    // `invalid_file_path`. That must cost one entry, not the whole selection: the remaining
    // files still go, the refused one is reported as skipped, and the overlay shows no error.
    let root = scratch_file("skip", b"x")
        .parent()
        .expect("scratch parent")
        .join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"aaaa").unwrap();
    std::fs::write(root.join("b.txt"), b"bbbb").unwrap();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let boot_id = state.snapshot.as_ref().unwrap().boot_id.clone();
    let mut outcome = ClientShellInput::default();
    state.open_file_upload(collect(std::slice::from_ref(&root)), &mut outcome);

    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    // Entry 0 is the directory itself; refuse it and the two files must still be attempted.
    let mut request_id = request_id(&outcome.actions).to_owned();
    let mut refused = 0usize;
    let mut begun_files = Vec::new();
    loop {
        let (_, actions) = state.handle_endpoint_result(
            &boot_id,
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("invalid_file_path".into()),
                message: "this file name is not allowed: leading dot".into(),
            }),
        );
        refused += 1;
        let Some(ClientShellAction::Endpoint { request, .. }) = actions.first() else {
            break;
        };
        let crate::api::schema::Method::FilePutBegin(params) = &request.method else {
            panic!("expected the next entry's begin, not a chunk");
        };
        begun_files.push(params.relative_path.clone().unwrap_or_default());
        request_id = request.id.clone();
    }

    assert_eq!(refused, 3, "every entry must get its own begin");
    begun_files.sort();
    assert_eq!(begun_files, ["tree/a.txt", "tree/b.txt"]);
    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_ref() else {
        panic!("expected the upload overlay to stay open");
    };
    assert!(upload.done, "the upload must finish rather than hang");
    assert_eq!(
        upload.error, None,
        "a refused name is not an upload-wide failure"
    );
    assert_eq!(upload.skipped.len(), 3, "{:?}", upload.skipped);
    assert!(upload
        .skipped
        .iter()
        .any(|entry| entry.contains("tree/a.txt")));
}

#[test]
fn cancelling_during_the_begin_round_trip_still_aborts_the_transfer() {
    // `cancel_file_upload` runs before any `transfer_id` exists, so it cannot abort. The begin
    // response then arrives with an id nothing owns; without an abort here the server keeps that
    // transfer and its temp file, and every later upload on this connection gets `transfer_busy`.
    let path = scratch_file("cancel-race", &[2u8; 20]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();

    let mut outcome = ClientShellInput::default();
    state.cancel_file_upload(&mut outcome);
    assert!(state.overlay.is_none());
    assert!(
        outcome.actions.is_empty(),
        "there is no transfer id to abort yet"
    );

    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-7".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let [ClientShellAction::Endpoint { request, .. }] = &actions[..] else {
        panic!("expected file.put.abort for the orphaned transfer, got {actions:?}");
    };
    let crate::api::schema::Method::FilePutAbort(params) = &request.method else {
        panic!("expected file.put.abort");
    };
    assert_eq!(params.transfer_id, "ft-1-7");
}

#[test]
fn a_failed_chunk_surfaces_one_error_and_stops() {
    let path = scratch_file("failure", &[1u8; 4]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Err(ClientShellEndpointError {
            code: Some("destination_refused".into()),
            message: "this destination is not allowed: system directory".into(),
        }),
    );
    assert!(actions.is_empty());
    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_ref() else {
        panic!("expected the upload overlay to stay open");
    };
    assert!(!upload.running);
    assert_eq!(
        upload.error.as_deref(),
        Some("this destination is not allowed: system directory")
    );
}

#[test]
fn cancelling_a_running_transfer_sends_abort() {
    let path = scratch_file("cancel", &[1u8; 20]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let mut outcome = ClientShellInput::default();
    state.cancel_file_upload(&mut outcome);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected file.put.abort");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::FilePutAbort(_)
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn losing_the_active_surface_mid_transfer_fails_the_upload_instead_of_leaving_it_stuck() {
    // The endpoint lane requires an active, online surface for every command
    // (`push_endpoint_method_with_kind` checks `endpoint_is_online` before sending). If that
    // stops being true between two chunks of the same still-active endpoint — the connection
    // drops, the surface deactivates — the send is refused with no request ever queued and thus
    // nothing left to time out later. The loop must not just walk away leaving `running: true`
    // with no pending activity and no way out but manual cancel.
    let path = scratch_file("surface-lost", &[5u8; 20]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 8,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    assert!(!actions.is_empty(), "expected the first chunk to be queued");

    // Take the active endpoint offline: the next send this drives (the following chunk, or the
    // commit once the loop reaches the end) must fail cleanly instead of vanishing silently.
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);

    let request_id = request_id(&actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &request_id,
        Ok(crate::api::schema::ResponseResult::FilePutChunkAccepted {
            transfer_id: "ft-1-1".into(),
            next_offset: 8,
        }),
    );
    assert!(
        actions.is_empty(),
        "no request can be queued once the surface is offline"
    );

    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_ref() else {
        panic!("expected the upload overlay to stay open with an error");
    };
    assert!(
        !upload.running,
        "must not stay running with nothing pending"
    );
    assert!(upload.error.is_some(), "must surface a clear error");
}

#[test]
fn the_overlay_shows_the_manifest_then_progress() {
    let path = scratch_file("render", &[0u8; 12]);
    let (mut state, boot_id) = shell_with_selection(&path);

    let frame = render_shell_frame(&mut state);
    assert!(frame.contains("1 file"), "manifest missing:\n{frame}");
    assert!(
        frame.contains("herdr-inbox") || frame.contains("inbox"),
        "{frame}"
    );

    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 4,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let frame = render_shell_frame(&mut state);
    assert!(frame.contains("sending"), "progress missing:\n{frame}");
}

#[test]
fn the_manifest_counts_are_computed_once_and_not_recounted_per_frame() {
    // Every chunk response repaints this overlay, so the render path must not walk `entries`.
    // Appending an entry after the counts were taken proves the render reads the stored counts:
    // if it recounted, the manifest would follow the appended entry.
    let path = scratch_file("counts", &[0u8; 8]);
    let (mut state, _boot_id) = shell_with_selection(&path);
    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_mut() else {
        panic!("expected the upload overlay");
    };
    assert_eq!((upload.file_count, upload.directory_count), (1, 0));
    let extra = upload.entries[0].clone();
    upload.entries.push(extra);

    let frame = render_shell_frame(&mut state);
    assert!(
        frame.contains("1 file(s)"),
        "the manifest must use the counts taken at open:\n{frame}"
    );
}

#[test]
fn esc_cancels_and_tab_toggles_the_destination() {
    let path = scratch_file("keys", &[0u8; 4]);
    let (mut state, _boot_id) = shell_with_selection(&path);
    state.handle_input_bytes(b"\t");
    let Some(ClientShellOverlay::FileUpload(upload)) = state.overlay.as_ref() else {
        panic!("overlay closed");
    };
    assert_eq!(
        upload.destination,
        crate::api::schema::FilePutDestination::PaneCwd
    );
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
}

#[test]
fn the_send_files_binding_asks_for_a_file_chooser() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::SendFiles),
        &mut outcome,
    );
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::ChooseFiles]
    ));
}

#[test]
fn send_files_defaults_to_prefix_u() {
    let keybinds = crate::config::Config::default().keybinds();
    assert_eq!(keybinds.send_files.labels(), vec!["prefix+u".to_owned()]);
}

fn finish_upload(pane_runs_agent: bool) -> (ClientShellState, Vec<ClientShellAction>) {
    finish_upload_with(pane_runs_agent, |_| {})
}

fn finish_upload_with(
    pane_runs_agent: bool,
    before_commit: impl FnOnce(&mut ClientShellState),
) -> (ClientShellState, Vec<ClientShellAction>) {
    let path = scratch_file("finish", &[1u8; 4]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 4,
            destination_label: "/home/tester/herdr-inbox".into(),
            complete: false,
            path: String::new(),
        }),
    );
    // The declared 4-byte file fits in one 4-byte chunk, so this chunk's `next_offset` lands
    // exactly on the declared size and the loop sends the commit next.
    let chunk_id = request_id(&actions).to_owned();
    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &chunk_id,
        Ok(crate::api::schema::ResponseResult::FilePutChunkAccepted {
            transfer_id: "ft-1-1".into(),
            next_offset: 4,
        }),
    );
    let commit_id = request_id(&actions).to_owned();

    if pane_runs_agent {
        let focused_pane_id = state
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.focused_pane_id.clone())
            .expect("focused pane id");
        if let Some(snapshot) = state.snapshot.as_mut() {
            snapshot.agents.push(crate::protocol::ClientShellAgent {
                pane_id: focused_pane_id,
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: None,
                display_agent: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: crate::api::schema::AgentStatus::Idle,
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: true,
            });
        }
    }
    before_commit(&mut state);

    let (_, actions) = state.handle_endpoint_result(
        &boot_id,
        &commit_id,
        Ok(crate::api::schema::ResponseResult::FilePutCommitted {
            transfer_id: "ft-1-1".into(),
            path: "/home/tester/herdr-inbox/payload.bin".into(),
            bytes: 4,
        }),
    );
    (state, actions)
}

#[test]
fn a_hostile_server_path_is_never_pasted() {
    for hostile in [
        "/home/t/a\nrm -rf /",
        "/home/t/a\x1b[201~; id",
        "/home/t/a\r",
        "/home/t/a\u{0}b",
    ] {
        assert_eq!(quote_upload_path(hostile), None, "{hostile:?} was accepted");
    }
    assert_eq!(
        quote_upload_path("/home/t/herdr-inbox/report 2026.txt").as_deref(),
        Some("'/home/t/herdr-inbox/report 2026.txt'")
    );
    assert_eq!(
        quote_upload_path("/home/t/herdr-inbox/report.txt").as_deref(),
        Some("/home/t/herdr-inbox/report.txt")
    );
    assert_eq!(
        quote_upload_path("/home/t/a'b.txt").as_deref(),
        Some("'/home/t/a'\\''b.txt'")
    );
}

#[test]
fn quote_upload_path_neutralizes_shell_metacharacters_via_single_quoting() {
    for (path, expected) in [
        ("/home/t/a\"b.txt", "'/home/t/a\"b.txt'"),
        ("/home/t/$(rm -rf /).txt", "'/home/t/$(rm -rf /).txt'"),
        ("/home/t/`id`.txt", "'/home/t/`id`.txt'"),
        ("/home/t/héllo/résumé.txt", "'/home/t/héllo/résumé.txt'"),
        ("   ", "'   '"),
    ] {
        assert_eq!(
            quote_upload_path(path).as_deref(),
            Some(expected),
            "{path:?} was not safely single-quoted"
        );
    }
    // A leading dash carries no shell meaning here: this is a pasted string, not an argv
    // element, so nothing downstream can parse it as an option flag.
    assert_eq!(
        quote_upload_path("/home/t/-rf.txt").as_deref(),
        Some("/home/t/-rf.txt")
    );
}

#[test]
fn a_finished_transfer_pastes_into_a_shell_pane() {
    let (state, actions) = finish_upload(false);
    assert!(matches!(
        &actions[..],
        [ClientShellAction::PastePane { text, .. }]
            if text == "/home/tester/herdr-inbox/payload.bin"
    ));
    assert!(state.overlay.is_some());
}

#[test]
fn the_paste_names_the_endpoint_that_committed_not_whichever_is_active_later() {
    // Pane ids are per-server. Switching machines between the commit and the action being drained
    // must not send the paste to an unrelated pane on the newly active machine.
    let profile_id = crate::client::endpoint::ProfileId::generate();
    let (_state, actions) = finish_upload_with(false, |state| {
        state.active_endpoint_id = ClientEndpointId::Ssh(profile_id.clone());
    });
    let [ClientShellAction::PastePane {
        endpoint_id,
        boot_id,
        ..
    }] = &actions[..]
    else {
        panic!("expected one paste action, got {actions:?}");
    };
    assert_eq!(
        endpoint_id,
        &ClientEndpointId::Local,
        "the paste must go to the endpoint that received the file"
    );
    assert!(
        !boot_id.is_empty(),
        "the paste must carry the boot it committed against"
    );
}

#[test]
fn a_finished_transfer_copies_instead_of_pasting_into_an_agent_pane() {
    let (mut state, actions) = finish_upload(true);
    assert!(matches!(
        &actions[..],
        [ClientShellAction::ClipboardWrite(bytes)]
            if bytes == b"/home/tester/herdr-inbox/payload.bin"
    ));
    // Nothing appears in the pane on this path, so the dialog has to show where the file landed.
    let frame = render_shell_frame(&mut state);
    assert!(
        frame.contains("/home/tester/herdr-inbox/payload.bin"),
        "the landed path must be shown when it is only copied:\n{frame}"
    );
}

#[test]
fn the_overlay_shows_the_destination_this_server_reported() {
    // A server with a configured inbox writes somewhere other than ~/herdr-inbox; showing the
    // hard-coded default would tell the user the wrong place.
    let path = scratch_file("label", &[0u8; 4]);
    let (mut state, boot_id) = shell_with_selection(&path);
    let mut outcome = ClientShellInput::default();
    state.start_file_upload(&mut outcome);
    let begin_id = request_id(&outcome.actions).to_owned();
    state.handle_endpoint_result(
        &boot_id,
        &begin_id,
        Ok(crate::api::schema::ResponseResult::FilePutBegan {
            transfer_id: "ft-1-1".into(),
            chunk_bytes: 4,
            // Control characters in a server string must never reach the frame.
            destination_label: "/srv/drop\u{1b}[201~box".into(),
            complete: false,
            path: String::new(),
        }),
    );
    let frame = render_shell_frame(&mut state);
    assert!(
        frame.contains("/srv/drop"),
        "the server's destination must be shown:\n{frame}"
    );
    assert!(
        !frame.contains('\u{1b}'),
        "a control character from the server reached the frame:\n{frame}"
    );
}
