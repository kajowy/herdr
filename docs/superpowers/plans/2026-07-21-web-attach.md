# Web attach (multi-seat + attach_stream) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let N clients attach to one herdr terminal at once, and expose that attach as a JSON/NDJSON socket-API stream (`terminal.attach_stream`) so non-Rust clients can view and type into a pane.

**Architecture:** A stream seat is a normal `ClientConnection` in the headless server with `mode = TerminalAttach`, `render_encoding = TerminalAnsi` and a new `wire = Json` marker, so it reuses the existing render fan-out, `prepare_frame` diffing and input routing untouched. Only two things are new: per-client wire encoding (bincode frame vs. one JSON line) at the send boundary, and an API-connection driver thread that owns the socket, feeds `ServerEvent`s in and writes encoded bytes out. Single-seat attach becomes a one-element seat set; PTY winsize becomes the min-bbox over the *negotiating* seats only.

**Tech Stack:** Rust 2021, no new dependencies (`serde`, `serde_json`, `base64 0.22`, `interprocess 2.4` are already in `Cargo.toml`).

## Global Constraints

- Work only in the worktree `/Users/kaj/git/herdr/.claude/worktrees/web-attach`, branch `feat/web-attach`. Never touch `/Users/kaj/git/herdr`.
- NEVER restart, kill, reload or replace the running herdr server. `cargo build` / `cargo test` in this worktree are safe; running the built binary as a server is not (the integration test in Task 6 spawns its own isolated server with its own socket dir — that is the only allowed exception, and it must use a unique `/tmp` base like the existing `tests/multi_client.rs` does).
- Do not push. Commit locally only.
- Before EVERY commit: `just lint` (= `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings`) and `cargo nextest run --locked` must be green. If `cargo nextest` is unavailable use `cargo test --locked`.
- No `PROTOCOL_VERSION` bump; do not add, remove or reorder `ServerMessage` / `ClientMessage` variants — the binary protocol shape is frozen.
- Existing single-terminal attach/takeover/resize tests must keep passing unchanged.
- Every commit message: `{type}: {subject}`, max 50 chars, English, no "Generated with Claude" footer, no test plan.
- `docs/` is gitignored in this repo — commit spec/plan docs with `git add -f`.

---

### Task 0: Commit spec and plan

**Files:**
- Add: `docs/superpowers/specs/2026-07-21-web-attach-design.md` (already on disk)
- Add: `docs/superpowers/plans/2026-07-21-web-attach.md` (this file)

- [ ] **Step 1: Force-add both docs (they are under the gitignored `/docs/*`)**

```bash
git add -f docs/superpowers/specs/2026-07-21-web-attach-design.md docs/superpowers/plans/2026-07-21-web-attach.md
git commit -m "docs: add web attach design and plan"
```

---

### Task 1: Multi-seat attach set

Replaces the single-owner map with a per-terminal seat set. Plain attach never rejects; `takeover` kicks every other seat.

**Files:**
- Modify: `src/server/headless.rs:214` (field decl), `:408` + `:3850` (constructors), `:1151-1172` (`remove_client`), `:2071-2144` (`attach_terminal_client`), `:4449-4454` (existing test assertions)
- Test: `src/server/headless.rs` `mod tests`

**Interfaces:**
- Produces: `HeadlessServer::terminal_attach_clients: HashMap<String, HashSet<u64>>` — replaces `terminal_attach_owners`. Later tasks read it via `self.terminal_attach_clients.get(terminal_id)`.

- [ ] **Step 1: Write the failing tests**

Add to `src/server/headless.rs` `mod tests`. Reuse the existing helper shape from `terminal_attach_client_exits_when_attached_pane_dies` (headless.rs:4418) — a factory keeps the later tasks short, so add it too:

