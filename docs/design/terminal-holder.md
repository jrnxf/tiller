# Terminal holder

- **Status:** accepted Wednesday 2026-11-11 (Lisbon time), with ADR 0004. Written the same day.
- **Decision record:** [ADR 0004](../adr/0004-own-terminal-holder.md).
- **Replaces:** herdr as the owner of agent panes. [`herdr-layout.md`](herdr-layout.md) becomes history at cutover.
- **Owner choices (Wednesday 2026-11-11):**
  - A separate holder process owns the PTYs. The daemon is a client of the holder.
  - The board is the main pane UI. `tiller attach` gives a real terminal.
  - There is no herdr backend and no fallback trait. `crates/herdr` and the herdr parts of the Nix module go at cutover.
  - `tiller attach` shows one pane at a time, plus a switcher (layout A in the mock). It has no sidebar.
  - The holder upgrades itself live, with a re-exec, whenever the binary changes.
  - Owner notifications go to the owner's desktop notification app, which shows them as cards.
  - A builder records raw PTY fixtures of real Claude Code sessions, after the owner gives the go.

This doc is the design. The contract changes it needs in `docs/spec/` land in task H01, after this doc and before any code. The specs tag each changed rule [H]: it states the behavior after the cutover (section 11), and herdr does that work until then.

## 1. Why

herdr owns every agent pane today. Tiller reaches it through `crates/herdr` (about 5,000 lines): a CLI client, a poller, a layout repairer and a terminal mirror built on herdr's observe and control streams. That gives four problems:

1. **Fidelity of the contract.** Tiller works around herdr's ideas: workspaces, tabs, row tokens that a herdr restart loses (R18), pane ids that herdr reuses, and an agent status that reads `idle` through a whole turn (`protocol.md` 4.5).
2. **No positive death evidence.** herdr does not report an agent's exit code. The liveness ladder must guess from a signal-0 probe.
3. **Remote panes are second class.** A saved herdr machine has no stream and no input, only a 5-second tail.
4. **Two upgrade stories.** A herdr upgrade kills every pane, so it waits for `herdr server stop` by hand.

A holder that Tiller owns fixes all four. It keeps a stable pane id, the session key and the metadata. It reaps its children, so it knows each exit code. It has one protocol for the board, `tiller attach` and the peer link. It upgrades in place without a lost pane.

## 2. The holder

### 2.1 One holder per host

One holder process per host and user owns every pane on that host. It runs as `tiller holder`, from the same binary as the daemon.

| | One holder per host (chosen) | One holder per session (dtach, abduco) |
|---|---|---|
| Processes and sockets | one | one per agent |
| Enumerate panes | one `List` call | scan a socket directory and probe each file |
| Metadata and keys | one table | spread across processes |
| Upgrade | one re-exec | one re-exec per session, and mixed versions for hours |
| Limits (pane count, memory) | enforced in one place | cannot be enforced |
| Blast radius of a holder crash | every pane on the host | one pane |

The blast radius is the only point for one holder per session. The design answers it in three ways:

- The holder is small. It has no network listener, no SQLite, no HTTP and no model calls. Its work is PTYs, the screen model and one socket.
- Each pane's model update runs inside `catch_unwind`. A panic in the model resets that pane's model only (section 3.5). The PTY, the child and every other pane keep running. The holder's release profile keeps `panic = "unwind"` for this reason.
- A lost holder is recovery row R21 (section 6.4). It reuses the reboot restore, which resumes every session with `claude --resume`.

### 2.2 Process and service

- **Linux (hosts B and the like):** a systemd user unit, `tiller-holder.service`. It is a sibling of the daemon's unit, not a child. With `KillMode=control-group`, a holder that the daemon spawned would die when the daemon unit restarts. Its own unit avoids that.
  - `ExecStart` names the stable profile path (`%h/.nix-profile/bin/tiller holder`, or the home-manager profile path), never a store path.
  - `X-RestartIfChanged=false` and `X-StopIfChanged=false`. A home-manager switch never restarts or stops it. A new binary reaches it only through the live upgrade (section 6.2).
  - `Restart=on-failure`, `KillMode=process`. A restart after a crash starts an empty holder (R21).
- **macOS with a user service (host A):** a launchd agent, `<reverse-dns>.tiller.holder`, with `KeepAlive` on a crash only. Its plist names the stable profile path, so it is the same file across versions and home-manager does not reload it.
- **No user service (host C, MDM blocks background items):** `tiller daemon` and `tiller attach` start the holder themselves when its socket does not answer. They run `tiller holder --detach`, which double-forks, calls `setsid` and closes the terminal's fds. The holder then outlives the terminal that started it. This keeps the fallback row in `host-capabilities.md`.
- **One holder at a time:** the holder takes an exclusive `flock` on `holder.lock` in its run directory before it binds. A second holder exits 0 with "a holder already runs (pid N)". The lock fd survives the live upgrade (section 6.2), so the lock is never free while a holder process lives. `tiller holder --detach` tries the lock before anything else, so a daemon on host C that starts a holder during an upgrade gets "already runs" and waits for the socket.

### 2.3 Socket and security

- **Path:** `$TILLER_HOME/run/holder.sock`, with `$TILLER_HOME/run` at mode `0700`. The default is `~/.tiller/run/holder.sock`. `[holder] socket` in `config.toml` overrides it with an absolute path. The path is checked against the 104-byte `sun_path` limit of macOS at start.
- **Mode:** the holder binds with `umask 077` and then sets `0600` on the socket.
- **Peer check:** on every accept the holder reads the peer's uid (`SO_PEERCRED` on Linux, `getpeereid` and `LOCAL_PEERPID` on macOS). A uid that is not the holder's own gets no reply and the socket is closed. The holder logs it with the uid and pid.
- **No network:** the holder never opens a TCP or UDP socket. A test checks this by listing the holder's fds.
- **Stale socket:** only a holder that holds the lock may remove a socket file. It removes one that does not answer a connect, and binds again.
- **Same-uid agents.** An agent runs as the same uid, so the socket is not a boundary against an agent. That is the same as herdr's socket today. The role sandbox keeps the boundary. It denies `tiller holder`, `tiller attach` and any path under `$TILLER_HOME/run` to agents, as it denies `herdr` today (`roles/*.settings.json`, `guards/bash.rs`, `guards/live.rs`).

### 2.4 The protocol

The socket carries frames. Each frame is `[len: u32 big-endian][kind: u8][body]`. `len` counts `kind` and `body`. A frame over 16 MiB ends the connection.

| `kind` | Body | Use |
|---|---|---|
| 0 | one JSON message (UTF-8) | requests, replies and events |
| 1 | `[stream: u32 big-endian][bytes]` | pane output to a subscriber |
| 2 | `[num: u32 big-endian][bytes]` | raw input from a client, to the pane with that `num` |

JSON keeps the control path easy to read in logs and easy to extend. Bulk bytes never go through JSON or base64. The `u32` in kinds 1 and 2 is a number the holder gave in a reply, so a frame never carries a string id.

**Handshake.** The client sends `hello` first, and the holder answers `welcome` or `refused`:

```json
{"type":"hello","proto":[1,1],"client":"daemon","version":"0.1.0-pre.30"}
{"type":"welcome","proto":1,"holder":"0.1.0-pre.30","instance":"01JD...","boot_id":"...","started_at":"2026-11-11T12:00:00Z","features":["kitty_keys","sync_buffer"]}
```

- `proto` is the range of major versions that the client speaks. The holder picks the highest one that both sides speak, or answers `refused` with its own range.
- Within one major version, a side ignores unknown fields and answers an unknown request type with `{"type":"error","code":"unsupported"}`. A new request type or field is a minor change. A changed meaning is a new major version.
- A holder supports its own major version and the one before it. A daemon supports the same two. So a daemon upgrade and a holder upgrade can land in either order.
- `instance` is a new ULID each time a holder starts from nothing. A live upgrade keeps it (section 6.2). A changed `instance` tells the daemon that the old holder and all its panes are gone (R21).

**Requests.** Each request carries an `id` (a number the client picks), and its reply carries the same `id`. Requests on one connection may overlap.

| Request | Reply | What it does |
|---|---|---|
| `spawn {key, argv, cwd, env, size, meta}` | `pane {pane}` | Starts a child on a new PTY. `argv[0]` must be an absolute path: the daemon resolves it, and the holder does no PATH search. `key` is the session id. A second `spawn` with a key that a running pane holds returns that pane with `existing: true` and starts nothing |
| `list {}` | `panes {instance, panes: [pane]}` | Every pane, running or exited |
| `get {pane}` | `pane {pane}` | One pane, or `not_found` |
| `screen {pane, read, format}` | `screen {...}` | `read` is `visible` or `{recent: lines}` (1 to 10,000). `format` is `text` or `ansi`. The reply holds `rows`, `cols`, `cursor`, `modes` and `rows_text` |
| `input {pane, text}` | `ok` | Types text as keystrokes, with no paste brackets |
| `paste {pane, text}` | `ok` | Sends text inside bracketed paste when the app turned mode 2004 on, else as `input` with each line ended by a carriage return |
| `keys {pane, keys: [name]}` | `ok` | Named keys (section 4.3), encoded for the app's current keyboard mode |
| kind 2 frame | none | Raw bytes from `tiller attach`, passed to the PTY as they are |
| `resize {pane, cols, rows, lease}` | `ok {cols, rows}` | Sets the PTY size through a lease (section 2.6) |
| `release {pane, lease}` | `ok {cols, rows}` | Drops a lease, and the size goes back to the next lease |
| `subscribe {pane, sync}` | `subscribed {stream, cols, rows}` | Starts kind 1 frames: one repaint, then live output (section 2.7) |
| `unsubscribe {stream}` | `ok` | Stops them |
| `meta {pane, set, remove}` | `pane {pane}` | Merges string keys into the pane's metadata |
| `signal {pane, signal}` | `ok` | `int`, `term`, `hup` or `kill`, to the child's process group |
| `close {pane, grace_ms}` | `exited {pane}` | `hup` to the group, then `kill` after `grace_ms` (default 3,000). It answers when the child is reaped. When the child exits first, what is left of the group still gets the `kill` at the deadline |
| `forget {pane}` | `ok` | Drops an exited pane and its screen. A running pane is refused |
| `watch {}` | `watching` | Starts `event` messages on this connection |
| `switch {pane}` | `ok {clients}` | Tells every attach client on this host to show `pane`: each one's watch gets a `show {pane}` event, and `clients` counts them (section 5.3) |
| `upgrade {binary, dry_run}` | `upgrading` or `upgrade_refused {reason}` | Section 6.2 |
| `ping {}` | `pong {}` | Health check |

