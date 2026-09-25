//! Integration test over the endpoint lane: drives real `file.put.*` requests through a real
//! server socket and checks the files that land on disk.

#![cfg(unix)]

pub mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::json;

#[test]
fn a_file_rides_the_endpoint_lane_into_the_inbox() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let boot_id = harness.boot_id(&mut stream);

    let data = b"integration payload".to_vec();
    let sha256 = support::sha256_hex(&data);
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "t1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "landed.txt",
                "bytes": data.len(),
                "sha256": sha256,
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "t2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    let accepted = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(accepted["result"]["next_offset"], data.len());

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "t3",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let committed = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let landed = committed["result"]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(landed).unwrap(), data);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(landed).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "an uploaded file must not be group- or world-readable"
        );
    }
}

#[test]
fn a_file_rides_the_endpoint_lane_into_a_panes_working_directory() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let (boot_id, pane_id) = harness.boot_id_and_focused_pane(&mut stream);

    let data = b"pane cwd payload".to_vec();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "p1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "landed-pane.txt",
                "bytes": data.len(),
                "sha256": support::sha256_hex(&data),
                "destination": "pane_cwd",
                "pane_id": pane_id
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "p2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "p3",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let committed = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let landed = committed["result"]["path"].as_str().unwrap();

    assert_eq!(std::fs::read(landed).unwrap(), data);
    assert_eq!(
        std::fs::canonicalize(Path::new(landed).parent().unwrap()).unwrap(),
        std::fs::canonicalize(harness.pane_cwd()).unwrap(),
        "a pane_cwd upload must land in the pane's own launch directory, not the inbox: {landed}"
    );
    assert!(
        !Path::new(landed).starts_with(harness.inbox()),
        "a pane_cwd upload must not land in the inbox: {landed}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(landed).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "an uploaded file must not be group- or world-readable"
        );
    }
}

#[test]
fn an_empty_directory_arrives_empty_and_a_nested_tree_keeps_its_shape() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let boot_id = harness.boot_id(&mut stream);
    let empty_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // The client's collector order: every directory before anything inside it.
    for (index, relative) in ["tree", "tree/empty", "tree/nested"].iter().enumerate() {
        support::send_endpoint_request(
            &mut stream,
            &boot_id,
            &json!({
                "id": format!("d{index}"),
                "method": "file.put.begin",
                "params": {
                    "suggested_name": relative.rsplit('/').next().unwrap(),
                    "relative_path": relative,
                    "entry_kind": "directory",
                    "bytes": 0,
                    "sha256": empty_sha256,
                    "destination": "inbox"
                }
            }),
        )
        .unwrap();
        let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
        assert_eq!(
            began["result"]["complete"], true,
            "a directory entry must finish at begin: {began}"
        );
        assert_eq!(
            began["result"]["path"],
            harness
                .inbox()
                .join(relative)
                .to_string_lossy()
                .into_owned(),
            "a directory entry must land at its relative path: {began}"
        );
    }

    // Re-sending a directory that already exists reuses it instead of creating `tree-2`.
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "d-again",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "tree",
                "relative_path": "tree",
                "entry_kind": "directory",
                "bytes": 0,
                "sha256": empty_sha256,
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let again = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(
        again["result"]["path"],
        harness.inbox().join("tree").to_string_lossy().into_owned(),
        "an existing directory must be reused, not suffixed: {again}"
    );

    // One file inside the nested directory, to prove the tree is writable afterwards.
    let data = b"leaf".to_vec();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "d3",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "leaf.txt",
                "relative_path": "tree/nested/leaf.txt",
                "entry_kind": "file",
                "bytes": data.len(),
                "sha256": support::sha256_hex(&data),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(began["result"]["complete"], false);
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "d4",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "d5",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    let inbox = harness.inbox();
    assert!(inbox.join("tree/empty").is_dir());
    assert_eq!(
        std::fs::read_dir(inbox.join("tree/empty")).unwrap().count(),
        0,
        "no marker file may be left in an empty directory"
    );
    assert_eq!(
        std::fs::read(inbox.join("tree/nested/leaf.txt")).unwrap(),
        data
    );
    assert!(
        !inbox.join("tree-2").exists(),
        "parents must not be duplicated"
    );
}