```rust
    /// Builds a server with one test workspace and returns its terminal id string.
    fn attach_test_server() -> (HeadlessServer, String) {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("attached");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .to_string();
        (server, terminal_id)
    }

    /// Connects a binary terminal-attach client and attaches it to `terminal_id`.
    fn connect_attach_client(
        server: &mut HeadlessServer,
        client_id: u64,
        cols: u16,
        rows: u16,
        terminal_id: &str,
        takeover: bool,
    ) -> (
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (writer, control_rx, render_rx) = test_client_writer();
        server.handle_server_event(ServerEvent::ClientConnected {
            client_id,
            cols,
            rows,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        });
        server.handle_server_event(ServerEvent::ClientAttachTerminal {
            client_id,
            terminal_id: terminal_id.to_owned(),
            takeover,
        });
        (control_rx, render_rx)
    }

    #[test]
    fn plain_attach_admits_a_second_seat() {
        let (mut server, terminal_id) = attach_test_server();
        let (_a_control, _a_render) = connect_attach_client(&mut server, 7, 80, 24, &terminal_id, false);
        let (b_control, _b_render) = connect_attach_client(&mut server, 8, 80, 24, &terminal_id, false);

        let seats = server
            .terminal_attach_clients
            .get(&terminal_id)
            .expect("seat set");
        assert_eq!(seats.len(), 2, "both seats attached: {seats:?}");
        assert!(seats.contains(&7) && seats.contains(&8));
        assert!(server.clients.contains_key(&7), "first seat not kicked");
        assert!(
            b_control.try_recv().is_err(),
            "second seat must not be rejected"
        );
    }

    #[test]
    fn takeover_kicks_every_other_seat() {
        let (mut server, terminal_id) = attach_test_server();
        let (a_control, _a_render) = connect_attach_client(&mut server, 7, 80, 24, &terminal_id, false);
        let (b_control, _b_render) = connect_attach_client(&mut server, 8, 80, 24, &terminal_id, false);
        let (_c_control, _c_render) = connect_attach_client(&mut server, 9, 80, 24, &terminal_id, true);

        assert_eq!(
            server
                .terminal_attach_clients
                .get(&terminal_id)
                .expect("seat set")
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![9]
        );
        assert!(!server.clients.contains_key(&7));
        assert!(!server.clients.contains_key(&8));
        assert_eq!(
            read_server_shutdown_reason(a_control.recv().expect("kick a")),
            Some("terminal attach taken over".to_owned())
        );
        assert_eq!(
            read_server_shutdown_reason(b_control.recv().expect("kick b")),
            Some("terminal attach taken over".to_owned())
        );
    }

    #[test]
    fn detaching_one_seat_keeps_the_others() {
        let (mut server, terminal_id) = attach_test_server();
        let (_a_control, _a_render) = connect_attach_client(&mut server, 7, 80, 24, &terminal_id, false);
        let (_b_control, _b_render) = connect_attach_client(&mut server, 8, 80, 24, &terminal_id, false);

        server.handle_server_event(ServerEvent::ClientDetach { client_id: 7 });

        let seats = server
            .terminal_attach_clients
            .get(&terminal_id)
            .expect("seat set");
        assert_eq!(seats.iter().copied().collect::<Vec<_>>(), vec![8]);
        assert!(server.clients.contains_key(&8));
        assert!(
            server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&server.terminal_id_by_string(&terminal_id).expect("id")),
            "resize lock must stay while a seat remains"
        );

        server.handle_server_event(ServerEvent::ClientDetach { client_id: 8 });
        assert!(!server.terminal_attach_clients.contains_key(&terminal_id));
        assert!(server.app.state.direct_attach_resize_locks.is_empty());
    }

    #[test]
    fn input_from_every_seat_reaches_the_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();

        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("attached");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24, 8);
        workspace.insert_test_runtime(pane_id, runtime);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .to_string();

        let (_a_control, _a_render) = connect_attach_client(&mut server, 7, 80, 24, &terminal_id, false);
        let (_b_control, _b_render) = connect_attach_client(&mut server, 8, 80, 24, &terminal_id, false);

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 7,
            data: b"a".to_vec(),
        });
        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 8,
            data: b"b".to_vec(),
        });

        assert_eq!(input_rx.try_recv().expect("seat a input"), Bytes::from("a"));
        assert_eq!(input_rx.try_recv().expect("seat b input"), Bytes::from("b"));

        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }
```

`crate::terminal::TerminalRuntime::test_with_channel(cols, rows, capacity)` may not exist under that exact name — check `src/terminal/runtime.rs` for the existing `test_with_channel_and_scrollback_bytes(...)` helper used at headless.rs:4514 and use whichever test constructor gives back an input receiver. Do NOT add a new production helper for this.

Also update the two stale assertions in `terminal_attach_client_exits_when_attached_pane_dies` (headless.rs:4449, :4454):

```rust
        assert_eq!(
            server
                .terminal_attach_clients
                .get(&terminal_id)
                .map(|seats| seats.iter().copied().collect::<Vec<_>>()),
            Some(vec![7])
        );
        // ... after PaneDied:
        assert!(!server.terminal_attach_clients.contains_key(&terminal_id));
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --bin herdr server::headless::tests::plain_attach_admits_a_second_seat`
Expected: FAIL — `no field terminal_attach_clients on type HeadlessServer` (compile error is a valid red).

- [ ] **Step 3: Replace the owner map with a seat set**

In `src/server/headless.rs`:

1. Field (`:214`): `terminal_attach_clients: HashMap<String, HashSet<u64>>,` and add `use std::collections::HashSet;` if absent. Update both constructors (`:408`, `:3850`) to `HashMap::new()`.
2. `remove_client` (`:1157-1165`) — only drop the resize lock when the seat set empties:

```rust
            if let ClientConnectionMode::TerminalAttach { terminal_id } = removed.mode {
                let seats_left = if let Some(seats) = self.terminal_attach_clients.get_mut(&terminal_id) {
                    seats.remove(&client_id);
                    seats.len()
                } else {
                    0
                };
                if seats_left == 0 {
                    self.terminal_attach_clients.remove(&terminal_id);
                    if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                        self.app.state.direct_attach_resize_locks.remove(&terminal_id);
                    }
                }
            }
```

3. `attach_terminal_client` (`:2090-2112`) — drop the rejection branch, kick all others on takeover:

```rust
        if takeover {
            let others: Vec<u64> = self
                .terminal_attach_clients
                .get(&terminal_id)
                .map(|seats| seats.iter().copied().filter(|id| *id != client_id).collect())
                .unwrap_or_default();
            for other in others {
                self.send_to_client(
                    other,
                    ServerMessage::ServerShutdown {
                        reason: Some("terminal attach taken over".to_owned()),
                    },
                );
                self.remove_client_and_resize_if_needed(other);
            }
        }
```

4. Registration (`:2132`): `self.terminal_attach_clients.entry(terminal_id.clone()).or_default().insert(client_id);`

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --bin herdr server::headless::tests`
Expected: PASS, including the pre-existing attach tests.

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`
Expected: no warnings, all green.

- [ ] **Step 6: Commit**

```bash
git add src/server/headless.rs
git commit -m "feat: allow multiple seats per attached terminal"
```

---

### Task 2: Seat size roles and min-bbox winsize

Adds `SizeRole` to attached clients and makes the PTY winsize the min-bbox over negotiating seats only. Binary attach clients stay `Negotiating`, so single-seat behavior is byte-identical to today.