A `pane` record is:

```json
{"pane":"p01JD...","num":7,"key":"<session ulid>","pid":4242,"argv":["claude","--resume","..."],
 "cwd":"/home/user/src/alpha","started_at":"...","state":"running","exit":null,
 "cols":120,"rows":40,"meta":{"label":"alpha","project":"alpha","role":"orchestrator","session":"..."},
 "title":"alpha","last_output_at":"...","attach_clients":0,"subscribers":1}
```

- `pane` is a holder id with a `p` prefix and a ULID. The holder never reuses one. `num` is a short number for kind 2 frames, unique within one `instance`. A subscription's `stream` number is a different counter. A herdr pane id could come back after a restart, which forced the token checks in `outbox.rs` and `control.rs`.
- `state` is `running` or `exited`. `exit` is `{code, signal, at}` once the holder has reaped the child. An exited pane keeps its last screen until `forget`, so the board can show the last frame and the ladder has positive evidence.
- `meta` holds the old row tokens (`project`, `card`, `role`, `run`, `model`, `session`) and `label`. It lives in the holder, so a daemon restart cannot lose it, and a live upgrade carries it.

**Events** (after `watch`): `spawned`, `exited {pane, exit}`, `resized {pane, cols, rows}`, `meta {pane}`, `title {pane, title}`, `bell {pane}`, `notify {pane, title, body}` (OSC 9 or OSC 777 from the app), `quiet {pane, at}` (no output for 500 ms after output), `model_reset {pane}` (section 3.5), `forgotten {pane, reason}` (by `forget`, or by a limit in section 2.8), `upgraded {holder}`, and `show {pane}` (after a `switch`, to the watches of attach clients only: a connection whose hello names the client `attach`). Events replace the herdr poller's 2-second snapshot. The daemon still runs a `list` every 30 s as a check, and once after each reconnect.

**Errors** are `{"type":"error","id":n,"code":c,"message":m}`. The codes are `not_found`, `exited`, `bad_request`, `unsupported`, `busy` (a limit, section 2.8) and `internal`.

### 2.5 Child processes

- The holder opens the PTY pair itself with `rustix` (`openpt`, `grantpt`, `unlockpt`, `ptsname`). It does not use `portable-pty`, because the upgrade needs the raw master fd and its `FD_CLOEXEC` flag.
- The child runs `setsid`, takes the slave as its controlling terminal (`TIOCSCTTY`), resets its signal mask and dispositions, and closes every other fd before `execve`.
- **Environment.** The holder does not pass on its own environment. The child gets exactly the `env` in `spawn`, plus `TERM=xterm-256color`, `COLORTERM=truecolor` and `TILLER_PANE=<pane>`. The daemon builds the env, as `spawn/launch.rs` does today, and that env already holds `TILLER_SESSION_ID` (`spawn/launch.rs:382`). This removes the herdr rule that "a pane does not inherit the daemon environment" (`spawn/launch.rs:386`), because the daemon now says what the pane gets.
- **Reaping.** The holder handles `SIGCHLD` and calls `waitpid` for each child. It records the exit code or signal, sends `exited`, and keeps the pane until `forget`. A child's grandchildren are not the holder's concern. `close` signals the whole process group.
- **Drain.** The holder always reads every PTY, whether or not anyone watches. A child never blocks on a full PTY buffer because no client reads.

### 2.6 Size and leases

A PTY has one size. Many clients may want different sizes. The holder keeps a stack of leases per pane:

- `spawn` sets the base size (`[holder] default_size`, such as `"100x30"`; default 120 by 40).
- A lease belongs to the connection that made it. Its id is the client's own string.
- `resize` with a new lease id pushes a lease. `resize` with a lease id the connection already holds changes that lease and moves it to the top. The top lease sets the PTY size, and the holder sends `SIGWINCH` through `TIOCSWINSZ`. The `resized` event names the top lease.
- `release`, or the end of the connection that holds the lease, removes it. The size falls back to the next lease. When no lease is left, the size stays as it is, so the app does not redraw for nothing.
- An attach client takes a lease on start. When its owner types and its lease is not the top one, it sends `resize` again with its own size, so the size follows the person at the keyboard. A board Fit takes a lease while its stream is open. A read-only viewer takes none.
- A live upgrade ends every connection, so it ends every lease (section 6.2). The PTY keeps its size. Each client takes its lease again when it reconnects.

This replaces `~/.tiller/mirror-fits.json` and `Fits::restore_left`. A daemon that dies mid-Fit closes its connection, so the holder pops its lease at once. Nothing needs a size given back later.

### 2.7 Live output

A subscriber gets one **repaint**, then **passthrough**:

- **Repaint.** The holder renders the pane's model into a byte string that draws the current screen on an empty terminal of the same size (section 3.4). It is the first kind 1 frame, and a JSON `repaint` message comes before it with the size. The board's first frame, the first frame of `tiller attach` and the upgrade handover all use the same renderer.
- **Passthrough.** After the repaint, the subscriber gets the PTY's own bytes, as the child wrote them, less the query sequences (below). Raw bytes keep full fidelity: the board's xterm.js or the owner's terminal draws what the agent drew.
- **Queries.** An app asks its terminal questions: DA1, DA2, cursor position (CPR), DECRQM, the kitty keyboard query, XTVERSION, the window size reports (`14t`, `18t`), and OSC 4, 10, 11 and 12 colors. With many viewers, many terminals would answer, or none would. So the holder answers them from its model, as one terminal, and cuts them from the passthrough stream. A test feeds each query and checks one answer on the PTY and none in any subscriber's stream. The answer to XTVERSION is `tiller <version>`.
- **Synchronized output (mode 2026).** The model holds a frame until its end marker, so a `screen` read never sees half a frame. The passthrough stream keeps the markers. A subscriber can ask `sync: "buffer"`, and the holder then holds that subscriber's bytes from the start marker to the end marker (at most 150 ms or 1 MiB) and sends them as one frame. The board asks for it, because the board's xterm.js may not honor mode 2026. `tiller attach` asks `sync: "pass"`, because modern terminals honor it.
- **Back-pressure.** Each subscriber has a queue of 4 MiB. Past it, that stream's queued live output is dropped and the stream gets `{"type":"lagged","stream":n}`. The connection and its other streams go on, and the client may subscribe again for a fresh repaint. The PTY reader never waits for a subscriber.

### 2.8 Limits

| Limit | Value | When passed |
|---|---|---|
| Panes per holder (running and exited) | 128 | `spawn` answers `busy` |
| Exited panes kept | 64, oldest dropped first, and none older than 24 h | dropped with a `forgotten` event |
| Scrollback per pane | 10,000 rows | oldest rows dropped |
| Subscribers per pane | 16 | `subscribe` answers `busy` |
| Connections | 64 | new connections are closed |
| Frame size | 16 MiB | the connection ends |
| Subscriber queue | 4 MiB | that stream's queued output is dropped and it gets `lagged`, and the connection stays |
| One reply frame, against a connection's 4 MiB queue | 4 MiB | the request answers `busy`, and the connection stays |
| A client's input the app left unread | 4 MiB | `input`, `paste` and `keys` answer `busy`, and raw input cuts the client |
| Leases per pane | 64 | `resize` with a new lease id answers `busy` |
| Requests of one connection that are answered later (`spawn`, `screen`, `close`) and not answered yet | 64 | the request answers `busy` |
| A pane's `title`, `bell` and `notify` events | a burst of 20, then one per 50 ms | a title waits for the pane's `quiet`, and a bell or a notification is dropped |

## 3. The screen model

### 3.1 The pick: `alacritty_terminal` 0.26

I compared `vt100` 0.18.1 and `alacritty_terminal` 0.26.0 in a throwaway crate on Wednesday 2026-11-11. Both read the nine Claude Code fixtures, then in `crates/herdr/tests/fixtures/claude/` and now in `crates/panes/tests/fixtures/claude/`. Each fixture was fed in, and the model's screen was read back and given to `composer::classify`.

| Check | `vt100` 0.18 | `alacritty_terminal` 0.26 |
|---|---|---|
| Composer verdict on all 9 fixtures, against the original | same | same |
| Row text on all 9 fixtures | same | same |
| Synchronized output: read in the middle of a frame | shows the half-drawn frame ("new fra") | shows the last whole frame until the end marker |
| Answers terminal queries (DA1, DA2, CPR, kitty `?u`, OSC 11, DECRQM 2026, `18t`) | no response path | answers all of them, except XTVERSION |
| Kitty keyboard push and pop | not tracked | tracked |
| Bracketed paste mode | tracked | tracked |
| Wide characters (CJK, emoji) | correct | correct |
| Reflow from 40 to 120 columns | keeps 3 rows | joins them into 1 row |
| Scrollback | 196 lines held | 196 lines held |

