//! End-to-end coverage for the `terminal.attach_stream` JSON socket API.
//!
//! Drives a real spawned server over its unix socket only, with no crate
//! internals, proving the whole path: request/response envelope, snapshot,
//! frame fan-out to the controller and an observer, input delivery, view-mode
//! rejection, controller takeover, and the unknown-terminal error.

#![cfg(unix)]

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, wait_for_socket,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-attach-stream-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket_path: &Path) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn send_json_request(socket_path: &Path, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    writeln!(stream, "{request}").unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();

    serde_json::from_str(&response).expect("response should be valid JSON")
}

/// Creates a workspace and returns its root pane's id and terminal id.
fn create_workspace_and_root_pane(socket_path: &Path, label: &str) -> (String, String) {
    let response = send_json_request(
        socket_path,
        &format!(
            "{{\"id\":\"ws_create\",\"method\":\"workspace.create\",\"params\":{{\"label\":\"{label}\"}}}}"
        ),
    );

    if response.get("error").is_some() {
        panic!("workspace.create failed: {response}");
    }

    let pane_id = response
        .pointer("/result/root_pane/pane_id")
        .and_then(Value::as_str)
        .expect("workspace.create should return root pane id")
        .to_string();

    let terminal_id = response
        .pointer("/result/root_pane/terminal_id")
        .and_then(Value::as_str)
        .expect("workspace.create should return root pane terminal id")
        .to_string();

    (pane_id, terminal_id)
}

fn pane_send_input(socket_path: &Path, pane_id: &str, text: &str) {
    let request = format!(
        "{{\"id\":\"send_input\",\"method\":\"pane.send_input\",\"params\":{{\"pane_id\":\"{pane_id}\",\"text\":\"{}\",\"keys\":[\"Enter\"]}}}}",
        text.replace('"', "\\\"")
    );
    let response = send_json_request(socket_path, &request);
    if response.get("error").is_some() {
        panic!("pane.send_input failed: {response}");
    }
}