**Files:**
- Modify: `src/server/clients.rs` (add `SizeRole`, field + constructor param), `src/server/headless.rs` (attach, `ClientResize`, detach paths)
- Test: `src/server/headless.rs` `mod tests`

**Interfaces:**
- Consumes: `HeadlessServer::terminal_attach_clients` (Task 1).
- Produces:
  - `pub(crate) enum SizeRole { Negotiating, Passive }` in `src/server/clients.rs` (derive `Debug, Clone, Copy, PartialEq, Eq`).
  - `ClientConnection::size_role: SizeRole` — defaults to `Negotiating` in `new_with_mode` (no new constructor parameter; stream seats set the field after construction in Task 4).
  - `HeadlessServer::terminal_attach_sizes: HashMap<String, (u16, u16)>` — the size passive seats render at.
  - `fn negotiated_attach_size(&self, terminal_id: &str) -> Option<(u16, u16)>` — min-bbox over negotiating seats of that terminal, `None` if there are none.
  - `fn apply_terminal_attach_size(&mut self, terminal_id: &str)` — recomputes, stores in `terminal_attach_sizes`, resizes the runtime when a negotiating seat exists, and re-points every passive seat (`terminal_size` + `request_full_redraw`). Task 4 extends it to emit `resize` stream events.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn negotiating_seats_use_min_bbox() {
        let (mut server, terminal_id) = attach_test_server();
        connect_attach_client(&mut server, 7, 120, 40, &terminal_id, false);
        assert_eq!(
            server.negotiated_attach_size(&terminal_id),
            Some((120, 40))
        );

        connect_attach_client(&mut server, 8, 100, 50, &terminal_id, false);
        assert_eq!(
            server.negotiated_attach_size(&terminal_id),
            Some((100, 40)),
            "min cols x min rows over both seats"
        );

        server.handle_server_event(ServerEvent::ClientDetach { client_id: 8 });
        assert_eq!(
            server.negotiated_attach_size(&terminal_id),
            Some((120, 40)),
            "detach recomputes from the remaining seat"
        );
    }

    #[test]
    fn passive_seat_never_changes_negotiated_size() {
        let (mut server, terminal_id) = attach_test_server();
        connect_attach_client(&mut server, 7, 120, 40, &terminal_id, false);
        connect_attach_client(&mut server, 8, 40, 10, &terminal_id, false);
        server.clients.get_mut(&8).expect("seat").size_role = SizeRole::Passive;
        server.apply_terminal_attach_size(&terminal_id);

        assert_eq!(
            server.negotiated_attach_size(&terminal_id),
            Some((120, 40)),
            "passive seat is excluded from the bbox"
        );
        assert_eq!(
            server.clients.get(&8).expect("seat").terminal_size,
            (120, 40),
            "passive seat renders at the terminal size"
        );
    }

    #[test]
    fn passive_resize_request_is_ignored() {
        let (mut server, terminal_id) = attach_test_server();
        connect_attach_client(&mut server, 7, 120, 40, &terminal_id, false);
        connect_attach_client(&mut server, 8, 120, 40, &terminal_id, false);
        server.clients.get_mut(&8).expect("seat").size_role = SizeRole::Passive;

        server.handle_server_event(ServerEvent::ClientResize {
            client_id: 8,
            cols: 30,
            rows: 8,
            cell_width_px: 0,
            cell_height_px: 0,
        });

        assert_eq!(server.negotiated_attach_size(&terminal_id), Some((120, 40)));
        assert_eq!(
            server.clients.get(&8).expect("seat").terminal_size,
            (120, 40)
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --bin herdr server::headless::tests::negotiating_seats_use_min_bbox`
Expected: FAIL — `no method named negotiated_attach_size`.

- [ ] **Step 3: Implement roles and bbox**

`src/server/clients.rs`:

```rust
/// Whether an attached client participates in PTY winsize negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SizeRole {
    /// Real terminals: the PTY winsize is the min-bbox over these seats.
    Negotiating,
    /// Web/stream viewers: they render at the terminal's size and never resize it.
    Passive,
}
```

Add `pub(crate) size_role: SizeRole` to `ClientConnection`, set to `SizeRole::Negotiating` in `new_with_mode`.

`src/server/headless.rs`:

- New field `terminal_attach_sizes: HashMap<String, (u16, u16)>` on `HeadlessServer` (+ both constructors).
- `negotiated_attach_size` folds `terminal_attach_clients[terminal_id]` over `self.clients`, keeping only seats whose `size_role == SizeRole::Negotiating`, and reduces `(min cols, min rows)`.
- `apply_terminal_attach_size(&mut self, terminal_id: &str)`:
  - `let size = self.negotiated_attach_size(terminal_id);`
  - if `Some(size)`: store it in `terminal_attach_sizes` and call `runtime.resize(rows, cols, cell_width_px, cell_height_px)` using the cell size of any negotiating seat (pick the smallest-cols seat's `cell_size`, matching what a single seat does today);
  - if `None`: keep the stored entry; if there is none, seed it from the first passive seat's own `terminal_size` and do NOT resize the runtime (spec §2: passive seats never drive winsize; `suggest_size` is deferred to v2);
  - for every passive seat of the terminal whose `terminal_size` differs from the stored size: set `terminal_size`, `request_full_redraw()`.
  - drop the `terminal_attach_sizes` entry when the seat set is gone (do this in `remove_client` next to the seat-set cleanup from Task 1).
- Call `apply_terminal_attach_size` from: the end of `attach_terminal_client` (replacing the direct `runtime.resize(...)` at `:2140-2142`), the `ClientResize` direct-attach branch (`:2397-2402`, replacing the direct resize), and `remove_client_and_resize_if_needed` (after `remove_client`, for the terminal the removed seat was attached to — capture that id before removal).
- In the `ClientResize` direct-attach branch, ignore the new size when `size_role == SizeRole::Passive`: do not write `terminal_size`, do not reset the baseline, return `false`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --bin herdr server::headless::tests`
Expected: PASS, including `terminal_attach_*` legacy tests.

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`
Expected: all green — in particular `tests/multi_client.rs` and `tests/detach_reattach.rs`.

- [ ] **Step 6: Commit**

```bash
git add src/server/clients.rs src/server/headless.rs
git commit -m "feat: negotiate attach winsize over seats min-bbox"
```

---

### Task 3: JSON stream wire codec

Pure, dependency-free encode/decode for the `attach_stream` wire. No server wiring yet, so this task is fully unit-testable and runs on Windows CI too.

**Files:**
- Create: `src/server/attach_stream.rs`
- Modify: `src/server/mod.rs` (add `pub(crate) mod attach_stream;`)
- Test: `src/server/attach_stream.rs` `mod tests`

**Interfaces:**
- Produces (all `pub(crate)`):
  - `enum StreamMode { Interactive, View }` — serde `#[serde(rename_all = "lowercase")]`, `Default` = `Interactive`.
  - `fn encode_server_message(msg: &crate::protocol::ServerMessage) -> Option<Vec<u8>>` — one newline-terminated JSON line, `None` for messages with no JSON mapping.
  - `fn encode_resize(cols: u16, rows: u16) -> Vec<u8>`
  - `fn encode_error(code: &str, message: &str) -> Vec<u8>`
  - `enum StreamCommand { Input { data: Vec<u8> }, Resize { cols: u16, rows: u16 }, Detach }`
  - `fn parse_command(line: &str) -> Result<StreamCommand, String>`

Mapping (spec §3):
- `ServerMessage::Terminal(TerminalFrame { seq, width, height, full, bytes })` → `{"type":"snapshot"|"frame","seq":<seq>,"cols":<width>,"rows":<height>,"data":"<base64 bytes>"}` (`snapshot` when `full`).
- `ServerMessage::ServerShutdown { reason }` → `{"type":"detached","reason":"<reason or \"server shutdown\">"}`.
- Everything else → `None`.

- [ ] **Step 1: Write the failing tests**

Create `src/server/attach_stream.rs` containing only the test module first (so the red is real):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ServerMessage, TerminalFrame};

    fn line(bytes: Vec<u8>) -> serde_json::Value {
        let text = String::from_utf8(bytes).expect("utf8 line");
        assert!(text.ends_with('\n'), "events are newline framed: {text:?}");
        serde_json::from_str(text.trim_end()).expect("json line")
    }

    #[test]
    fn full_terminal_frame_encodes_as_snapshot() {
        let event = line(
            encode_server_message(&ServerMessage::Terminal(TerminalFrame {
                seq: 1,
                width: 200,
                height: 48,
                full: true,
                bytes: b"hi".to_vec(),
            }))
            .expect("encoded"),
        );
        assert_eq!(event["type"], "snapshot");
        assert_eq!(event["cols"], 200);
        assert_eq!(event["rows"], 48);
        assert_eq!(event["seq"], 1);
        assert_eq!(event["data"], "aGk=");
    }

    #[test]
    fn partial_terminal_frame_encodes_as_frame() {
        let event = line(
            encode_server_message(&ServerMessage::Terminal(TerminalFrame {
                seq: 2,
                width: 200,
                height: 48,
                full: false,
                bytes: b"hi".to_vec(),
            }))
            .expect("encoded"),
        );
        assert_eq!(event["type"], "frame");
    }

    #[test]
    fn shutdown_encodes_as_detached() {
        let event = line(
            encode_server_message(&ServerMessage::ServerShutdown {
                reason: Some("terminal attach taken over".into()),
            })
            .expect("encoded"),
        );
        assert_eq!(event["type"], "detached");
        assert_eq!(event["reason"], "terminal attach taken over");
    }

    #[test]
    fn messages_without_json_mapping_are_dropped() {
        assert!(encode_server_message(&ServerMessage::ReloadSoundConfig).is_none());
    }

    #[test]
    fn resize_and_error_events_encode() {
        let resize = line(encode_resize(120, 40));
        assert_eq!(resize["type"], "resize");
        assert_eq!(resize["cols"], 120);
        assert_eq!(resize["rows"], 40);

        let error = line(encode_error("input_not_allowed", "seat is view-only"));
        assert_eq!(error["type"], "error");
        assert_eq!(error["code"], "input_not_allowed");
    }

    #[test]
    fn commands_parse() {
        assert_eq!(
            parse_command(r#"{"type":"input","data":"aGk="}"#).expect("input"),
            StreamCommand::Input {
                data: b"hi".to_vec()
            }
        );
        assert_eq!(
            parse_command(r#"{"type":"resize","cols":100,"rows":30}"#).expect("resize"),
            StreamCommand::Resize {
                cols: 100,
                rows: 30
            }
        );
        assert_eq!(
            parse_command(r#"{"type":"detach"}"#).expect("detach"),
            StreamCommand::Detach
        );
    }

    #[test]
    fn malformed_commands_report_an_error() {
        assert!(parse_command("not json").is_err());
        assert!(parse_command(r#"{"type":"input","data":"!!not base64!!"}"#).is_err());
    }
}
```

Register the module in `src/server/mod.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --bin herdr server::attach_stream`
Expected: FAIL — `cannot find function encode_server_message`.

- [ ] **Step 3: Implement the codec**

Above the test module in `src/server/attach_stream.rs`. Use `base64::engine::general_purpose::STANDARD` via `base64::Engine` (see existing usage: `rg -n "base64" src/`). Derive `Serialize` on a private `StreamEvent` enum with `#[serde(tag = "type", rename_all = "snake_case")]` and `Deserialize` on `StreamCommand` the same way; `data` fields are `String` on the wire and decoded in `parse_command`. Each encode helper ends with a pushed `b'\n'`. Derive `Debug, PartialEq, Eq` on `StreamCommand` so the tests can assert equality.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --bin herdr server::attach_stream`
Expected: PASS (7 tests).

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`

- [ ] **Step 6: Commit**

```bash
git add src/server/attach_stream.rs src/server/mod.rs
git commit -m "feat: add json codec for attach stream events"
```

---

### Task 4: Stream seats in the headless server

Teaches the server to hold a JSON-wire seat: per-client encoding at the send boundary, seat registration via a new `ServerEvent`, view-mode input rejection, and `resize` events for passive seats.

**Files:**
- Modify: `src/server/clients.rs` (`ClientWire`, fields), `src/server/client_transport.rs` (new `ServerEvent` variant), `src/server/headless.rs` (encoding + handler)
- Test: `src/server/headless.rs` `mod tests`

**Interfaces:**
- Consumes: Task 2's `SizeRole`, `apply_terminal_attach_size`; Task 3's `encode_server_message`, `encode_resize`, `encode_error`, `StreamMode`.
- Produces:
  - `pub(crate) enum ClientWire { Bincode, Json }` in `src/server/clients.rs`; `ClientConnection::wire` (default `Bincode`), `ClientConnection::input_mode: StreamMode` (default `Interactive`), `ClientConnection::input_rejected_notified: bool` (default `false`).
  - `ServerEvent::AttachStreamConnected { terminal_id: String, mode: StreamMode, size_role: SizeRole, cols: u16, rows: u16, writer: ClientWriter, respond_to: std::sync::mpsc::Sender<Result<AttachStreamAccepted, String>> }` in `src/server/client_transport.rs`, plus `pub(crate) struct AttachStreamAccepted { pub(crate) client_id: u64, pub(crate) cols: u16, pub(crate) rows: u16 }`. The `Err(String)` payload is an API error code (`"terminal_not_found"`, `"unsupported"`).
  - `HeadlessServer::frame_for_client(&self, client_id: u64, msg: &ServerMessage) -> Option<Vec<u8>>` — JSON line for `ClientWire::Json`, length-prefixed bincode otherwise.
  - `HeadlessServer::send_stream_event(&mut self, client_id: u64, bytes: Vec<u8>)` — pushes a raw JSON line down the client's control channel.

- [ ] **Step 1: Write the failing tests**

```rust
    fn stream_seat(
        server: &mut HeadlessServer,
        terminal_id: &str,
        mode: crate::server::attach_stream::StreamMode,
    ) -> (
        u64,
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (writer, control_rx, render_rx) = test_client_writer();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        server.handle_server_event(ServerEvent::AttachStreamConnected {
            terminal_id: terminal_id.to_owned(),
            mode,
            size_role: SizeRole::Passive,
            cols: 80,
            rows: 24,
            writer,
            respond_to,
        });
        let accepted = response_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("attach response")
            .expect("attach accepted");
        (accepted.client_id, control_rx, render_rx)
    }

    fn json_event(bytes: Vec<u8>) -> serde_json::Value {
        let text = String::from_utf8(bytes).expect("utf8");
        serde_json::from_str(text.trim_end()).expect("json event")
    }

    #[test]
    fn stream_seat_receives_snapshot_then_deltas() {
        let (mut server, terminal_id) = attach_test_server();
        let (_client_id, _control_rx, render_rx) = stream_seat(
            &mut server,
            &terminal_id,
            crate::server::attach_stream::StreamMode::Interactive,
        );

        server.render_and_stream();
        let first = json_event(render_rx.try_recv().expect("first frame"));
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["cols"], 80);
        assert_eq!(first["rows"], 24);
        assert!(first["data"].as_str().expect("data").len() > 0);
    }

    #[test]
    fn view_mode_input_is_rejected_once() {
        let (mut server, terminal_id) = attach_test_server();
        let (client_id, control_rx, _render_rx) = stream_seat(
            &mut server,
            &terminal_id,
            crate::server::attach_stream::StreamMode::View,
        );

        server.handle_server_event(ServerEvent::ClientInput {
            client_id,
            data: b"rm -rf /".to_vec(),
        });
        let error = json_event(control_rx.try_recv().expect("error event"));
        assert_eq!(error["type"], "error");
        assert_eq!(error["code"], "input_not_allowed");

        server.handle_server_event(ServerEvent::ClientInput {
            client_id,
            data: b"x".to_vec(),
        });
        assert!(
            control_rx.try_recv().is_err(),
            "the error is reported once, then the stream continues quietly"
        );
        assert!(server.clients.contains_key(&client_id), "seat stays attached");
    }

    #[test]
    fn stream_seat_on_unknown_terminal_is_rejected() {
        let (mut server, _terminal_id) = attach_test_server();
        let (writer, _control_rx, _render_rx) = test_client_writer();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        server.handle_server_event(ServerEvent::AttachStreamConnected {
            terminal_id: "nope".to_owned(),
            mode: crate::server::attach_stream::StreamMode::View,
            size_role: SizeRole::Passive,
            cols: 80,
            rows: 24,
            writer,
            respond_to,
        });

        assert_eq!(
            response_rx
                .recv_timeout(Duration::from_millis(200))
                .expect("attach response"),
            Err("terminal_not_found".to_owned())
        );
        assert!(server.terminal_attach_clients.is_empty());
    }

    #[test]
    fn takeover_detaches_stream_seats_with_a_json_event() {
        let (mut server, terminal_id) = attach_test_server();
        let (_client_id, control_rx, _render_rx) = stream_seat(
            &mut server,
            &terminal_id,
            crate::server::attach_stream::StreamMode::Interactive,
        );
        connect_attach_client(&mut server, 9, 80, 24, &terminal_id, true);

        let event = json_event(control_rx.recv().expect("detached event"));
        assert_eq!(event["type"], "detached");
        assert_eq!(event["reason"], "terminal attach taken over");
    }

    #[test]
    fn passive_seat_is_told_about_negotiated_resizes() {
        let (mut server, terminal_id) = attach_test_server();
        let (client_id, control_rx, _render_rx) = stream_seat(
            &mut server,
            &terminal_id,
            crate::server::attach_stream::StreamMode::View,
        );
        connect_attach_client(&mut server, 9, 120, 40, &terminal_id, false);

        let event = json_event(control_rx.try_recv().expect("resize event"));
        assert_eq!(event["type"], "resize");
        assert_eq!(event["cols"], 120);
        assert_eq!(event["rows"], 40);
        assert_eq!(
            server.clients.get(&client_id).expect("seat").terminal_size,
            (120, 40)
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --bin herdr server::headless::tests::stream_seat_receives_snapshot_then_deltas`
Expected: FAIL — `no variant named AttachStreamConnected`.

- [ ] **Step 3: Implement stream seats**

1. `src/server/clients.rs`: add `ClientWire` (`Bincode` | `Json`, `Debug, Clone, Copy, PartialEq, Eq`) and the three new `ClientConnection` fields with the defaults listed under **Interfaces**.
2. `src/server/client_transport.rs`: add `AttachStreamAccepted` and the `ServerEvent::AttachStreamConnected` variant. `ClientWriter` does not derive `Debug`-friendly channels — the enum already derives `Debug`, and `std::sync::mpsc::Sender` is `Debug`, so this compiles as-is.
3. `src/server/headless.rs`:
   - `frame_for_client(client_id, msg)`: look up the client's `wire`; `ClientWire::Json` → `crate::server::attach_stream::encode_server_message(msg)`; `ClientWire::Bincode` → `Self::frame_server_message(msg).ok()`. Route `send_to_client` (`:2008`), `send_to_all_clients` (`:1974`, encode per client inside the loop instead of once up front), `send_client_graphics_cleanup` (`:1206`) and the render paths (`:2930`, `:3150`, `:3183`) through it. At `:3150` keep the oversize/`max_frame_size` handling for bincode clients only — JSON seats skip the graphics path entirely because `frame.graphics` is cleared for non-app clients (`:3113-3115`).
   - `handle_server_event` arm for `AttachStreamConnected`:
     - `#[cfg(unix)]` only; on Windows respond `Err("unsupported".into())` and return `false` (client ids are allocated by the Windows accept thread, so a second allocator would collide — the JSON attach stream is a unix-socket feature).
     - resolve the terminal via `terminal_id_by_string`; `None` → respond `Err("terminal_not_found")`, return `false`.
     - allocate `client_id` from `self.next_client_id` (post-increment, same as the accept path).
     - build `ClientConnection::new_with_mode(ClientConnectionMode::TerminalAttach { terminal_id }, None, clamp_terminal_size(cols, rows), HostCellSize::default(), TerminalTheme::default(), None, self.allocate_activity_stamp(), RenderEncoding::TerminalAnsi, false, Some(writer))`, then set `wire = Json`, `size_role`, `input_mode = mode`.
     - insert into `self.clients`, insert into the seat set, insert the `direct_attach_resize_locks` entry, call `apply_terminal_attach_size(&terminal_id)`, `request_full_redraw()` on the seat (guarantees the first frame is `full: true` → a `snapshot`).
     - respond `Ok(AttachStreamAccepted { client_id, cols, rows })` with the seat's post-`apply` `terminal_size`, and return `true`.
   - `ClientInput` arm (`:2295`): before routing to the runtime, if the client's `input_mode == StreamMode::View`, then — when `!input_rejected_notified` — set the flag and `send_stream_event(client_id, encode_error("input_not_allowed", "attach stream seat is view-only"))`; return `false` either way.
   - Same guard in the `ClientResize` direct-attach branch for `SizeRole::Passive` seats (Task 2 already ignores the size; add the one-shot `input_not_allowed` error for JSON seats).
   - `apply_terminal_attach_size`: when a passive seat's `terminal_size` changes and its wire is `Json`, also `send_stream_event(client_id, encode_resize(cols, rows))`.
   - `send_stream_event`: fetch the client's writer, `control.send(bytes)`; on error mark the client broken exactly like `send_to_client` does.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --bin herdr server::headless::tests`
Expected: PASS.

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`

- [ ] **Step 6: Commit**

```bash
git add src/server/clients.rs src/server/client_transport.rs src/server/headless.rs
git commit -m "feat: serve json wire seats from the render loop"
```

---

### Task 5: `terminal.attach_stream` API method

Exposes the seat over the existing JSON socket listener: request → `attach_stream_started` response → NDJSON events, with commands read back on the same connection.

**Files:**
- Modify: `src/api/schema.rs` (method + params + response variant), `src/api/server.rs` (routing + connection driver), `src/api/mod.rs` (thread the event sender), `src/server/headless.rs:1040`/`:3582`/`:3722` (pass `server_event_tx`), `src/main.rs:633` (pass `None`)
- Test: `src/api/server.rs` `mod tests`

**Interfaces:**
- Consumes: Task 4's `ServerEvent::AttachStreamConnected` / `AttachStreamAccepted`, Task 3's `parse_command`.
- Produces:
  - `Method::TerminalAttachStream(TerminalAttachStreamParams)` with `#[serde(rename = "terminal.attach_stream")]`; `pub struct TerminalAttachStreamParams { pub terminal_id: String, #[serde(default)] pub mode: StreamModeParam, #[serde(default)] pub size_role: SizeRoleParam, #[serde(default)] pub cols: Option<u16>, #[serde(default)] pub rows: Option<u16> }`. `StreamModeParam` = `interactive` (default) | `view`; `SizeRoleParam` = `passive` (default) | `negotiating`. Both are public serde enums in `schema.rs` that the server maps onto the internal `StreamMode` / `SizeRole`.
  - `ResponseResult::AttachStreamStarted { terminal_id: String, cols: u16, rows: u16 }`.
  - `api::start_server_with_capabilities(api_tx, event_hub, capabilities, attach_events: Option<tokio::sync::mpsc::Sender<crate::server::client_transport::ServerEvent>>)` — `start_server` gains the same trailing parameter.

- [ ] **Step 1: Write the failing test**

In `src/api/server.rs` `mod tests` (the file's tests are already `#[cfg(all(test, unix))]` and have the `local_stream_pair` helper):

```rust
    #[test]
    fn attach_stream_without_server_event_channel_reports_unsupported() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-attach-unsupported");
        client
            .write_all(
                br#"{"id":"as_1","method":"terminal.attach_stream","params":{"terminal_id":"t1","mode":"view","size_role":"passive"}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let event_hub = EventHub::default();
        let thread = std::thread::spawn(move || {
            handle_connection(server, &api_tx, &event_hub, &running, None, None)
        });

        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(response["error"]["code"], "unsupported");
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn attach_stream_forwards_the_seat_request_and_streams_events() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (mut client, server, _path) = local_stream_pair("api-attach-stream");
        client
            .write_all(
                br#"{"id":"as_2","method":"terminal.attach_stream","params":{"terminal_id":"t1","mode":"interactive","size_role":"passive","cols":80,"rows":24}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let event_hub = EventHub::default();
        let thread = std::thread::spawn(move || {
            handle_connection(server, &api_tx, &event_hub, &running, None, Some(event_tx))
        });

        // The server side of the seat: accept the registration, then push one frame.
        let writer = loop {
            match event_rx.blocking_recv().expect("seat request") {
                crate::server::client_transport::ServerEvent::AttachStreamConnected {
                    terminal_id,
                    writer,
                    respond_to,
                    ..
                } => {
                    assert_eq!(terminal_id, "t1");
                    respond_to
                        .send(Ok(
                            crate::server::client_transport::AttachStreamAccepted {
                                client_id: 42,
                                cols: 80,
                                rows: 24,
                            },
                        ))
                        .unwrap();
                    break writer;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        };

        let started: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(started["result"]["type"], "attach_stream_started");

        writer
            .render
            .send(crate::server::attach_stream::encode_resize(80, 24))
            .unwrap();
        let event: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(event["type"], "resize");

        // Input from the browser reaches the server loop as a ClientInput event.
        client
            .write_all(br#"{"type":"input","data":"aGk="}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        match event_rx.blocking_recv().expect("input event") {
            crate::server::client_transport::ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 42);
                assert_eq!(data, b"hi".to_vec());
            }
            other => panic!("unexpected event: {other:?}"),
        }

        drop(client);
        thread.join().unwrap().unwrap();
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --bin herdr api::server::tests::attach_stream`
Expected: FAIL — `handle_connection` takes 5 arguments.

- [ ] **Step 3: Implement the API method**

1. `src/api/schema.rs`: add the method variant, params structs and `ResponseResult::AttachStreamStarted { .. }` (tag `attach_stream_started`, matching the `SubscriptionStarted {}` style at `:694`).
2. `src/api/server.rs`:
   - `api_method_name`: `Method::TerminalAttachStream(_) => "terminal.attach_stream"`.
   - Thread `attach_events: Option<tokio::sync::mpsc::Sender<ServerEvent>>` through `start_server`, `start_server_with_capabilities`, the accept loop and `handle_connection`.
   - New `fn stream_terminal_attach(stream, request_id, params, attach_events, running) -> io::Result<()>`, routed from `handle_connection` next to `Method::EventsSubscribe`:
     - `None` channel → write the standard error response with code `unsupported`, return.
     - build a `ClientWriter` pair (`std::sync::mpsc::channel()` for control, `sync_channel(1)` for render — same capacities as `client_writer_loop` uses) plus a `std::sync::mpsc::channel()` for the accept response; `try_send` the `AttachStreamConnected` event; on `Err`/timeout (`recv_timeout(APP_RESPONSE_TIMEOUT)`) write `server_unavailable`.
     - `Err(code)` → write the error response with that code (`terminal_not_found`, `unsupported`) and return.
     - `Ok(accepted)` → write `SuccessResponse { result: AttachStreamStarted { .. } }`, then `stream.split()` into `(recv_half, send_half)`; spawn a writer thread that drains control (priority) and render channels and writes each `Vec<u8>` verbatim to `send_half` (the bytes already end in `\n`), exiting when both channels close or the write fails; on the current thread read NDJSON lines from `recv_half` with `BufReader::new`, mapping each `parse_command` result to `ServerEvent::ClientInput { client_id, data }`, `ServerEvent::ClientResize { client_id, cols, rows, cell_width_px: 0, cell_height_px: 0 }` or `ServerEvent::ClientDetach { client_id }`; a parse error sends nothing and continues.
     - When the read loop ends (EOF, detach command, or `!running`), send `ServerEvent::ClientDetach { client_id }`, drop the writer channels, join the writer thread, return `Ok(())`.
3. `src/server/headless.rs`: pass `Some(self.server_event_tx.clone())` at the three `api::start_server*` call sites (`:1040`, `:3582`, `:3722`). `src/main.rs:633`: pass `None` (the in-process TUI has no headless client registry).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --bin herdr api::`
Expected: PASS.

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`

- [ ] **Step 6: Commit**

```bash
git add src/api src/server/headless.rs src/main.rs
git commit -m "feat: add terminal.attach_stream socket api method"
```

---

### Task 6: End-to-end acceptance test

Proves the whole path against a real spawned server: two seats, snapshot→delta, input, view-mode rejection, unknown terminal.

**Files:**
- Create: `tests/attach_stream.rs`
- Test: itself

**Interfaces:**
- Consumes: everything above, over the socket only — no crate internals (integration tests see only the public surface, so drive the server exclusively through `herdr.sock`).

- [ ] **Step 1: Write the failing test**

Model the harness on `tests/multi_client.rs`: `mod support;`, `spawn_server(&config_home, &runtime_dir, &api_socket)`, `wait_for_socket`, `create_workspace_and_root_pane`, a unique `/tmp` base, and `cleanup_spawned_herdr` at the end. Copy those helpers rather than exporting new ones from `multi_client.rs` if they are file-private — matching the repo's existing duplication between test binaries.

```rust
#[test]
fn attach_stream_delivers_snapshot_then_frames_and_accepts_input() {
    // 1. spawn an isolated server, create a workspace, read its terminal id
    //    (pane.list → the root pane's terminal_id).
    // 2. open TWO connections to the api socket, each sending:
    //    {"id":"s1","method":"terminal.attach_stream",
    //     "params":{"terminal_id":"<id>","mode":"interactive","size_role":"passive"}}
    // 3. assert both read {"result":{"type":"attach_stream_started",...}}
    // 4. assert the first streamed event on each is {"type":"snapshot"} and that a
    //    later event is {"type":"frame"} (send input to force a redraw).
    // 5. send {"type":"input","data":"<base64 of \"echo herdr-stream-ok\\n\">"} on
    //    connection A, then poll `agent read` / `pane.read` over the api socket until
    //    the output contains "herdr-stream-ok" (deadline 10s).
    // 6. assert connection B also received frames after A's input (multi-seat fan-out).
}

#[test]
fn attach_stream_view_mode_rejects_input() {
    // same setup, mode:"view"; sending an input command yields
    // {"type":"error","code":"input_not_allowed"} and the stream stays open
    // (a subsequent frame still arrives).
}

#[test]
fn attach_stream_on_unknown_terminal_errors() {
    // terminal_id:"does-not-exist" → {"error":{"code":"terminal_not_found"}} and the
    // connection closes without any stream events.
}
```

Write these out fully (no comment-only bodies) using the existing helpers; the comments above are the specification of each step, not the deliverable.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked --test attach_stream`
Expected: FAIL before Tasks 1-5 land; after them, iterate until green.

- [ ] **Step 3: Fix whatever the end-to-end path exposes**

Real-socket failures here are real bugs — fix them in the server, not by weakening the test. Common suspects: the writer thread not flushing, `snapshot` not being `full` on the first frame, a passive seat's render area not matching the stored size.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked --test attach_stream`
Expected: PASS (3 tests).

- [ ] **Step 5: Lint and full suite**

Run: `just lint && cargo nextest run --locked`
Expected: all green, including the previously existing test binaries.

- [ ] **Step 6: Commit**

```bash
git add tests/attach_stream.rs
git commit -m "test: cover attach stream end to end"
```

---

## Spec coverage

| Spec requirement | Task |
| --- | --- |
| §1 multi-seat set, plain attach never rejects | 1 |
| §1 takeover kicks all other seats | 1 |
| §1 input from any seat reaches the PTY | 1 |
| §1 render fan-out to every seat | already true (`clients::render_targets`); asserted in 1 & 6 |
| §1 detach removes one seat, recomputes size | 1, 2 |
| §2 `SizeRole`, min-bbox over negotiating seats | 2 |
| §2 zero negotiating seats keeps the current size | 2 |
| §2 passive seats may not resize | 2, 4 |
| §3 request/response envelope, `mode`, `size_role` | 5 |
| §3 snapshot → diffed frames | 3, 4, 6 |
| §3 `resize` events | 3, 4 |
| §3 input / resize / detach commands | 3, 5 |
| §3 `input_not_allowed`, stream continues | 4, 6 |
| §3 connection close == detach | 5 |
| §4 CLI `terminal watch` | deferred by the spec — not implemented |
| §5 dashboard consumer | out of scope for this repo |
| Error handling: unknown terminal | 4, 5, 6 |
| Error handling: slow consumer drops frames | already true (`render` channel is `sync_channel(1)`, `render_pending` defers); no new code |
| Error handling: takeover while streaming → `detached` then close | 4 |

## Known deviations from the spec

- **`cursor` field on `snapshot`/`frame` events is omitted.** The `TerminalAnsi` payload already carries cursor positioning inline (that is what a terminal emulator consumes), and the cursor is not available at the encode boundary without threading `FrameData` through `ServerMessage`. xterm.js needs nothing extra. Revisit only if a consumer asks.
- **`terminal.attach_stream` is unix-only.** Stream seats need a server-allocated client id; on Windows those ids come from a separate accept thread, so the method answers `unsupported` there rather than risking id collisions.