`alacritty_terminal` wins on every point where the two differ. The points that matter most:
- **Sync output.** A Claude Code redraw is one synchronized frame. A half frame read by the composer could give a false `Holds` or `Unknown` verdict.
- **The query path.** Answers from the model are what let many viewers share one pane (section 2.7).
- **Kitty keyboard mode.** The holder must know the mode to encode named keys (section 4.3) and to let `tiller attach` read its prefix (section 5.2).

**Version pin.** `alacritty_terminal` is pinned with `=0.26.0` in `Cargo.toml`. A bump is its own task, and it must pass the fixture gate (section 9.1).

### 3.2 What the fixtures do not prove

The nine fixtures are herdr's renderings of a viewport: 119 columns, rows split by `\r\n`. They are not raw PTY streams. They prove that the model reads a screen the same way. They do not prove that the model follows Claude Code's own cursor moves, sync frames and erase sequences. Task H08 records raw streams of real sessions (with the owner's go), and the gate then replays them (section 9.1).

### 3.3 Reading the screen

`screen` gives the parsers what `herdr pane read` gave them:
- `rows_text`: one string per row, with trailing spaces trimmed and rows joined by `\n`.
- With `format: "ansi"`: SGR escapes for color and attributes, in the shape `composer::classify` and `prompt.rs` read today.

The gate in section 9.1 compares the parsers' results, not bytes, because the escapes may differ in harmless ways (the order of SGR parameters, for example).

### 3.4 The repaint renderer

Tiller writes its own renderer over `alacritty_terminal`'s grid. `alacritty_terminal` has no ANSI output. The renderer emits:
1. A reset (`ESC c` is not used, because it clears the outer terminal's own state. The renderer uses `CSI ! p` and an explicit mode list).
2. The alternate screen, if the app is on it.
3. Every row of the visible grid, with the minimum SGR changes, wide characters and their spacer cells, and the line-wrap flag kept for reflow.
4. The modes the app turned on: bracketed paste, application cursor keys, application keypad, mouse modes, focus events, cursor shape and visibility, and the kitty keyboard flags (as one push).
5. The cursor position, last.

For the board's history and for `tiller attach` scroll, the renderer can also emit the scrollback rows above the screen.

The repaint is for a viewer, so it draws what a viewer sees: the active screen and its modes. It does not carry the primary screen under an alternate screen, the saved cursor, the scroll region or the charsets. A viewer does not need them, because the app's next bytes go to the viewer's own terminal state. The live upgrade does need them, so it does not use the repaint (section 6.2).

A property test (proptest) checks that a repaint fed into a fresh model gives the same active grid, cell by cell, as the model it came from.

### 3.4.1 Whole sequences only

The holder feeds the model only whole escape sequences and whole UTF-8 characters. A small scanner in front of the model finds where the last read ends. When it ends inside a sequence (ESC, CSI, OSC, DCS, APC or a split UTF-8 character), the holder holds that tail back and feeds it with the next read. A held tail over 64 KiB is fed as it is. Passthrough to subscribers does not wait for the scanner. So the model's parser is in its ground state between reads, and the upgrade can hand over the held tail as plain bytes. The one exception is a tail over 64 KiB, which the parser has already read: until that sequence ends, a live upgrade is refused (section 6.2).

### 3.5 A model that panics

Each pane's model update (`vte::ansi::Processor::advance` into the pane's `Term`) runs inside `catch_unwind`. On a panic:
1. The holder logs the panic and the last 4 KiB of input.
2. It makes a fresh model at the same size, with the modes the app set (the alternate screen, keyboard, mouse, paste and cursor modes), and sends the child `SIGWINCH`, so the app redraws. An app sets its modes once and does not set them again when it redraws.
3. It cuts every subscriber of that pane (`lagged`), and they subscribe again for a repaint.
4. It sends the event `model_reset {pane}`. The daemon records it as an observation.

The child and its PTY are not touched.

## 4. The Tiller interface

### 4.1 Crates

| Crate | What it holds |
|---|---|
| `crates/panes` (`tiller-panes`) | The protocol types and framing, the `Panes` and `Live` traits, the socket client (`HolderClient`), and the parts of `crates/herdr` that are about panes but not about herdr: `composer.rs`, `submit.rs`, `process.rs`, the key names, `front.rs` as `open` (the `Terminal` trait, `MacApp`, `Opened`), and `fake.rs` and `testing.rs` as `FakePanes` and `testing::Fleet` |
| `crates/poll` (`tiller-poll`) | The generic poller from `herdr/src/poller.rs`: `Source`, `PollError`, `Poller`, `Registry`, `Health` and `backoff`. Many daemon and delivery pollers use it and have nothing to do with panes, so it gets its own crate. `HerdrSource` and `verdict` do not move |
| `crates/holder` (`tiller-holder`) | The holder: PTYs, reaper, model, renderer, query responder, leases, socket server and upgrade |

`tiller-panes` and `tiller-poll` depend on no terminal crate. The daemon, the agents crate and the CLI depend on `tiller-panes`. Only the CLI binary (for `tiller holder`) depends on `tiller-holder`.

### 4.2 The trait

The trait follows the holder, not herdr. It has no workspaces, tabs, layout or ordering. A pane is a child process with a key, metadata and a screen.

```rust
pub trait Panes: Send + Sync {
    /// Starts a child, or returns the running pane that already holds `req.key`.
    fn spawn(&self, req: &Spawn) -> Result<Pane>;
    /// Every pane, with the holder instance it came from.
    fn list(&self) -> Result<Listing>;
    fn get(&self, pane: &PaneId) -> Result<Option<Pane>>;
    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen>;
    fn input(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn paste(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()>;
    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane>;
    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()>;
    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit>;
    fn forget(&self, pane: &PaneId) -> Result<()>;
    fn switch(&self, pane: &PaneId) -> Result<usize>;
}

/// Streams that hold a connection open. Separate from `Panes` so that a fake
/// for most tests does not have to implement them.
pub trait Live: Send + Sync {
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription>;
    fn watch(&self) -> Result<Events>;
}
```

- `Subscription` yields `Repaint { cols, rows, bytes }`, then `Output(bytes)`, `Resized`, `Exited` and `Lagged`. It holds an optional size lease, so `subscription.resize(cols, rows)` is the board's Fit.
- `PanesError` (with `thiserror`) has `NotFound`, `Exited`, `Unreachable`, `Refused { request, code, message }`, `Version { client, holder, message }`, `Protocol` and `Io`. `Unreachable` means "unknown, never gone", as `HerdrError::Unreachable` does today (`herdr/src/client.rs:28`).
- `Notify` is not on the trait. It is its own trait in `tiller-delivery` (section 7).

### 4.3 Key names

The board and the CLI send key names. Today they are "named as `herdr pane send-keys` names them" (`dto.rs:2139`). They become Tiller's own list, with the same spelling, so the board does not change:
- The names `board/src/components/board/terminal-keys.ts` and `daemon/src/prompt.rs` send today, all lowercase: `enter`, `esc`, `tab`, `backspace`, `space`, `up`, `down`, `left`, `right`, and the rest of that file's table.
- Modifier prefixes `ctrl+`, `alt+` and `shift+`, as in `ctrl+c` and `shift+tab`. A name prints them in that order, but parses them in any order, each at most once, because the board sends `alt+ctrl+x`.
- A single printable character.

The holder encodes each name for the app's current mode: legacy, application cursor keys, or the kitty keyboard flags that the app pushed. The list lives in `tiller-panes::keys`. H02 builds it from those two files, not from this summary, and tests check that every name they can emit parses. No test reads their source as text: `terminal-keys.ts` exports `keyNames()`, a board test keeps it equal to what `typedKeys` sends and to the committed `crates/panes/tests/fixtures/board-key-names.json`, and `tiller-panes` parses each name in that fixture. The daemon's prompt test parses each name `Dialog::keys` sends.

### 4.4 Every herdr call path, mapped

| Path today | Where | With the holder |
|---|---|---|
| Worker spawn: `create_workspace(label, worktree)`, `report_tokens`, `relayout`, `send_text(launch line)`, `send_enter` | `agents/spawn/mod.rs:576-598` | One `spawn { key: session, argv: [claude, ...], cwd: worktree, env, meta }`. The launch line is gone: the holder runs `claude` directly, not a shell that types it. `relayout` goes away |
| Orchestrator spawn | `agents/spawn/orchestrator.rs:237,305-309` | The same `spawn` |
| Resume after a crash or a reboot | `agents/control.rs:1269-1298`, `reconcile/sources.rs:203-262` (`HerdrLauncher`) | `spawn` with `claude --resume <id>`. `HerdrLauncher` becomes `PaneLauncher` |
| Workspace label, block order, glyphs | `herdr/layout.rs`, `relayout`, `layout::repair` | The label moves to `meta.label`, and `agents::labels` makes it (from `worker_label` and `orchestrator_label`). Order is not stored: the switcher and the board sort by project, then role, then start time. `rename_workspace`, `move_workspace` and `repair` go away |
| Row tokens re-reported after a herdr restart (R18) | `reconcile/pass.rs:271-273` | Goes away. `meta` lives in the holder and survives a daemon restart and a live upgrade |
| Snapshot for reconcile | `reconcile/pass.rs:161`, `recovery/evidence.rs:30-48`, `ladder.rs` | `list`. The pane's `key` replaces `tokens["session"]`, and `state`/`exit` replace `agent` and `agent_status` |
| Agent alive probe | `reconcile/sources.rs:26-30` (`HerdrStatusProbe`) | `state == "running"`, and `kill(pid, 0)` as a cross-check. An `exited` pane is positive evidence of death, with its exit code |
| Target check before typing | `delivery/outbox.rs:506` | `get(pane)` and `pane.key == session.id`. The holder never reuses a pane id, so a mismatch means a bug, and it fails loudly |
| Composer read and line submit with Enter pacing | `delivery/outbox.rs:277-295,537-557`, `herdr/submit.rs` | `screen(visible, ansi)` and `classify` as today. `submit.rs` moves to `tiller-panes` with no change in logic: `input(line)`, wait `Pacing.settle`, `keys(["enter"])`, read back, press again up to `PRESSES` (4). Section 4.5 says why it stays |
| Interrupt | `agents/control.rs:224-258` | `keys(["esc"])`, and an observation with source `holder` |
| Stop: Escape, `/exit` at an empty composer, prove the agent gone | `agents/control.rs:651-790` | The same steps. "Gone" is now the `exited` event, with no polling |
| Stop an orchestrator, close a pane, teardown | `control.rs:848`, `spawn/mod.rs:659,1166`, `control/project.rs:280` | `forget(pane)` once the stop steps proved the agent gone (the pane `exited`). When the owner forced the stop, `close(pane, 3 s)` first, then `forget`, as today. The `dead_panes` guard above covers clean |
| `dead_panes`: closes only the run's bare-shell panes, and refuses while one still runs an agent ("a live pane is never closed") | `spawn/mod.rs:1134-1165` | The check stays, on holder facts. A run's panes are those whose `key` is one of its sessions. Clean and teardown `forget` the `exited` ones, and refuse while any is `running`. No bare shell exists any more, because the holder runs `claude` with no shell under it |
| Owner notification | `delivery/notify.rs:70` | The `Notify` trait and the command notifier (section 7) |
| Open in herdr (focus) | `api.rs:5290` (`focus`), `herdr/front.rs`, `cli/cmd/spawn.rs:525-545` | Open in terminal (section 5.3): `switch` when an attach client runs, else start the terminal app with `tiller attach` |
| Live view (observe and control streams) | `daemon/mirror.rs`, `api.rs:4424` (`terminal`) | `Live::subscribe` (section 5.1) |
| Fit | `api.rs:4759` (`terminal_fit`), `mirror.rs` `Fits` | A lease on the subscription (section 2.6). `mirror-fits.json` and `restore_left` go away |
| Keys and paste from the board | `api.rs:4526` (`terminal_input`) | `keys` and `paste` |
| Screen read, prompt read and prompt answer | `api.rs:4645` (`prompt_read`), `api.rs:4691` (`prompt_answer`), `daemon/src/prompt.rs` | `screen(visible)`, then `keys(dialog.keys(option))`. That is a digit for a numbered dialog, or arrow keys and then `enter` for a cursor menu such as the folder trust dialog. `prompt.rs` does not change |
| History | `api.rs:4808` (`terminal_history`, `read_recent` at 4851, 1,000 rows) | `screen({recent: lines})`, still capped at 1,000 rows for the API |
| Tail for a saved machine | `api.rs:4873` (`terminal_tail`), `cli/cmd/run.rs:247-275`, `views.rs` `pane_tail_of` | Local panes: `screen(visible)`. Saved machines go away (section 8). A peer's tail is a `query` on the peer link, which the peer answers with its own `screen(visible)` (HR2, `protocol.md` 7.4, Agent verbs on a peer). Its live view is the peer pane link |
| Remote poller per saved machine | `daemon/workers.rs:589-630` (`MachinesSource`, `HerdrSource::remote`) | Goes away. Peers have their own link (section 8) |
| Local poller | `daemon/workers.rs:809` (`HerdrSource::local`, in `start`) | `HolderSource`: `watch` events, plus `list` every 30 s and after each reconnect. The poller name is `holder` |
| Token counts reported to herdr | `report_tokens` | Goes away (it was herdr sidebar metadata) |
| Tool version | `daemon/workers.rs:228-234`, `reconcile/tools.rs` | `tools.holder` from `welcome.holder`. `tools.herdr` goes away |
| `tiller doctor` herdr check | `cli/cmd/doctor.rs:65-82` | A `holder` check: the socket answers, the proto matches, and the holder version equals the daemon's (a warning when it differs, since an upgrade is due) |
| `tiller admin` `stop_herdr` | `cli/cmd/admin.rs:386-450` | `close` for each of the project's panes |
| Peer pane link | `daemon/peer/pane.rs` | `Live::subscribe` and `screen({recent})` on the peer's own holder. The frames do not change |
| Replay sandbox | `replay/sandbox.rs`, `replay/audit.rs` | `FakePanes`. The audit counts verbs from `FakePanes::calls()` |
| The generic poller | `herdr/poller.rs` (`Source`, `PollError`, `Registry`, `Poller`, `backoff`), used by `daemon/src/{forge.rs,forge/gate.rs,memory/watch.rs,owner.rs,quota.rs,server.rs,sse.rs,workers.rs}`, `delivery/src/sources.rs` and `cli/src/daemon.rs` | Moves to `tiller-poll`. The import path changes, and `PollError::Herdr` becomes `PollError::Source`, made with `PollError::from_source`, because `tiller-poll` cannot name herdr's error |
| Test helpers | `herdr/testing.rs` (`Fleet`, `Pty`), `herdr/fake.rs`, used across the daemon, delivery, agents and reconcile tests | `tiller-panes::testing` over `FakePanes`. Each test keeps what it checks, and the herdr-only checks (layout, tokens) go |
| Terminal launcher | `herdr/front.rs` (`Terminal`, `MacApp`, `Opened`, `open_agent`) | `tiller-panes::open`, launching `tiller attach` in place of herdr (section 5.3) |
| Child process helper | `cli/cmd/learn.rs:21` (`process::{Ended, Interrupt}`) | Moves to `tiller-panes::process` unchanged |
| Role sandbox `Bash(herdr *)` | `roles/*.settings.json`, `agents/roles.rs:445`, `guards/*` | Deny `tiller holder*`, `tiller attach*` and `$TILLER_HOME/run/**` instead |

### 4.5 Why line submit keeps its loop

`paste` puts text inside bracketed paste when Claude Code has mode 2004 on, and Claude Code then reads an Enter after it as a submit. That could make the 500 ms wait unneeded. The port keeps the loop exactly as it is, because the fixture gate and `delivery/tests/outbox.rs` pin today's behavior. A later task can measure paste against raw stream fixtures and then shorten `Pacing`. The read-back verification stays either way, because it is the evidence that a message left.

### 4.6 Observations

herdr's `agent_status` (`working`, `idle`, `done`, `blocked`) goes away. The holder gives two better sources:
- **Exit.** The `exited` event, with the code or signal. It is positive evidence for the liveness ladder.
- **Screen.** On each `quiet` event, the daemon reads `screen(visible)` and runs `classify`. `Busy` gives `working`, `Empty` gives `idle`, and `Dialog` gives `blocked`, which maps to `waiting_input`, or to `waiting_permission` when a hook said so. `Holds` and `Unknown` give nothing.

The observation source `herdr` becomes `holder`. `observe/merge.rs` `herdr_verdict` becomes `holder_verdict` with the same rules, and the "`herdr:idle` held against a fresh hook `working` for 30 s" rule (`core/state/runs.rs:196`) applies to `holder:idle`. Hook verdicts still win when both are newer than the projection.

## 5. The board and the CLI

### 5.1 The board's live view

The endpoints and the NDJSON line kinds stay. Their meaning changes:

| Line | Today (herdr) | With the holder |
|---|---|---|
| `size` | the pane's PTY size | the PTY size, sent first and on each `resized` |
| `frame` with `full: true` | a full screen from herdr's observe | the repaint |
| `frame` with `full: false` | a diff on top of the last frame | raw passthrough bytes |
| `closed` | the pane closed | the child exited, with `reason` and the exit code |
| `unavailable` | herdr stopped answering | the holder stopped answering |
| `unfit` | herdr refused the Fit | not sent: a lease cannot be refused. The kind stays in the board's parser for one release |

- **Fit** is a lease on the stream's subscription. A board that changes size sends `.../terminal/fit`, and the daemon moves the lease. When the stream ends, the lease ends with it.
- **History** is `screen({recent: lines})`, at most 1,000 rows.
- **Bounds.** The daemon keeps 8 views at most, and each view keeps its queue of 64 lines. A view that falls behind is ended and opens again for a fresh repaint, as today.
- **Prompt** reads `screen(visible)` and answers with `keys(dialog.keys(option))`: a digit, or arrows and `enter` for a cursor menu. The check and the key are still two calls, so the window between them stays one round trip (`protocol.md` 7.4, Prompt).
- **Tail** for a local pane is `screen(visible)`. The saved-machine case goes away. A peer's tail, prompt read and answer run on the peer's own holder, reached as a `query` or a forwarded `mutate` (HR2, `protocol.md` 7.4, Agent verbs on a peer).
- **Copy.** "Open in herdr" becomes "Open in terminal". The reconnecting banner, the host checks and the degraded copy name the holder. Section 10 lists each file.

### 5.2 `tiller attach`

```
tiller attach [<run|session|card>] [--read-only]
```

With no target, it shows the switcher first.

- **Owner only (D12).** An agent never attaches. Before it touches the terminal, `tiller attach` runs the owner check of `authority::principal`: no `CLAUDECODE`, stdin a terminal, and no Claude Code process among its ancestors. A fleet caller gets `forbidden`, as `tiller peer serve` refuses one. `TILLER_SESSION_ID` plays no part, so a notification click, whose handler gets it (section 7), passes.
- **Raw mode.** It puts the owner's terminal in raw mode, subscribes with `sync: "pass"`, writes the repaint, then passes the bytes through. It takes a size lease at the window's size and moves it on a window change (it reads the size every 100 ms, so it needs no signal handler).
- **One screen.** The whole session runs on the owner's alternate screen, so the owner's own screen and scrollback come back untouched on detach. Attach takes the app's screen switches (`?1049`, `?1047` and `?47`) out of the repaint and the live bytes, and asks for a repaint when the app switches screens.
- **Input.** Keys go to the pane as kind 2 frames, as the terminal sent them, so the agent gets exactly what the owner typed. The lease moves to the top on each key: when the pane's size (from the repaint and the watch's `resized` events) is not the size this client last set, attach sends `resize` again before the key.
- **The prefix is `Ctrl-\`**, as in the demo the owner tried. After the prefix:
  - `d` detaches.
  - `s` opens the switcher.
  - `n` and `p` go to the next and the previous agent.
  - `Ctrl-\` sends one `Ctrl-\` to the agent.
  - `?` shows a help screen over the pane that lists every prefix key. It sends nothing to the pane. Any key closes it and asks for a repaint. That key is swallowed and never reaches the pane.
  - Any other key closes the prefix and does nothing.
  - `[attach] prefix` in `config.toml` can change the prefix key to another Ctrl chord: a letter, or `\` or `]`. Ctrl-i, Ctrl-j and Ctrl-m are refused, because they send the bytes of Tab and Enter.
  - A kitty press of a modifier key alone, such as the Shift before `?`, leaves the prefix waiting.
- **The kitty keyboard.** When the agent pushed kitty keyboard flags, the owner's terminal sends `Ctrl-\` as `CSI 92;5u`, not as byte `0x1c`. `tiller attach` knows the pane's mode from the holder and reads both forms.
- **Detach.** It drops the lease, then restores the terminal: it pops the kitty flags, turns off bracketed paste, mouse and focus modes, leaves the alternate screen, shows the cursor and resets SGR. Then it leaves raw mode. The pane runs on.
- **Many clients.** Any number of attach clients and board views can watch one pane. All of them see the same output. Input from all of them goes to the pane in the order it arrives, with no lock. The size follows the client that typed last.
- **Read-only.** `--read-only` sends no input and takes no lease. It still counts in `attach_clients`, which counts each pane's streams on attach connections.
- **The switcher (layout A).** An overlay drawn over the pane, listing every pane by project block, with a status glyph, the label and the age. The data comes from the holder's `list` and `meta`, so the switcher works while the daemon is down. When the daemon answers, its activity (`working`, `waiting_permission` and so on) gives the glyph. Otherwise the holder's `running` or `exited` does. Up and Down move, Enter attaches, and Escape closes it and asks for a repaint.
- **When the pane exits.** Attach shows a one-line note under the last frame ("the agent exited with code 0. Ctrl-\ s for the list, Ctrl-\ d to detach") and stays, so the last screen stays readable.
- **Errors.** No holder: "no holder runs on this host (start it with `tiller holder`, or run `tiller doctor`)". An ended session: it says so and opens the switcher.

### 5.3 Open in terminal

The focus endpoint (`POST /api/v1/agents/{s}/focus`), the board's O key and `tiller agent focus` keep their place. What they do:

| Attach client on this host | `[attach] terminal` set | What the daemon does | `opened` |
|---|---|---|---|
| yes | no | `switch` to the pane | `focused` |
| yes | yes | `switch`, then `open -a <terminal>` | `raised` |
| no | yes | `open -na <terminal> --args <terminal_exec> tiller attach <session>` | `launched` |
| no | no | nothing to show it in. The board shows the `tiller attach` command to copy | `unseen` |
| either | yes | the app does not open. `problem` says why | `failed` |

`[herdr] terminal` and `terminal_exec` become `[attach] terminal` and `terminal_exec`, with the same meaning. The holder knows its attach clients, so the probe is a `list` field (`attach_clients`), not herdr's title-clearing side effect.

For an agent on a peer, the board launches the terminal with `ssh -t <alias> tiller attach <session>`, where `<alias>` is the peer's `peers.<label>.ssh`. No new protocol is needed for that.

## 6. Restart, upgrade and reboot

### 6.1 The daemon restarts freely

The daemon owns no PTY. A daemon restart, crash or upgrade closes its socket connection and nothing else. On start it:
1. Connects with `hello`. When the holder does not answer, the daemon backs off (5 s, then 15 s, held at 15 s, as R18 does today) and treats every pane as unknown, never gone.
2. Compares `welcome.instance` with the instance it last saw (`meta.holder_instance` in `state.db`). The same instance means every pane it knew is still there or has a recorded exit. A new instance is R21.
3. Runs `list`, then `watch`, and reconciles the `panes` cache.

R01 stays as it is. Board views reconnect and get a fresh repaint.

### 6.2 The holder upgrades live, by re-exec

The owner chose live upgrades at any time. The holder replaces its own program image with `execve`, and keeps its PID:
- The children stay its children, so `waitpid` and exit codes keep working.
- The cgroup and the service unit do not change, so systemd and launchd see no restart.
- The listening socket and every PTY master are fds, and they survive `execve` when `FD_CLOEXEC` is off.

**Trigger.** The daemon checks at start, and after each `config.changed`, whether the binary the holder runs differs from the binary at the stable profile path. It compares the resolved store paths, read from `/proc/<pid>/exe` on Linux and `proc_pidpath` on macOS. When they differ, it sends `upgrade {binary: <stable path, resolved>}`. `tiller holder upgrade` does the same by hand.

**Steps:**
1. **Preflight.** The holder runs `<binary> holder --check-handover` as a child process, with a 5 s limit. The new binary prints the handover versions, the proto range and the holder version it accepts, as JSON. When it does not accept the current handover version or this proto, or the run fails, the holder answers `upgrade_refused` and keeps running.
2. **Dry run.** The holder quiesces (step 3) only to write a full handover record (step 4), then resumes. It pipes the record to `<binary> holder --dry-run-handover`, with a 10 s limit. The child reads it, rebuilds every model, writes the record again and prints a checksum of what it wrote. The checksum must equal the old holder's checksum of its own record. Any other result is `upgrade_refused`. Nothing has changed yet. A version that reads an older record version writes it again at the version it read, so the two checksums can match. An `upgrade` with `dry_run: true` stops here: the reply is `upgrading`, and the client reads it as "ready".
3. **Quiesce.** The holder takes the requester's write turn, so no frame is half written to that connection at exec, and takes the spawn turn, so no spawn is half done. It stops reading the PTYs and writing input to them, and locks every pane, its input queue and the pane table, in that order. Output that arrives meanwhile waits in the kernel's PTY buffer. A child that fills the buffer blocks on its write for the time of the handover, which is about 100 ms.
4. **Handover.** The holder writes one versioned record to a sealed `memfd` (Linux), or to an unlinked file in the run directory (macOS). The record is the holder's own binary format, not `serde`. A fixture of version 1 (`crates/holder/tests/fixtures/handover-v1.bin`) keeps its bytes fixed. The record holds:
   - The instance, the boot id, the start time and the next pane `num`.
   - The listening socket's fd number and the `holder.lock` fd number, and the requester's socket fd and request id.
   - Per pane: the pane record (id, `num`, key, pid, argv, cwd, size, meta, title, state and exit), the master fd number, whether output has ended, an exit that was reaped and not yet sent, and the time left of a `close` grace.
   - Per pane, the **input queue**: every frame not yet written to the PTY, with the part of a frame that a short write left, and whether the queue was closed.
   - Per pane, the **model state**, as structured data, not as ANSI: both grids (primary with its scrollback, and alternate) with every cell's character, colors, flags and wrap flag, the cursor and its pen (the current SGR), the saved cursor of each screen, the scroll region, the charsets and which one is active, the tab stops, every terminal mode flag, the kitty keyboard stack of each screen, and the colors the app set.
   - Per pane, the parser's place: the scanner's held tail (section 3.4.1) and its state, the bytes of an open sync frame, the character `CSI b` repeats, and the title, the title stack and the told events not yet taken.

   Leases and subscribers are not in the record, because they belong to connections and every connection ends at exec (section 2.6). A pane that does not fit a record refuses the upgrade with `upgrade_refused {reason: "record: ..."}`: an open sequence over the scanner's 64 KiB bound, a sync frame that started inside a cut sequence, or a model that does not read back the same. It then clears `FD_CLOEXEC` on the masters, the listener, the lock, the requester's socket and the record's fd.
5. **Exec.** `execve(<binary>, ["tiller", "holder", "--resume", "<fd>"])`. When `execve` fails, it returns. The old holder then sets `FD_CLOEXEC` again, resumes, and answers `upgrade_refused {reason: "exec: ..."}`.
6. **Resume.** The new image reads the record. It rebuilds each model from the model state, at the same size, and puts the scanner's held tail back in front of the next read. An open sync frame gets a new deadline, 150 ms from resume. It adopts the fds, sets `FD_CLOEXEC` on them again, reaps any child that exited during the handover (its exit code is not lost, because the zombie waits for `waitpid`), and sends an exit that the old image reaped and did not send. A `close` goes on with the grace it had left. It answers the requester `upgrading`, then `upgraded {holder}`, and closes that socket. Then it serves. Other client connections did not survive (they were close-on-exec), so clients reconnect. `welcome` now shows the new `holder` version with the same `instance`. The daemon records `holder.upgraded` with both versions.

Every refusal answers `upgrade_refused` with a reason that starts with its step: `busy` (another upgrade runs, or the requester's connection is busy), `preflight`, `version`, `record`, `dry_run`, `requester` or `exec`. After each one the old holder serves on as it was.

What the handover loses: nothing in the PTYs, the children or the metadata. The parser's place is in the record, so a read that ended inside a sequence goes on after the upgrade as if there had been none. The leases end and are taken again.

**The model state needs a patch.** `alacritty_terminal` 0.26 has no public read and write path for five parts: the inactive grid, both kitty keyboard stacks, the tab stops, the scroll region and the active charset. HA1 found the gaps, and the owner chose (D17) a small getter and setter patch on the pinned version. Cargo uses it through `[patch.crates-io]` from `vendor/alacritty_terminal`, the version stays `=0.26.0`, and the diff is kept in `nix/patches/alacritty_terminal-0.26.0-handover.patch`, as the repo does for `bun2nix`. The `alacrittyPatchFresh` flake check proves that the vendored copy is the released crate plus that diff. `vendor/README.md` says how to send it upstream and when to drop it.

**Rollback.** An upgrade to the old store path, by the same steps.

**Why not the other two ways:**
- **Drain** (stop new spawns, wait for every agent to end, then restart) never finishes, because agents run for hours.
- **Passing the fds to a new process** with `SCM_RIGHTS` loses the parent link. The new process cannot `waitpid` for children it did not start, so exit codes are lost. Linux `pidfd` cannot wait for a non-child either, and macOS has no `pidfd`.

### 6.3 A machine reboot

Every pane, every child and the holder are gone. The holder starts at boot (systemd user with lingering) or at login (launchd), with a new `instance` and no panes. R04 runs as it does today, with one change: step (1) "recreate the herdr layout" goes away, because there is no layout. The orchestrators come back first, then each worker in its own worktree, with `claude --resume`.

### 6.4 The holder is lost (new row R21)

The holder crashed, or someone killed it. Every child got `SIGHUP` when its master closed. The disk and `state.db` are intact.
- **Detection.** A `welcome` with a new `instance` while the boot id is the same.
- **Invariants.** As in R04: worktrees, runs, reservations and the outbox are untouched.
- **Automatic.** Each session whose pane is gone goes through the ladder's agent probe (`kill(pid, 0)` on the recorded pid). Dead sessions are resumed as in R04, steps (2) to (4).
- **To the developer.** One notification (section 7): "the terminal holder on <host> restarted, N of M agents resumed". The board shows the restart on the Machines row for 24 h.

## 7. Notifications through a command

The owner chose their desktop notification app. Tiller stays generic: it runs a configured command, and the owner's config names the app's hook.

```toml
[notify]
command = ["notify-remote-hook", "claude"]   # on host B, which has no screen: the hook queues the card for host A
# command = ["notify-app", "hook", "claude"] # on a Mac with the app installed
```

The command names above are placeholders for the owner's own hook commands.

- **Payload.** The notifier writes one JSON object to the command's stdin, in the shape of a Claude Code `Notification` hook event, because the owner's app already reads that shape:
  `{"hook_event_name":"Notification","notification_type":"tiller","title":"alpha-42","message":"...","session_id":"<orchestrator session>","cwd":"/home/user/src/alpha"}`.
- **Environment.** The command gets `TILLER_SESSION_ID`, `TILLER_PANE` and `TILLER_HOST` (the host label), so a click handler can attach to the right pane.
- **Outcome.** Exit 0 within 10 s is `notification.sent`. Anything else is `notification.fail`, with stderr in the message. The record-then-send rule of `protocol.md` 3.4 does not change, so a notification is never sent twice.
- **No command set.** Tiller sends nothing, and `tiller doctor` warns "owner notifications are off".
- **The holder's own `notify` events** (OSC 9 and OSC 777 from an app) are recorded as observations. They are not sent to the owner. The owner's own Claude Code hooks already raise those cards.

**Outside this repo (owner-approved step at cutover):** the dotfiles must change in two places.
- The app's click handler today focuses a herdr pane from the `HERDR_*` ids. It must run `tiller attach "$TILLER_SESSION_ID"` in the terminal app instead: locally, or as `ssh -t <host> tiller attach ...` for a card from host B.
- The app's list of forwarded env names must name `TILLER_SESSION_ID TILLER_PANE TILLER_HOST` in place of the herdr ids.

This affects the agents' own cards too, because their panes now carry `TILLER_PANE` and `TILLER_SESSION_ID`, not `HERDR_*`.

## 8. Peers and remote hosts

**Now:**
- The peer pane link (`protocol.md` 10.7, `daemon/peer/pane.rs`) moves to the peer's own holder. The board host still opens `tiller peer pane` over SSH, and the `pane_open` frame keeps its fields, with an optional `fit`.
- Remote input from the board (HR1): signed `pane_input` and `pane_fit` frames on the same pane link, allowed only with the `act` scope (`authority.md` G46, `protocol.md` 10.7). They reach the pane through the peer's own input path, holder or herdr, and a peer pane fits the stage and gives its size back as a local one does.
- Every other agent verb for a peer (HR2, the owner's decision D20 on Friday 2026-11-13): the tail and the prompt read as a `query` on the peer link, and the prompt answer, interrupt, steer, stop, pause, unpause, resume, relaunch and an attention item's withdraw as a forwarded `mutate`. Each needs the `act` scope (the tail excepted), and the peer runs it through the same core function as its own board, on its own holder, with its own checks (`authority.md` section 3 and G46, `protocol.md` 7.4, Agent verbs on a peer). A `read` pairing and every fleet role stay refused, and no host forwards to a third (G42).
- Saved herdr machines go away: `hosts.herdr_machine`, `tiller peer add --herdr-machine`, the remote herdr poller, the saved-machine tail and `herdr --machine` focus. A remote host must be a paired Tiller peer (ADR 0003). The cutover pairs or removes each saved-machine-only host first (section 11).
- Open in terminal for a peer's agent uses `ssh -t <alias> tiller attach <session>` (section 5.3).

**Later (not in this build):**
- One Fit turn across the peer's own board and its `peer pane` processes: the holder's lease (H11) gives it.

## 9. The test gate

### 9.1 Parser results must not change

- **H00 freezes today's results** before any code moves. For each fixture, then in `crates/herdr/tests/fixtures/claude/`, it stores `composer::classify` and the `prompt.rs` parse result as `insta` snapshots, made with today's herdr-era code.
- **Model gate.** Each fixture is fed into the holder's model at 119 by its row count. The model's `screen(visible, ansi)` then goes to `classify` and to the prompt parser. Both results must equal the frozen snapshots. The fixtures move to `crates/panes/tests/fixtures/claude/`, unchanged.
- **Raw stream gate (H08).** Raw PTY recordings of real Claude Code sessions: start, a draft, a turn, a permission dialog, a trust dialog, `/exit`, and a 40 to 120 column resize in the middle of a turn. Each recording marks moments with a known verdict. Replayed through the model, each marked moment must give that verdict. The recordings are scrubbed (section 12) before they are committed.
- **Repaint round trip.** proptest: random byte streams and every fixture, fed in, repainted, and fed into a fresh model, give the same grid.

### 9.2 Restart tests

A test agent stands in for Claude Code. `tiller-panes` ships `fake-agent`, a small binary built only for tests. It draws a composer, echoes typed keys, submits on Enter, and copies Claude Code's habit of reading a fast Enter as a newline. Every test below uses it, and no test starts a model-backed agent.
- Kill the daemon with `SIGKILL` during a stream, during a Fit, and between a typed line and its Enter. The agent keeps running. The restarted daemon reconnects, the lease is gone, the message is delivered once, and nothing is typed twice.
- Restart the daemon 20 times in a row while the agent writes a counter. The counter has no gap.

### 9.3 Upgrade tests

With 8 live `fake-agent` panes, two of them in the alternate screen, one with kitty flags pushed and one in the middle of a sync frame:
- After an upgrade: the same PIDs, the same `instance`, and the model state equal field by field: both grids, scrollback, pen, saved cursors, scroll region, charsets, tab stops and modes. Meta is kept. Leases are gone, and each client's lease comes back when it reconnects.
- An app in the alternate screen that leaves it after the upgrade shows its old primary screen.
- An upgrade while a pane's last read ended inside a CSI sequence and inside a UTF-8 character: the grid after the next read equals the grid with no upgrade.
- An agent that writes a counter with no pause through the upgrade: no gap and no repeated number.
- A child that exits during the handover: its exit code is recorded by the new image.
- A preflight that fails, a dry run that fails and an `execve` that fails: the old holder keeps running and answers every request after it.
- An upgrade from version N-1 to N, and a rollback from N to N-1.

### 9.4 Fault tests

- `SIGKILL` the holder: the daemon sees a new instance, the ladder runs, sessions are resumed (R21), and one notification is sent.
- A forced model panic (a test-only feature flag): only that pane's model resets, and the child lives.
- A slow subscriber: its stream gets `lagged` at 4 MiB, its connection stays, and the agent's writes never wait.
- A flood: 200 MiB of output in 10 s. The holder's memory stays bound, and every reply to `ping` takes under 50 ms.
- A wrong uid on the socket: tested by calling the peer check with injected credentials, because a test cannot change its uid.
- A stale socket file, a lock held by a live holder, and two holders started at once: exactly one serves.
- Every query sequence is answered once on the PTY and never reaches a subscriber.
- A frame over 16 MiB and a malformed JSON message end that connection only.

### 9.5 Attach tests

These drive `tiller attach` through a PTY harness in the test:
- Raw mode is on during attach and off after detach.
- Detach puts back every mode the agent had turned on.
- The prefix works as byte `0x1c` and as `CSI 92;5u`. `Ctrl-\ Ctrl-\` sends one `0x1c`.
- `Ctrl-\ ?` shows the help screen with every prefix key and sends nothing to the pane. The next key closes it, and the pane is repainted. That key is swallowed, and nothing reaches the pane.
- Two clients get the same output, and the size follows the one that typed last.
- `--read-only` sends nothing.
- The switcher works with the daemon down.

### 9.6 Board

- `terminal-stream.test.ts` covers repaint then passthrough, `closed` with an exit code, and `unfit` still parsed.
- Screenshots of the Agents view, the card page and Settings host checks, in both themes, at 1440 px and 390 px. They show the new copy and the live view of a `fake-agent` pane.

## 10. Every spec and doc change

The doc rows land in H01, before the code, so that each builder builds against a written contract. The specs tag each changed rule [H]. The code rows land with the code that needs them:
- `contract/openapi.json` is generated, and its staleness test fails on a hand edit. H11 and H13 regenerate it.
- `nix/module.nix` goes with H06 (the holder service) and H13 (herdr removed).
- `flake.nix`, `roles/*.settings.json`, `scripts/two-host-demo.sh` and `demo/` go with H13. The daemon reads the role files, so they change with the code.
- The board files of 10.1 go with H12.
- `docs/spec/schema.sql` keeps today's DDL with [H] comments until H10's migration.

| File | Change |
|---|---|
| `docs/spec/protocol.md` | 6.1: an agent's name, called the herdr name before the cutover. 6.2: `unavailable` for an unreachable holder. 10: a harness with no adapter is observed through the holder. 3.1 and 3.3: "typed through the holder", target check by pane key, composer read by `screen`. 3.4: notifications through `[notify] command`, opened with `tiller attach`. 4.5: the `herdr` poller rows become `holder` (exit and screen). 6.4: `tiller attach`, `tiller holder` and `tiller holder upgrade`, `agent focus` as Open in terminal, `peer add` without `--herdr-machine`, and doctor's `holder` check. 7.3: `tools.holder`. 7.4: rewrite Open in herdr and the Terminal mirror for the holder, and drop Tail for saved machines. 9: drop herdr-only machines and `herdr_link_down`. 10.1 and 10.7: peers only, link on the holder. 12: coverage rows |
| `docs/spec/recovery.md` | Sources: `holder` in place of herdr. The ladder: step 2 is the holder's `list` and the pane key, step 3 is `state` and the exit code. R04: drop the layout step. R05: peers only. R18: rewrite as "Holder unreachable" (no tokens to re-report). New R21: holder lost. New R22: holder upgrade failed (the old holder runs on, `upgrade_refused` is a Health warning) |
| `docs/spec/state.md`, `docs/spec/schema.sql` | `panes` table owned by the holder (section 11.1). `hosts.herdr_machine` dropped. Observation source `holder`. `meta.holder_instance`. Poller name `holder`. Events `holder.upgraded` and `holder.lost` |
| `docs/spec/authority.md` | Agents are denied `tiller holder`, `tiller attach` and `$TILLER_HOME/run`, in place of the `herdr` CLI. The owner's input through `tiller attach` is the owner's, as a board tap is (G29) |
| `docs/design/host-capabilities.md` | The "herdr status works" minimum becomes "the holder starts and its socket answers". The no-user-service row: the daemon and attach start the holder detached. Rows that name herdr panes now name holder panes |
| `docs/design/herdr-layout.md` | A header: superseded by `terminal-holder.md` at cutover, kept as history |
| `docs/design/README.md` | Index row for this doc. Host B "reached through herdr's saved machine" becomes "a paired peer" |
| `docs/design/cli-board-parity.md` | Open in terminal and `tiller attach` |
| `docs/plan/build-plan.md`, `docs/plan/checklist.md` | The H tasks of section 13 |
| `README.md` | The crate table: `panes` and `holder` in place of `herdr` |
| `AGENTS.md` | A line under Notes: `alacritty_terminal` is pinned, and a bump must pass the fixture gate |
| `contract/openapi.json` | The DTO renames (section 11.1) and the focus copy |
| `nix/module.nix` | The holder service (section 11.2). herdr removed from the daemon's PATH comment and the module options |
| `flake.nix` | The fixture path moves to `crates/panes/tests/fixtures`. The Homebrew herdr check message goes |
| `roles/*.settings.json` | `Bash(herdr *)` becomes `Bash(tiller holder*)` and `Bash(tiller attach*)`, each with its `Bash(<tiller> ...)` twin, with no space before the `*` so the bare commands match too |
| `probe/` | History, left as is (`probe/reports` record what was measured then) |
| `scripts/firstmate-export` and its fixtures | Left as is. It reads the predecessor's herdr ids, which is part of the import contract |
| `scripts/two-host-demo.sh` | `TILLER_HERDR=/nonexistent/herdr` becomes a test holder socket in the demo's home |
| `demo/` | Its herdr copy becomes holder copy |

### 10.1 Board files

`components/board/herdr.ts` (and its test) becomes `terminal-open.ts`. The other files that need copy or logic changes: `live-terminal.tsx`, `terminal-stream.ts`, `terminal-keys.ts`, `card-actions.tsx`, `inbox.tsx`, `agents-view.tsx`, `run-menu.tsx`, `lane.tsx`, `card-page.tsx`, `orchestrator-start.tsx`, `attention/terminal.tsx`, `attention/view-live.tsx`, `attention/answer-strip.tsx`, `board/agents-model.ts`, `settings/settings-page.tsx`, `states/reconnecting-banner.tsx`, `states/lane-note.tsx`, `states/degraded.ts`, `settings/host-checks.tsx`, `settings/check-lines.ts`, `settings/host-config-sections.tsx`, `shell/machines-section.tsx`, `lib/machines.ts`, `lib/peers.ts`, `lib/api-types.ts`, the `lib/fixture*.ts` files and `vite-fixtures.ts`, with their tests.

## 11. Cutover

### 11.1 State changes (one migration)

| Today | After |
|---|---|
| `panes` table, "owner: herdr", key `(host_id, pane_id)`, `agent_name`, `agent_status` (herdr's word) | `panes` rebuilt: `pane_id` (holder id), `host_id`, `holder_instance`, `session_id`, `pid`, `label`, `state` (`running`/`exited`), `exit_code`, `exit_signal`, `exited_at`, `metadata`, `fetched_at`, `gone_at`. Old rows are dropped: no herdr pane outlives the cutover |
| `hosts.herdr_machine` | Dropped (SQLite `DROP COLUMN`). The migration refuses to run while a remote host has a `herdr_machine` and no peer pairing. Its message names the host and the way out: `tiller peer add` (D11) |
| Observation source CHECK `'herdr'` | `'holder'` added. Old rows keep `'herdr'` as history. The CHECK needs a table rebuild in SQLite |
| `sessions.pane_id`, `outbox.target_pane_id` | Names kept. The values are now holder pane ids |
| Config `[spawn] herdr`, `herdr_socket`, `[herdr] terminal`, `terminal_exec`, host-config `herdr.terminal`, env `TILLER_HERDR` | `[holder] socket` and `default_size`, `[attach] terminal`, `terminal_exec` and `prefix`, `[notify] command`, and host-config `attach.terminal`. A config that still names a herdr key starts fine, logs a warning, and `tiller doctor` says which key to remove |
| DTO and CLI JSON `herdr_name`, `pane_tail` from saved machines, `herdr_terminal`, `tools.herdr`, `stale_reason` values `herdr_link_down`, `host_asleep` and `poll_failed` | `name`, `pane_tail` from local panes only, `attach_terminal` and `tools.holder`. The three herdr `stale_reason` values go, and the local host gets `holder_down` (`protocol.md` 9, R18). The `cli/tests/snapshots` files are updated |

The migration follows R11: a pre-migration copy, one transaction, and no hook ever migrates.

### 11.2 Nix module

- A new `tiller-holder` user service on Linux and launchd agent on macOS, as in section 2.2.
- `services.tiller.holder.enable` defaults to `false` when H06 lands, so a dotfiles bump before cutover starts no holder on the live host. H13 changes the default to the daemon's `enable`.
- `herdr` leaves the daemon's PATH comment and every option that named it. The module no longer needs herdr at all.

### 11.3 Steps on the live tiller

Each step is done by the owner, or by a builder with the owner present, at a checkpoint. They run only while no agent runs.

| # | Step | Undo |
|---|---|---|
| 0 | Check: no running agent (`tiller agents` shows none running), every active run paused or ended, quota checked | - |
| 1 | `tiller backup now`, and note the version and the backup path | - |
| 2 | Pair every remote host that is only a saved herdr machine (`tiller peer add`) | Two-way: unpair |
| 3 | Bump the Tiller flake input in the dotfiles and switch. The holder service starts. The daemon restarts and runs the migration | **One-way for state.** Back only by restoring the step 1 backup together with the old binary. That loses what was written since |
| 4 | `tiller doctor`: the `holder` check passes, and no herdr key is left | - |
| 5 | Start one orchestrator. Check the board's live view, a Fit, `tiller attach`, detach and reattach, a notification card and its click, and `tiller holder upgrade --dry-run` | Stop it |
| 6 | Dotfiles (owner-approved, outside this repo): the notification app's click handler and its forwarded env list (section 7). herdr itself may stay installed for other use. Tiller no longer needs it | Two-way: revert the commit |
| 7 | Resume the paused runs with `tiller agent resume` | - |

**One-way doors, in one place:**
- The migration drops `hosts.herdr_machine` and the old `panes` rows. Only a restore of the backup brings them back.
- The deletion of `crates/herdr` is a git change, so `git revert` can undo it. But after the migration, the old code cannot read the new schema.

**Two-way doors:** the binary, the service units, the dotfiles changes, and the pairing.

## 12. Fixture recording (H08)

- **Recorder.** `tiller holder record --out <dir>` (a test build only: the `record` feature) writes two files per pane: `<pane>.raw` with the raw PTY output, and `<pane>.jsonl` with the timestamps, byte offsets and size changes (`crates/holder/src/record.rs`).
- **Who runs it.** A builder, once the owner gives the go. The run starts real Claude Code sessions, so it spends model quota. It needs about 20 minutes of sessions in a scratch project.
- **Scrub.** Before anything is committed: `scripts/scrub-check.sh` passes, and the builder reads every recording for paths, hostnames, user names and secrets. Paths become `/home/user`, and hosts become `example.com`. A recording that cannot be cleaned is dropped.
- **Marks.** A sidecar JSON per recording lists byte offsets with the expected composer verdict and prompt parse. The recordings, their sidecar format and their known limits are in `crates/panes/tests/fixtures/claude-raw/README.md`. `crates/cli/tests/raw_gate.rs` is the gate.

## 13. Build plan

Each task but S1P, HA1 and H15 is one builder, one branch, one worktree and one PR, with a `/code-review` before the PR and `no-mistakes` as the gate. Those three change no file in the repo: S1P (the preview and its steps for the other hosts) and HA1 (the field list) report to the owner, and H15 is steps on the live tiller with the owner. Times are agent wall-clock time, with review and CI included.

**Where the port lands.** H00 to H08, S1a and S1b add code or docs that nothing uses yet, so they merge to `main`. The port (S1c, then H09 to H14) cannot land on `main` piece by piece without a fallback, and the owner ruled out a fallback. So those tasks merge into an integration branch, `holder-port`, and the owner merges `holder-port` into `main` once, at cutover. S1c creates `holder-port` from `main`. The builder of each later port task merges `main` into `holder-port` first. The live tiller is never deployed from `holder-port`. The S1P preview runs on a test home, not on the live tiller.

**The first slice (S1).** Before H02, a thin slice lets the owner type in a terminal that the holder owns, reload the board and restart the daemon, and find the same terminal still running. S1a and S1b are the minimal parts of H02, H03 and H04. S1c is the minimal daemon wiring, the first part of H09 to H11. H02 to H11 then extend the slice code: each builder reads it first and keeps its interfaces unless this doc says otherwise. Their times below are what is left after the slice.
- **The slice is done** when the owner, on the S1P preview, types in a shell pane and sees the output, reloads the page and sees the same screen, and restarts the daemon with the holder still up and gets the same pane back, with its history, still taking input.
- **"Resumable" in the slice** means a board reload and a daemon restart only. A holder restart, a live upgrade and a reboot come with H05 and H10.

| # | Task | Depends on | Time |
|---|---|---|---|
| H00 | Freeze today's composer and prompt results on the 9 fixtures as `insta` snapshots | - | 30 min |
| H01 | Spec amendments: every row of section 10 except the code ones, plus ADR 0004 marked accepted and the S1 rows of this section | this doc merged | 2 h |
| S1a | The minimal part of H02: the `tiller-panes` crate with protocol types, framing (kinds 0, 1 and 2), the version handshake, `HolderClient`, the `Panes` trait with the methods the slice uses, and `FakePanes`. No module moves. Framing round-trip tests and a handshake version mismatch test | H01, H00 | 2 h |
| S1b | The minimal part of H03 and H04: `tiller holder` with the socket (mode 0600, peer check), the lock, PTY spawn and reaping with exit codes, the `alacritty_terminal` model with `catch_unwind`, `subscribe` with a full repaint and then passthrough, raw input, and resize. One fixed cap on each client's output queue: a full queue drops the client. No leases, sync buffering or fault tests. An end-to-end test that spawns a shell, types and reads the repaint, a peer check test and an exit code test | S1a | 2.5 h |
| S1c (`holder-port`) | The minimal daemon wiring: new agents launch in the holder, the agent record keeps the pane id, the terminal and input endpoints use the holder, and the daemon finds its panes after it restarts. Only the board copy it needs. It tests against `FakePanes` first. Its end-to-end test, a first restart test ahead of 9.2, restarts the daemon with the holder up and checks that the endpoint serves the same pane and takes input | S1a, and S1b for the end-to-end test | 1.5 h |
| S1P | A preview from `holder-port` on one host: a holder and a daemon on a test `TILLER_HOME`, the board on a spare port, and one plain shell pane, never a model agent. The owner checks the slice. Steps to run the same preview on the other hosts | S1b, S1c | 30 min |
| H02 | Finish `tiller-panes` and add `tiller-poll`: the rest of the protocol types, the `Live` trait, the rest of the `Panes` methods, and key names. Move `composer.rs`, `submit.rs`, `process.rs`, `front.rs`, `testing.rs` and the generic poller there, with `tiller-herdr` re-exporting them so `main` still builds | S1P (the slice done) | 3 h |
| H03 | Finish the `tiller-holder` core: env, the requests other than `subscribe` and `upgrade` that the slice left out, and the model gate of 9.1 | H02 | 4 h |
| H04 | Repaint renderer, query answers and filter, sync buffering, leases, back-pressure in place of the slice's fixed cap, limits. Round-trip proptest and fault tests of 9.4 | H03 | 4.5 h |
| HA1 | A read-only audit: list the `alacritty_terminal` state that the handover of 6.2 must carry, with its public read and write path. A field with no such path stops H05 until the owner picks an upstream change or a patch in `nix/patches/` | H03 | 30 min |
| H05 | Live upgrade, from HA1's field list: the handover record, preflight, dry run, re-exec and resume. The tests of 9.3 | H04, HA1 | 6 h |
| H06 | `tiller holder` service: `--detach`, `upgrade`, the Nix module service and launchd agent, the doctor `holder` check | H03 | 3 h |
| H07 | `tiller attach`: raw mode, prefix (both encodings), detach restore, many clients, read-only, switcher. The tests of 9.5 | H04 | 5 h |
| H08 | Raw fixture recording (owner's go): recorder, about 20 min of real sessions, scrub, marks, raw stream gate | H04 | 3 h |
| H09 (`holder-port`) | Agents and delivery on `Panes`, from S1c's wiring: spawn, resume, control, stop, submit, notifier on `[notify] command`, labels. `fake-agent` and the restart tests of 9.2 | S1c, H05, H06 | 6 h |
| H10 (`holder-port`) | Reconcile, observe and state: `HolderSource`, ladder, R18, R21 and R22, `holder` observations, the migration of 11.1, config keys | H09 | 6 h |
| H11 (`holder-port`) | Daemon API and peers: live view on `subscribe`, Fit as a lease, history, prompt, input, Open in terminal, peer pane link, the saved-machine paths removed | H10, H07 | 5 h |
| HR2 | Peer agent parity (D20): the spec rows first (HR2a: `authority.md` section 3, the matrix and G46, `protocol.md` 7.4 and 10.7, ADR 0003's note), then the code: the agent verbs and the withdraw forwarded as `mutate`, the tail and the prompt read as `query`, the 60 s wait for a proof, and the tests the spec names | H11 | 4 h |
| H12 (`holder-port`) | Board: copy, `terminal-open.ts`, stream semantics, the tail UI for saved machines removed, every agent control live on a peer's agent (HR2), screenshots in both themes at 1440 and 390 px | H11, HR2 | 4 h |
| H13 (`holder-port`) | Delete `crates/herdr` and every herdr name left (section 10 code rows, roles, flake, demo, scripts). Set `services.tiller.holder.enable` to default to the daemon's `enable`. `git grep -i herdr` finds only history: `probe/`, `scripts/firstmate-export` and its fixtures, ADRs, superseded docs and the specs' [H] notes on what came before the cutover. Tests with herdr in their name are renamed | H12, H08 | 3 h |
| H14 (`holder-port`) | End-to-end soak on a test home with `fake-agent` panes: 4 h of daemon restarts, 3 live upgrades and a holder kill, with no gap in any counter. Then the full CI gate and `nix flake check` | H13 | 6 h (4 h is the soak itself) |
| H15 | Cutover on the live tiller, with the owner (section 11.3), after the owner merges `holder-port` | H14 | 1 h |

**Total:** about 73 hours of agent time: about 9 hours for H00, H01 and the slice, and about 64 hours for H02 to H15 with HA1 and HR2. H00 runs beside H01, and S1c beside S1b. H06, H07, H08 and HA1 run beside H04 and H05. The critical path is H01, S1a, S1b, the end of S1c (its end-to-end test and review, about 1 h) and S1P, then H02 to H05, then H09 to H11, HR2 and H12 to H15: about 61 hours. HR2 sits between H11 and H12, so the H12 board covers peer agents too.

## 14. What stays open

Decided:
- **D11, no `tiller host remove`.** The 11.1 migration refusal named it as a second way out, but the CLI has no such command and no spec defines it. The owner chose to make `tiller peer add` the only way out: the refusal names it alone, and step 2 of 11.3 pairs each saved herdr machine.
- **D12, a binary check on `tiller attach`.** `tiller` runs outside the role sandbox, so on a host where hooks do not fire the deny rule is the only verb check against an agent that runs `tiller attach` (`authority.md` section 8). The owner chose a refusal in the binary, as `tiller peer serve` has, that still lets a notification click through. Section 5.2 has the rule.

Two points are for a later task or the owner, recorded here so they are not lost:
- Shorter Enter pacing with bracketed paste (section 4.5), once raw stream fixtures exist.
- The 60 s wait for a peer agent verb that waits for proof (HR2, `protocol.md` 7.4) is a proposed value. The owner has not confirmed it.

Remote input over the peer link (section 8) has its row: `authority.md` G46 and `protocol.md` 10.7. The other agent verbs for a peer have theirs since D20 (HR2): `authority.md` section 3 and G46, and `protocol.md` 7.4, Agent verbs on a peer.