fn pane_read_recent(socket_path: &Path, pane_id: &str, lines: usize) -> String {
    let response = send_json_request(
        socket_path,
        &format!(
            "{{\"id\":\"pane_read\",\"method\":\"pane.read\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"recent\",\"lines\":{lines}}}}}"
        ),
    );

    if response.get("error").is_some() {
        panic!("pane.read failed: {response}");
    }

    response
        .pointer("/result/read/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn pane_read_recent_contains(
    socket_path: &Path,
    pane_id: &str,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_read_recent(socket_path, pane_id, 200).contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A raw connection to `terminal.attach_stream`: the initial JSON request
/// line, its response, and every subsequent NDJSON stream event, all on the
/// same socket.
struct StreamConn {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl StreamConn {
    /// Opens the socket and sends a well-formed attach request, returning
    /// the connection plus the parsed response line.
    fn open(
        socket_path: &Path,
        terminal_id: &str,
        mode: &str,
        cols: u16,
        rows: u16,
    ) -> (Self, Value) {
        Self::open_with_takeover(socket_path, terminal_id, mode, cols, rows, false)
    }

    fn open_with_takeover(
        socket_path: &Path,
        terminal_id: &str,
        mode: &str,
        cols: u16,
        rows: u16,
        takeover: bool,
    ) -> (Self, Value) {
        let mut conn = Self::connect(socket_path);
        conn.send_line(&format!(
            "{{\"id\":\"s1\",\"method\":\"terminal.attach_stream\",\"params\":{{\"terminal_id\":\"{terminal_id}\",\"mode\":\"{mode}\",\"takeover\":{takeover},\"cols\":{cols},\"rows\":{rows}}}}}"
        ));
        let response = conn.read_line(Duration::from_secs(5));
        (conn, response)
    }

    /// Opens the socket and sends an attach request for a terminal id that
    /// does not exist on the server.
    fn open_unknown_terminal(socket_path: &Path, terminal_id: &str) -> (Self, Value) {
        let mut conn = Self::connect(socket_path);
        conn.send_line(&format!(
            "{{\"id\":\"s1\",\"method\":\"terminal.attach_stream\",\"params\":{{\"terminal_id\":\"{terminal_id}\"}}}}"
        ));
        let response = conn.read_line(Duration::from_secs(5));
        (conn, response)
    }

    fn connect(socket_path: &Path) -> Self {
        Self {
            stream: UnixStream::connect(socket_path).expect("should connect to API socket"),
            buf: Vec::new(),
        }
    }

    fn send_line(&mut self, json: &str) {
        self.stream.write_all(json.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
        self.stream.flush().unwrap();
    }

    fn send_input(&mut self, text: &str) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        self.send_line(&format!(r#"{{"type":"input","data":"{encoded}"}}"#));
    }

    /// Reads one NDJSON line, waiting up to `timeout`. Returns `None` on a
    /// timeout or if the peer closed the connection.
    fn try_read_line(&mut self, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        self.stream.set_nonblocking(true).unwrap();

        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                let text = String::from_utf8(line).expect("stream line should be utf8");
                self.stream.set_nonblocking(false).unwrap();
                return Some(
                    serde_json::from_str(text.trim_end())
                        .unwrap_or_else(|err| panic!("invalid json line {text:?}: {err}")),
                );
            }

            if Instant::now() >= deadline {
                self.stream.set_nonblocking(false).unwrap();
                return None;
            }

            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.stream.set_nonblocking(false).unwrap();
                    return None;
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("failed to read stream line: {err}"),
            }
        }
    }

    fn read_line(&mut self, timeout: Duration) -> Value {
        self.try_read_line(timeout)
            .unwrap_or_else(|| panic!("timed out waiting for a stream line"))
    }

    /// Reads lines until one has `type == event_type`, or `timeout` expires.
    fn wait_for_event_type(&mut self, event_type: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let slice = remaining.min(Duration::from_millis(200));
            if let Some(value) = self.try_read_line(slice) {
                if value["type"] == event_type {
                    return Some(value);
                }
            } else if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Reads and discards any lines already pending, up to `quiet`.
    fn drain(&mut self, quiet: Duration) {
        while self.try_read_line(quiet).is_some() {}
    }
}

#[test]
fn attach_stream_delivers_snapshot_then_frames_and_accepts_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let (pane_id, terminal_id) = create_workspace_and_root_pane(&api_socket, "attach-stream-basic");

    // One interactive controller (which may send input) plus one view-only
    // observer. The single-controller rule forbids a second interactive
    // stream, so fan-out is proven through the observer. Each renders at its
    // own declared size: the controller drives the PTY, the observer only
    // chooses its render area.
    let (mut conn_a, started_a) =
        StreamConn::open(&api_socket, &terminal_id, "interactive", 90, 28);
    let (mut conn_b, started_b) = StreamConn::open(&api_socket, &terminal_id, "view", 80, 24);

    assert_eq!(started_a["result"]["type"], "attach_stream_started");
    assert_eq!(started_a["result"]["terminal_id"], terminal_id.as_str());
    assert_eq!(started_a["result"]["cols"], 90);
    assert_eq!(started_a["result"]["rows"], 28);
    assert_eq!(started_b["result"]["type"], "attach_stream_started");
    assert_eq!(started_b["result"]["cols"], 80);
    assert_eq!(started_b["result"]["rows"], 24);

    // The first stream event after the started response is the snapshot.
    let snapshot_a = conn_a.read_line(Duration::from_secs(5));
    assert_eq!(snapshot_a["type"], "snapshot", "first event: {snapshot_a}");
    assert_eq!(snapshot_a["cols"], 90);
    assert_eq!(snapshot_a["rows"], 28);

    let snapshot_b = conn_b.read_line(Duration::from_secs(5));
    assert_eq!(snapshot_b["type"], "snapshot", "first event: {snapshot_b}");
    assert_eq!(snapshot_b["cols"], 80);
    assert_eq!(snapshot_b["rows"], 24);

    // Drain any redraw noise so the frame assertions below are tied to our input.
    conn_a.drain(Duration::from_millis(200));
    conn_b.drain(Duration::from_millis(200));

    conn_a.send_input("echo herdr-stream-ok\n");

    assert!(
        pane_read_recent_contains(
            &api_socket,
            &pane_id,
            "herdr-stream-ok",
            Duration::from_secs(10)
        ),
        "pane output should reflect input sent over the attach stream. pane output:\n{}",
        pane_read_recent(&api_socket, &pane_id, 200)
    );

    assert!(
        conn_a
            .wait_for_event_type("frame", Duration::from_secs(5))
            .is_some(),
        "connection A should receive a frame redraw after its own input"
    );
    assert!(
        conn_b
            .wait_for_event_type("frame", Duration::from_secs(5))
            .is_some(),
        "connection B should receive a frame redraw fanned out from connection A's input"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn attach_stream_view_mode_rejects_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let (pane_id, terminal_id) = create_workspace_and_root_pane(&api_socket, "attach-stream-view");

    let (mut conn, started) = StreamConn::open(&api_socket, &terminal_id, "view", 80, 24);
    assert_eq!(started["result"]["type"], "attach_stream_started");

    let snapshot = conn.read_line(Duration::from_secs(5));
    assert_eq!(snapshot["type"], "snapshot", "first event: {snapshot}");

    conn.drain(Duration::from_millis(200));

    conn.send_input("echo view-mode-should-not-run\n");

    let error = conn
        .wait_for_event_type("error", Duration::from_secs(5))
        .expect("view-mode input should yield an error event");
    assert_eq!(error["code"], "input_not_allowed");

    // The stream must stay open: real terminal output still reaches this seat.
    pane_send_input(&api_socket, &pane_id, "echo view-mode-still-alive");
    assert!(
        conn.wait_for_event_type("frame", Duration::from_secs(10))
            .is_some(),
        "view-mode seat should keep receiving frames after a rejected input"
    );

    // Resizing an observer changes only its own render area.
    conn.send_line(r#"{"type":"resize","cols":100,"rows":30}"#);
    let deadline = Instant::now() + Duration::from_secs(5);
    let resized = loop {
        match conn.try_read_line(Duration::from_millis(200)) {
            Some(event) if event["cols"] == 100 && event["rows"] == 30 => break Some(event),
            _ if Instant::now() >= deadline => break None,
            _ => {}
        }
    };
    assert!(
        resized.is_some(),
        "view-mode seat should render at its resized area"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn attach_stream_on_unknown_terminal_errors() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let (mut conn, response) = StreamConn::open_unknown_terminal(&api_socket, "does-not-exist");

    assert!(
        response.get("result").is_none(),
        "unknown terminal should not start a stream: {response}"
    );
    assert_eq!(
        response["error"]["code"], "terminal_not_found",
        "response: {response}"
    );

    assert!(
        conn.try_read_line(Duration::from_millis(500)).is_none(),
        "unknown-terminal connection should yield no stream events"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn attach_stream_interactive_requires_takeover_to_replace_the_controller() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let (pane_id, terminal_id) =
        create_workspace_and_root_pane(&api_socket, "attach-stream-takeover");

    let (mut first, started) = StreamConn::open(&api_socket, &terminal_id, "interactive", 80, 24);
    assert_eq!(started["result"]["type"], "attach_stream_started");
    first
        .wait_for_event_type("snapshot", Duration::from_secs(5))
        .expect("first controller should receive a snapshot");

    // A second controller without takeover is rejected and never streams.
    let (mut rejected, response) =
        StreamConn::open(&api_socket, &terminal_id, "interactive", 80, 24);
    assert!(
        response.get("result").is_none(),
        "second controller should not start without takeover: {response}"
    );
    assert_eq!(response["error"]["code"], "terminal_busy");
    assert!(
        rejected.try_read_line(Duration::from_millis(500)).is_none(),
        "rejected connection should yield no stream events"
    );

    // With takeover the new controller wins and the old one is told why.
    let (mut second, started) =
        StreamConn::open_with_takeover(&api_socket, &terminal_id, "interactive", 80, 24, true);
    assert_eq!(started["result"]["type"], "attach_stream_started");

    let detached = first
        .wait_for_event_type("detached", Duration::from_secs(5))
        .expect("replaced controller should receive a detached event");
    assert_eq!(detached["reason"], "terminal attach taken over");

    second
        .wait_for_event_type("snapshot", Duration::from_secs(5))
        .expect("new controller should receive a snapshot");
    second.drain(Duration::from_millis(200));
    second.send_input("echo herdr-takeover-ok\n");
    assert!(
        pane_read_recent_contains(
            &api_socket,
            &pane_id,
            "herdr-takeover-ok",
            Duration::from_secs(10)
        ),
        "input from the new controller should reach the pane. pane output:\n{}",
        pane_read_recent(&api_socket, &pane_id, 200)
    );

    cleanup_spawned_herdr(server, base);
}
