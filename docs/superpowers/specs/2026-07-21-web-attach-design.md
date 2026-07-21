# Web attach: multi-seat terminals + JSON attach-stream API

Date: 2026-07-21
Components: herdr server (`src/server/*`, `src/protocol/*`, `src/cli*`) — primary;
`pr-summary/dashboard.mjs` (ttm-discord-bot, worktree `dashboard`) — consumer.
Register: product (terminal multiplexer feature + read/write web console)

## Problem

herdr terminals are single-seat: `terminal_attach_owners` maps each terminal to
exactly one attached client (`src/server/headless.rs:2089-2111`). A second
attach is rejected ("retry with --takeover") and `--takeover` kicks the current
owner. There is no way for two humans/tools to watch one agent pane, no way to
send input from several places at once (tmux/screen-style), and no
web-friendly transport — the only live-attach protocol is the binary bincode
client protocol (v13).

The PR-autopilot dashboard wants a full terminal emulation of an agent's pane
in the browser (xterm.js): live view AND typing, from several devices at once,
without kicking the terminal the user is sitting in.

## Goal

1. **Multi-seat attach** in the herdr server: N clients attached to one
   terminal; every seat's input reaches the PTY; every seat receives frames.
2. **`terminal.attach_stream`** — a JSON socket-API method exposing the same
   per-client frame stream + input channel over the existing JSON/NDJSON
   socket listener, so non-Rust clients (the dashboard's Node bridge) attach
   without speaking bincode.
3. **Size policy "terminals negotiate, web is passive"** — PTY winsize is
   negotiated only by size-negotiating clients (existing binary/TUI attach);
   stream clients declare `size_role: "passive"` and never affect winsize.
4. Dashboard consumer (separate plan): Node bridge socket↔browser + vendored
   xterm.js in the work-detail Console tab; input gated by a config token.

## Non-goals (YAGNI)

- No per-client PTY reflow (a PTY has one winsize; passive clients scale
  client-side). No change to how the program renders.
- No web server inside herdr (no HTTP/WS listener in Rust) — the JSON socket
  API is the boundary; bridging to browsers is the consumer's job.
- No auth inside herdr's socket API (local socket, same-user trust, as today).
  The dashboard bridge enforces the browser-facing token.
- No change to the binary protocol shape (no PROTOCOL_VERSION bump); only the
  server-side attach behavior changes (no more single-seat rejection).
- No recording/playback, no presence roster in v1 (seat count is queryable via
  existing client listing if needed later).

## Design

### 1. Multi-seat core (`src/server/headless.rs`, `src/server/clients.rs`)

- Replace `terminal_attach_owners: HashMap<String, u64>` with
  `terminal_attach_clients: HashMap<String, HashSet<u64>>`.
- `attach_terminal_client(client_id, terminal_id, takeover)`:
  - plain attach → insert into the set; never reject because the set is
    non-empty;
  - `takeover == true` → send `ServerShutdown { "terminal attach taken over" }`
    to every OTHER seat, clear them, insert self (semantics preserved:
    "kick everyone else", now plural).
- Input: `ServerEvent::ClientInput` from any client whose mode is
  `TerminalAttach`/stream-attach for terminal T → `apply_terminal_attach_input`
  (unchanged; it is already per-client and unconditional).
- Render fan-out: the render loop must deliver prepared frames to EVERY seat
  of a terminal, not just the foreground/owner. Each client keeps its own
  `render_state` baseline (already per-client), so diffing works per seat.
- Detach/disconnect: remove from the set; run
  `remove_client_and_resize_if_needed` (resize recomputes from remaining
  negotiating seats only).

### 2. Size policy

- Each attached client carries `size_role: Negotiating | Passive`.
  - Binary/TUI attach clients: `Negotiating` (today's behavior).
  - Stream clients: role comes from the `attach_stream` request; the dashboard
    always sends `"passive"`.
- `effective_size` for a terminal = min-bbox (min cols × min rows) over its
  **negotiating** seats only. Zero negotiating seats → keep current size
  (existing "virtual frame with no attached clients" path, headless.rs:3004).
- Passive clients receive frames at the terminal's current size and
  scale/letterbox client-side (xterm.js font-size fit). They may not send
  `resize`; the server ignores/rejects `resize` from passive seats.
- v2 (explicitly deferred): passive `suggest_size` applied only when no
  negotiating seat exists (autopilot panes have no attached terminal).

### 3. JSON socket API: `terminal.attach_stream`

Rides the existing JSON socket listener and framing (same request envelope the
CLI uses; NDJSON on the wire; no new dependencies).

Request:

```json
{ "method": "terminal.attach_stream",
  "params": { "terminal_id": "w653…-2", "mode": "interactive", "size_role": "passive" } }
```

`mode`: `"interactive"` (input allowed) | `"view"` (server drops input from
this seat). The dashboard uses `view` for token-less browsers and
`interactive` for token-holders.

Server → client (stream, one JSON object per line):

```json
{ "type": "snapshot", "cols": 200, "rows": 48, "data": "<base64 ANSI full frame>", "cursor": {"row": 47, "col": 12, "visible": true} }
{ "type": "frame",    "data": "<base64 ANSI delta>", "cursor": {…} }
{ "type": "resize",   "cols": 120, "rows": 40 }
{ "type": "detached", "reason": "terminal attach taken over" }
```

- On subscribe the server sends one full `snapshot` (reset the seat's
  `render_state` baseline, force a full redraw — the mechanism exists:
  `reset_baseline` + forced fresh frame, headless.rs:738-739), then diffed
  `frame`s from the same `prepare_frame` pipeline the binary attach uses,
  re-encoded as base64 in JSON events. `resize` is emitted whenever
  `effective_size` changes.

Client → server (same connection):

```json
{ "type": "input", "data": "<base64 bytes>" }
{ "type": "resize", "cols": 100, "rows": 30 }   // negotiating seats only
{ "type": "detach" }
```

- `input` from a `view`-mode or passive-with-`resize` seat → ignored with a
  `{"type":"error","code":"input_not_allowed"}` event (once), stream continues.
- Connection close == detach.

### 4. CLI (optional, thin)

`herdr terminal watch <target>` (or extend `agent attach`) is NOT required for
the dashboard and is deferred; the API is the deliverable. (Existing
`agent attach` keeps its binary path and gains multi-seat behavior for free.)

### 5. Consumer: dashboard bridge + Console tab (separate plan, second)

- Node (zero-dep): per browser client, `net.createConnection(herdr.sock)` →
  `terminal.attach_stream`; bridge to the browser over a minimal hand-rolled
  WebSocket (RFC 6455, text frames, ~150 lines; fallback: SSE + POST input).
- Browser: vendored xterm.js (~290 KB, served by dashboard.mjs, no npm) in the
  work-detail Console tab; `term.onData` → `input` messages; `snapshot`/
  `frame` → `term.write`; fit-to-width via dynamic font sizing.
- Input gating: `workAttachToken` in `~/.pr-bot/config.json`. Browser supplies
  it once (stored in localStorage). Bridge maps token → `mode:"interactive"`,
  no/bad token → `mode:"view"`. Read stays open as today; write requires the
  token. Server-side check only; token never rendered into HTML.
- Existing 1.5s-poll drawer and `/api/pane/:id/output` stay (fallback when the
  stream API is unavailable, e.g. older herdr).

## Sizing UX summary (the user's core question)

Every viewer sees the FULL grid in their own physical size: web clients scale
the grid client-side (phone = smaller glyphs or pan, ultrawide = larger), and
never shrink anyone else's terminal, because they don't participate in winsize
negotiation. Real terminals keep today's behavior; multiple real terminals =
min-bbox (tmux parity).

## Error handling

- `attach_stream` on unknown terminal → standard JSON error response
  (`terminal_not_found`), no stream.
- Slow stream consumer: bounded per-client send queue (exists —
  "render queue full, deferring latest retained frame"); on overflow keep
  dropping intermediate frames, never block the server; the next frame is
  always a valid delta against the seat's committed baseline (or force a
  fresh snapshot on overflow).
- Takeover while streaming → `{"type":"detached","reason":…}` then close.
- Dashboard bridge: herdr socket unreachable → Console tab falls back to the
  existing polling view with a notice.

## Testing

Rust (`headless.rs` unit tests, extending the existing attach tests):
- two clients attach plain → both in the set, both receive frames;
- input from both seats reaches the PTY runtime;
- takeover kicks all other seats (plural);
- passive seat never changes `effective_size`; negotiating seats min-bbox;
- detach of one seat → remaining seats keep streaming, resize recomputed;
- `attach_stream` delivers snapshot-then-deltas; `view` mode input rejected
  with `input_not_allowed`;
- stream on unknown terminal errors cleanly.

Dashboard (later plan): pure helpers unit-tested (frame decode, token check);
end-to-end via Playwright against a live pane.

## Rollout

- herdr: implement on a new branch off the repo's current base; build
  `target/release/herdr` (the `~/.local/bin/herdr` symlink picks it up);
  **restarting the herdr server kills live agent sessions** — deploy in a calm
  window (or via the preview channel).
- Dashboard consumer ships second, feature-detecting `terminal.attach_stream`
  (falls back to polling on older servers).

## Acceptance

- Two terminals + two browsers attached to one agent pane: all four see the
  same live screen; typing from any of them reaches the agent; nobody is
  kicked; closing any one changes nothing for the rest.
- A phone viewing a 200×48 pane sees the whole grid scaled down; attaching it
  never resizes the real terminal's view.
- Browser without the token: sees the live stream, cannot type
  (`input_not_allowed`), UI labels it read-only.
- Existing single-terminal workflows (attach, takeover, resize-on-detach)
  behave exactly as before.
