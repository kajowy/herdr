use super::*;

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
    state.open_file_upload(&[path.to_path_buf()], &mut outcome);
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
    state.open_file_upload(std::slice::from_ref(&root), &mut outcome);

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