#[test]
fn a_surface_switch_mid_transfer_aborts_and_leaves_no_partial_file() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let boot_id = harness.boot_id(&mut stream);

    let data = vec![9u8; 64];
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "s1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "partial.bin",
                "bytes": data.len(),
                "sha256": support::sha256_hex(&data),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "s2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data[..32])
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "s3",
            "method": "client_shell.surface.set",
            "params": { "active": false }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "s4",
            "method": "client_shell.surface.set",
            "params": { "active": true }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "s5",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let refused = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(refused["error"]["code"], "transfer_not_found");
    assert_eq!(
        std::fs::read_dir(harness.inbox())
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

#[test]
fn a_second_connection_cannot_drive_the_first_connections_transfer() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream1 = harness.connect_client_shell();
    let boot_id1 = harness.boot_id(&mut stream1);
    let mut stream2 = harness.connect_client_shell();
    let boot_id2 = harness.boot_id(&mut stream2);

    let data = b"isolated".to_vec();
    support::send_endpoint_request(
        &mut stream1,
        &boot_id1,
        &json!({
            "id": "i1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "isolated.txt",
                "bytes": data.len(),
                "sha256": support::sha256_hex(&data),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream1, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();

    support::send_endpoint_request(
        &mut stream2,
        &boot_id2,
        &json!({
            "id": "i2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    let refused = support::read_endpoint_response(&mut stream2, Duration::from_secs(10)).unwrap();
    assert_eq!(
        refused["error"]["code"], "transfer_not_found",
        "a second connection must not be able to drive the first connection's transfer id: {refused}"
    );

    support::send_endpoint_request(
        &mut stream2,
        &boot_id2,
        &json!({
            "id": "i3",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let refused_commit =
        support::read_endpoint_response(&mut stream2, Duration::from_secs(10)).unwrap();
    assert_eq!(refused_commit["error"]["code"], "transfer_not_found");

    // Positive control: the same code for a transfer id that never existed, and then a successful
    // chunk on the owning connection. Without both, `transfer_not_found` above could be passing
    // for the wrong reason — a rejected request shape, or a transfer that was never really open.
    support::send_endpoint_request(
        &mut stream1,
        &boot_id1,
        &json!({
            "id": "i3b",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": "ft-nonexistent-0",
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    let bogus = support::read_endpoint_response(&mut stream1, Duration::from_secs(10)).unwrap();
    assert_eq!(
        bogus["error"]["code"], "transfer_not_found",
        "a malformed id on the owning connection must get the same code: {bogus}"
    );

    support::send_endpoint_request(
        &mut stream1,
        &boot_id1,
        &json!({
            "id": "i3c",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    let accepted = support::read_endpoint_response(&mut stream1, Duration::from_secs(10)).unwrap();
    assert_eq!(
        accepted["result"]["next_offset"],
        data.len(),
        "the owning connection must be able to drive its own transfer: {accepted}"
    );

    // Clean up on the owning connection so the transfer does not leak a temp file.
    support::send_endpoint_request(
        &mut stream1,
        &boot_id1,
        &json!({
            "id": "i4",
            "method": "file.put.abort",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream1, Duration::from_secs(10)).unwrap();
}

#[test]
fn wrong_offset_oversized_chunk_and_checksum_mismatch_each_get_their_own_error_code() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let boot_id = harness.boot_id(&mut stream);

    // A wrong offset.
    let data = b"twelve bytes".to_vec();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "offset.bin",
                "bytes": data.len(),
                "sha256": support::sha256_hex(&data),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 5,
                "data_b64": support::base64_standard(&data)
            }
        }),
    )
    .unwrap();
    let offset_error =
        support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(offset_error["error"]["code"], "transfer_offset_mismatch");
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e3",
            "method": "file.put.abort",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    // An oversized chunk (this harness configures `file_chunk_bytes = 64`).
    let big = vec![7u8; 65];
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e4",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "oversized.bin",
                "bytes": big.len(),
                "sha256": support::sha256_hex(&big),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e5",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&big)
            }
        }),
    )
    .unwrap();
    let chunk_error =
        support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(chunk_error["error"]["code"], "transfer_chunk_too_large");
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e6",
            "method": "file.put.abort",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();

    // A checksum mismatch.
    let payload = b"checksum data".to_vec();
    let wrong_sha256 = support::sha256_hex(b"not the bytes that will be sent");
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e7",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "checksum.bin",
                "bytes": payload.len(),
                "sha256": wrong_sha256,
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e8",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&payload)
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "e9",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let checksum_error =
        support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    assert_eq!(
        checksum_error["error"]["code"],
        "transfer_checksum_mismatch"
    );

    assert_ne!(offset_error["error"]["code"], chunk_error["error"]["code"]);
    assert_ne!(
        chunk_error["error"]["code"],
        checksum_error["error"]["code"]
    );
    assert_ne!(
        offset_error["error"]["code"],
        checksum_error["error"]["code"]
    );
}

#[test]
fn uploading_the_same_leaf_name_twice_lands_the_second_under_a_numbered_suffix() {
    let harness = support::spawn_server_with_file_inbox();
    let mut stream = harness.connect_client_shell();
    let boot_id = harness.boot_id(&mut stream);

    let first = b"first upload".to_vec();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c1",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "dup.txt",
                "bytes": first.len(),
                "sha256": support::sha256_hex(&first),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id = began["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c2",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id,
                "offset": 0,
                "data_b64": support::base64_standard(&first)
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c3",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id }
        }),
    )
    .unwrap();
    let first_committed =
        support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let first_path = first_committed["result"]["path"]
        .as_str()
        .unwrap()
        .to_owned();

    let second = b"second upload".to_vec();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c4",
            "method": "file.put.begin",
            "params": {
                "suggested_name": "dup.txt",
                "bytes": second.len(),
                "sha256": support::sha256_hex(&second),
                "destination": "inbox"
            }
        }),
    )
    .unwrap();
    let began2 = support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let transfer_id2 = began2["result"]["transfer_id"].as_str().unwrap().to_owned();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c5",
            "method": "file.put.chunk",
            "params": {
                "transfer_id": transfer_id2,
                "offset": 0,
                "data_b64": support::base64_standard(&second)
            }
        }),
    )
    .unwrap();
    support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    support::send_endpoint_request(
        &mut stream,
        &boot_id,
        &json!({
            "id": "c6",
            "method": "file.put.commit",
            "params": { "transfer_id": transfer_id2 }
        }),
    )
    .unwrap();
    let second_committed =
        support::read_endpoint_response(&mut stream, Duration::from_secs(10)).unwrap();
    let second_path = second_committed["result"]["path"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(
        second_path,
        harness.inbox().join("dup-2.txt").to_string_lossy()
    );
    assert_eq!(std::fs::read(&first_path).unwrap(), first);
    assert_eq!(std::fs::read(&second_path).unwrap(), second);
}
