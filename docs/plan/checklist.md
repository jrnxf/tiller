# Tiller checklist

This is where build progress is tracked. The briefs (goal, files owned, full acceptance text) live in `build-plan.md`; each task heading here names its brief.

- **Who ticks:** each task's PR ticks that task's boxes in the same commit that does the work, so this file on the default branch always matches what has merged. The developer ticks checkpoint boxes, or tells the executor to.
- **Where:** `docs/plan/checklist.md`, since task T0.2 moved it into the repo with every box ticked until then.
- **Every task ends with the same delivery box:** independent `/code-review`, plus `mattpocock-skills:code-review` against the spec when the task has one, then `no-mistakes`, then a merge on the developer's go.
- **Estimates** are agent wall-clock time for one lane, including review and CI.

Started Sunday 2026-11-01, Lisbon time.

---

## Already decided

- [x] Replace Firstmate with an owned system, with one cutover per host through the export prompt
- [x] Stack: Rust core, TanStack Router board embedded in the binary, no SSR
- [x] herdr layout: nested workspaces, one per worker, under the project's orchestrator
- [x] CP-UI: mocks approved, third pass (Sunday 2026-11-01)
- [x] Name: Tiller, CLI `tiller` (Sunday 2026-11-01)

## CP0: Before anything (developer, 5-10 min)

- [x] Defaults confirmed: repo `tiller`, private, MIT; remote lanes read-only; work data stays on the work Mac; one owner host per project; auto memory off for fleet roles (Sunday 2026-11-01)
- [x] Remote lanes default reopened (C12, Tuesday 2026-11-03): owner approved ADR 0003, the federated board, with its three recommended choices (the personal Mac as the one board host, the work Mac out with `federation.accept = false`, an unreachable peer's mutation refused at once and nothing queued). Blocks only the Federation section and T6.3
- [x] Firstmate fleet ledger: on. Remote host since 2026-10-23, personal Mac since Sunday 2026-11-01 19:57
- [x] alpha secondmate trial for 2-3 days during the build: yes
- [x] GitHub repo created: `tiller` under the developer's account, private, empty (Sunday 2026-11-01)
- [ ] `tiller` added as a Firstmate project with `no-mistakes` delivery (first mate)
- [ ] alpha secondmate launched for the trial (first mate)

---

## Wave A: Foundations (day 1)

### T0.1 Repo bootstrap
`sonnet` high - 2-3 h - after CP0

- [x] Cargo workspace with the `cli` crate; clippy with `-D warnings`, rustfmt, `cargo nextest`
- [x] `board/`: Vite and TanStack Router app with a placeholder route; bun, oxlint, oxfmt, knip, vitest
- [x] Nix flake: crane builds `tiller` with the board embedded; board derivation from `bun.lock`; dev shell with Rust, bun, sccache, sqlite3; home-manager module stub
- [x] GitHub Actions CI: clippy, format checks, nextest, board lint, typecheck and tests
- [x] `no-mistakes` config, MIT `LICENSE`, `README.md`, `AGENTS.md` with the hard rules and the commands for both halves
- [ ] herdr 0.9.3 to 0.9.5 and the herdr config rows, through a dotfiles PR
- [x] Check: CI green
- [x] Check: `tiller --version` works, and `tiller daemon` serves the placeholder board from the embedded assets
- [ ] Check: `nix build` gives one binary on macOS and on the Linux remote host
- [x] Check: warm sccache rebuild of one crate in a worktree under 10 s
- [x] Check: board build path reported (bun2nix, or the fixed-output fallback after the 1 h tripwire)
- [ ] Delivered

### T0.2 Import and scrub the design docs
`opus` medium - 1-1.5 h - after T0.1

- [x] Every input from the plan's inputs table copied into `docs/` (design docs, export prompt, build plan, this checklist); the two Lavish design files are pending, see `docs/design/README.md`
- [x] Machine names replaced with roles (host A personal Mac, host B remote Linux host, host C work Mac); local paths and private project names removed
- [x] `docs/adr/0001-replace-firstmate.md` and `docs/adr/0002-sqlite-authority.md` written
- [x] `scripts/scrub-check.sh` written and running in CI
- [x] Check: the scrub check finds nothing
- [ ] Delivered

### T0.3 Capability probe
`opus` high - 1-1.5 h, plus about 10 min per host - no dependency

- [x] `probe/tiller-probe.sh` checks: hooks under managed settings, launchd or systemd user service, localhost port, herdr CLI and `api snapshot`, auto memory and `CLAUDE_CODE_DISABLE_AUTO_MEMORY`, `--append-system-prompt-file`, `claude plugin eval --no-publish`, sqlite3 and the Nix-store binary, transcript paths
- [x] Run on the personal Mac
- [x] Run on the remote host
- [ ] One-line work Mac command handed to the developer
- [x] `docs/design/host-capabilities.md` drafts the minimum common behavior
- [x] Check: a before and after diff proves the probe changed nothing outside a temp directory
- [ ] Delivered

### T0.4 Firstmate dry-run export (scout)
first mate, Part C scout `opus` medium - 1-1.5 h per host - after CP0

- [ ] Fleet ledger switched on first, if approved at CP0
- [ ] Export run with `MODE=dry` on the personal Mac, Part C included (baseline and replay set)
- [ ] Export run with `MODE=dry` on the remote host
- [ ] Bundle kept local under `~/.tiller/import/`, never committed
- [ ] Check: every Part G check passes, shown in the report
- [ ] Open questions listed for CP1

### T0.5 Throwaway board demo
`sonnet` medium - 1-1.25 h - after CP0

- [x] One-command local server in `demo/` polling Firstmate's fleet snapshot and herdr (local and the saved machine)
- [x] SSE into one Linear-styled page with all projects visible, read-only, jump-to-pane for local panes
- [x] Check: real backlog data shows
- [ ] Check: screenshots in both themes attached
- [x] Left running as the interim board until cutover
- [ ] Delivered

### T0.6 Firstmate parity matrix (scout)
`opus` high - 1-1.5 h - after T0.4

- [x] `docs/design/parity.md` built from `capabilities.md`, `memory/requirements.md`, `commitments.jsonl`, `regressions.md`, the contract docs and `VISION.md`
- [x] One row per capability actually used, with evidence (how often, last used)
- [x] Each row has an owner task and a test name, or is a proposed drop with its reason
- [x] Check: every capability in the bundle maps to a row
- [ ] Delivered

### T0.x herdr layout demo (optional)
about 3 min of the developer's time

- [ ] `--session tiller-demo` nested-workspace demo run and looked at

## CP1: Foundations review (developer, 40-50 min)

- [ ] Probe results from all three hosts reviewed (developer runs the work Mac one)
- [ ] Dry-run bundle's open questions answered
- [ ] Board demo used
- [x] Minimum common behavior across hosts agreed (approved as the baseline at CP2, Monday 2026-11-02, H01; host C still has to run the probe, and any fallback it needs is a separate decision)
- [ ] Parity matrix: every proposed drop approved or sent back

---

## Wave B: Specs (day 1-2)

### T1.1 State model
`fable` high - 3-4 h - after CP1 - runs alone

- [x] `docs/spec/state.md` and draft `docs/spec/schema.sql`
- [x] Entities, ULIDs and display ids (`alpha-42`); relationships (asks to cards, card to runs, run to session, run to PRs, owner host)
- [x] Lifecycle facts, column precedence, each card type's definition of done
- [x] `rev` columns and events in the same transaction; outbox states; approval states and binding; capacity reservations; fact-ownership table
- [x] Check: every fact has one owner, every column is derived, every mutation names its transaction and event
- [x] Delivered (Monday 2026-11-02, landed on main in the commit that adds the spec; CP2 reviews the draft)

### T1.2 Recovery table
`fable` high - 2-3 h - after a T1.1 draft

- [x] `docs/spec/recovery.md`, one row per scenario: daemon restart, orchestrator crash, worker crash, host reboot, remote disconnect, duplicate events, PR merged outside Tiller, stale approval, failed delivery, truncated backup, schema migration
- [x] Each row: detection, invariants, auto repair vs developer, end-to-end test name
- [x] Check: no row can destroy unlanded work automatically
- [x] Check: every regression in the bundle maps to a row
- [x] Delivered (Monday 2026-11-02, landed on main in the commit that adds the spec; CP2 reviews the draft)

### T1.3 Protocol and CLI
`opus` high - 2-3 h - after T1.1

- [x] `docs/spec/protocol.md`: ask intake and `tiller ask resolve`; wake-up line format
- [x] Every hook event mapped to an observation (all 13 `notification_type` values, `StopFailure`, transcript-detected interrupts); Stop hook observe mode; transcript tailing
- [x] Full `tiller` command surface in AXI style (structured output, stable ids, exit codes, idempotency keys)
- [x] Board API: HTTP reads and mutations with `rev`; one SSE stream with `Last-Event-ID` resume; boot snapshot
- [x] Quota feed from `quota-axi --full` every 5 min, `accounts` block dropped; machine summary per host
- [x] Check: every command is idempotent or says why not; every hook event maps to exactly one handler
- [x] Delivered (Monday 2026-11-02, landed on main in the commit that adds the spec; CP2 reviews the draft)

### T1.4 Authority and guards
`opus` high - 1.5-2 h - after T1.1

- [x] `docs/spec/authority.md`: role matrix; each guard labeled enforced or convention; deny rules per role; PreToolUse backstop
- [x] Merges only by the daemon after a bound approval; review, execute and merge as three separate decisions
- [x] Alternate paths listed for testing (`gh api`, `curl`, raw `git push`, writes outside the worktree, deleting worktrees)
- [x] Check: every enforced guarantee has a test name
- [x] Delivered (Monday 2026-11-02, landed on main in the commit that adds the spec; CP2 reviews the draft)

### T1.5 Memory
`opus` medium - 1.5-2 h - after T1.1

- [x] `docs/spec/memory.md`: source map (canonical file per memory, dotfiles-managed files edited at source)
- [x] Scope, evidence, validity, review status, provenance including `legacy-import`
- [x] `observations` and `proposals` tables never loaded into context; approval bound to diff plus base revision
- [x] Retiring, which sessions loaded a revision, scrub hook on memory commits, no promotion across the personal and work boundary
- [x] Check: nothing unapproved can reach a fleet role's loaded context
- [x] Delivered

### T1.6 Spec review
`fable` high, fresh session - 1.5-2 h - after T1.2 to T1.5

- [x] Coverage of the second opinion's 43 points
- [x] Coverage of the parity matrix: every kept capability specified
- [x] Conflicts between specs, unexplained departures from Firstmate's contracts, anything untestable
- [x] Clear fixes applied; the rest listed for CP2
- [x] Delivered (Monday 2026-11-02; fixes landed in `docs/spec/`, the open list and coverage tables in `docs/spec/cp2-open.md`)

## CP2: Specs approved (developer, 30-45 min)

- [x] State, recovery, protocol, authority and memory specs approved. After this, contract changes need a stop-and-ask. (Monday 2026-11-02, Lisbon time; every decision is closed in `docs/spec/cp2-open.md`, and the open owner values are listed at its top)

---

## Wave C: Core starts (day 2-3)

### T2.1 State core
`opus` high - 6-8 h - after CP2 - critical path

- [x] Interface published at about 1.5 h: every crate and module stubbed, dependencies pinned in `[workspace.dependencies]`
- [x] Schema and migrations, WAL mode, `busy_timeout`
- [x] Lifecycle states as enums, transitions as functions returning `Result`
- [x] Repository layer: every mutation in `BEGIN IMMEDIATE` with its event row; `rev` conflict errors
- [x] ULID and display id minting; reservations
- [x] Check: property test with N concurrent writer processes, no lost updates
- [x] Check: write killed mid-transaction leaves no half state
- [x] Check: migrations run both ways on a fixture
- [x] Delivered

### T2.2 `tiller` CLI
`sonnet` high - 3-4 h - after the T2.1 interface

- [x] Every command in `protocol.md`, AXI standard
- [x] Check: insta snapshot of each command's JSON output plus an exit code test
- [x] Check: same idempotency key twice has no second effect
- [x] Check: startup under 10 ms on the personal Mac
- [x] Delivered

### T2.3 Backup, restore and export
`opus` medium - 2-3 h - after the T2.1 interface

- [x] Nightly `sqlite3 .backup` inside each host's boundary
- [x] `tiller restore`, JSONL export and import, schema migration tool
- [x] Check: restore drill recovers an acknowledged card with its pending decision
- [x] Delivered

### T2.5 Delivery and herdr adapter
`opus` high - 3-4 h - after the T2.1 interface

- [x] Outbox: store, then wake-up line with message id, then delivery states; idempotent retries; target-pane session check
- [x] herdr adapter: one poller per source, no overlapping polls, backoff, last-success timestamps
- [x] herdr layout rules applied (nested workspaces, glyph repair)
- [x] Check: end-to-end tests for a busy pane, a permission dialog, a gone pane, a duplicate send, a crash between store and send
- [x] Delivered

### T2.6 Observations
`opus` medium - 3-4 h - after the T2.1 interface

- [x] `tiller hook <event>` writes straight to SQLite, no daemon round trip
- [x] Transcript tailer, process liveness checks; hook structs tolerate unknown fields
- [x] Check: hook p95 under 10 ms, also with the daemon down
- [x] Check: interrupts, `StopFailure`, permission prompts and session end detected in tests
- [x] Delivered

### T3.1 Board app shell
`sonnet` high - 2.5-3.5 h - after CP2 (fixtures until T2.8)

- [x] Sidebar: view knob, project toggles
- [x] Sidebar Machines section: health dot, running vs free slots, "view only" tag, click filters with a removable header chip, stale state
- [x] Sidebar quota footer: dot (green, amber, red), tiny meter, "% week left", reset time; daemon state in tooltip; click opens Health
- [x] File-based routing, theme (system default), density, Cmd+K palette
- [x] QueryClient with boot-snapshot hydration, SSE cache writer, query option factories over the generated schema
- [x] Check: screenshots match the mocks, both themes, 1440 px and 390 px
- [x] Check: cold load is one complete first render under 100 ms, CLS 0, no pending UI
- [x] Check: quota dot states from fixtures; no email or account id anywhere
- [x] Delivered

### T4.1 Roles (drafting can start here)
`fable` high for the orchestrator, `opus` high for the rest - 4-6 h - final after CP2

- [x] `roles/`: orchestrator, scout, builder, reviewer, verifier, each with job, tools, model and effort defaults, done evidence, handoff format
- [x] Orchestrator never edits project code; reviewer has no memory tool
- [x] Settings fragment with deny rules per role
- [x] Check: each role launched through its real launch form; instructions, tools, model and memory verified at startup and on resume
- [x] Delivered

---

## Wave D: Core lands (day 3-5)

### T2.4 Importer
`opus` medium - 2-3 h - after T2.1 and the T0.4 bundle

- [x] `tiller import <bundle>` for `tiller-import/v1`; dry run prints the full plan
- [x] Real run writes state.db; memory becomes a placement plan for approval, with `legacy-import` provenance
- [x] Check: importing the dry-run bundle reproduces every card count in its manifest
- [x] Check: a second import changes nothing
- [x] Delivered

### T2.7 Reconcile engine
`fable` high or `opus` xhigh - 4-6 h - after T2.5 and T2.6 - critical path

- [x] Reconcile loop over cards, runs, panes, worktrees and PRs, per `recovery.md`
- [x] Board column derivation
- [x] Check: every recovery table row has a passing end-to-end test
- [x] Check: conflicting evidence ends as `needs` or `unknown`, never a destructive action
- [x] Delivered

### T2.8 Daemon and service
`opus` high - 2-3 h - after T2.1, integrating T2.5 to T2.7

- [x] Single-instance lock; runs pollers, reconcile loop and the quota feed
- [x] axum board API: HTTP plus one SSE stream; `index.html` with the boot snapshot; hashed assets with `Cache-Control: immutable`
- [x] `contract/openapi.json` through utoipa, CI fails when stale; localhost health endpoint
- [ ] home-manager module: launchd on macOS, systemd user unit on Linux, through a dotfiles PR
- [ ] Check: survives a kill and a reboot on both personal hosts
- [x] Check: idle CPU and memory measured and reported
- [x] Check: browsers resume SSE after a daemon restart, no reload, no missed event
- [ ] Delivered

### T3.2 Board views
`sonnet` high - 3-4 h - after T3.1

- [x] Board: all-projects swimlanes; All / Active / Needs you filter in the header on desktop, tab row on phones
- [x] Agents: orchestrators with nested `├`/`└` workers; role, model, status, last output line
- [x] Agents: "Open in herdr" button (key O, tooltip), remote agents open herdr on that machine first; detail panel with the `herdr agent focus` command and recent events
- [x] Card detail: timeline, runs, PR and head SHA, bound approval, events, freshness
- [x] Check: 4 projects and 40 cards fit at 1440x900 without page scroll, both densities
- [x] Check: "Open in herdr" focuses the right pane, local and remote
- [x] Check: live updates with no reload or flicker; no pending state between views
- [ ] Delivered

### T3.3 Attention inbox and actions
`opus` medium - 3-4 h - after T3.1 and T2.2

- [x] Global attention queue: question, evidence, recommendation, consequence, still current or not
- [x] Permission prompts kept separate from decisions
- [x] Actions with receipts (recorded, delivered, done): answer, approve bound to head SHA, reprioritize, comment, add card, open in herdr
- [x] Check: a second click repeats nothing
- [x] Check: approving a stale SHA is refused with a readable message
- [x] Delivered

### T4.2 Spawn, capacity and layout
`opus` high - 3-4 h - after T2.5, T2.7, T4.1 - critical path

- [x] `tiller spawn`: atomic capacity reservation; quota check per account floor with a fixed share per host
- [x] Treehouse worktree, herdr workspace nested under the orchestrator, role and brief injected, run recorded
- [x] `tiller done` refuses while commits are unlanded
- [x] Check: two simultaneous spawns cannot oversubscribe capacity
- [x] Check: cleanup never removes unlanded work
- [x] Delivered

### T4.3 Guards and landing
`opus` high - 3-4 h - after T1.4 and T2.1

- [x] PreToolUse backstop hooks and deny rules from `authority.md`
- [x] `tiller land`, daemon only, merges after a bound approval, rechecks the head SHA just before merging
- [x] Check: every alternate path tested, blocked or documented as convention
- [x] Check: a push after approval invalidates the approval
- [ ] Delivered

### T4.4 Intake wiring
`opus` medium - 2-3 h - after T2.6 and T1.3

- [x] UserPromptSubmit hook records the ask; `tiller ask resolve`; Stop hook in observe mode; card tags in replies
- [x] Unresolved asks stay visible
- [x] Check: three interleaved asks map to the right cards
- [x] Check: a board-created card is never created twice
- [x] Delivered

## CP3: Core end to end (developer, 15 min)

- [ ] `tiller` plus the daemon run against the imported dry-run bundle, with the restore drill and recovery tests passing

---

## Wave E: Board and agents finish (day 5-6)

### T3.4 Degraded states
`sonnet` medium - 1.5-2 h - after T3.2

- [x] herdr disconnected, stale lanes with last-updated time, stale machine with its reason, unknown agent state, reconnecting banner, empty projects
- [x] Check: each state triggers from a fixture and has a screenshot
- [x] Delivered

### T3.5 Health page
`sonnet` high, `dataviz` skill - 2-3 h - after T3.1

- [x] Verdict line plus one thing to watch
- [x] Four signals with readings, status, chips and 8-week trends: Shipping, Your time, Quality, Safety
- [x] Claude quota block: plain sentence, used vs elapsed meter, other windows, reserve and burn chips, Lisbon times
- [x] "Show the numbers", collapsed by default
- [x] No Firstmate comparison anywhere
- [x] Check: every number traces to a state.db query or the quota feed, with a test
- [x] Check: no email or account id; screenshots match the mocks, both themes, both widths
- [x] Delivered

### T3.6 UI pass
`sonnet` high - 1.5-2 h - after T3.2 to T3.5

- [x] Pixel review of every screen, both themes, 1440 px and 390 px
- [x] Keyboard navigation, focus rings, contrast
- [x] Check: screenshots attached, nothing clipped or overflowing, no console errors
- [x] Delivered

## CP4: Live board (developer, 15-20 min)

- [ ] Developer uses the live board against the daemon

### T4.5 Handoff and resume
`sonnet` high - 1.5-2 h - after T2.2

- [x] `tiller handoff` records active cards, pending decisions, workers, conclusions and next action
- [x] Orchestrator signals breakpoints; only the developer clears a session
- [x] Check: a fresh orchestrator answers three scripted state questions right from the handoff record alone
- [x] Delivered

### T4.6 Evals
`opus` medium - 2-3 h - after T4.1

- [ ] Skill-trigger evals with `claude plugin eval --no-publish` (deferred until tiller ships its first skill)
- [x] Role behavior evals with a holdout set; pinned judge and rubric
- [ ] No-skill baseline per skill (deferred until tiller ships its first skill)
- [x] Infrastructure tests kept separate
- [ ] Check: every eval runs in CI on demand; the holdout is never used for tuning
- [ ] Delivered

## CP5: Supervised live trial (developer, 30-45 min)

- [ ] On a throwaway sandbox repo: ask, card, worker, PR, bound approval, merge

---

## Wave F: Replay gate (day 6-7)

### T5.1 Replay harness
`opus` high - 2-3 h - after M4 - critical path

- [ ] Asks from `baseline/replay.jsonl` fed through intake and the orchestrator in sandbox mode
- [ ] Dispatch, landing and pane sends stubbed; every decision recorded
- [ ] Check: 30-50 asks with no side effects, proved by a sandbox audit
- [ ] Delivered

### T5.2 Replay run and difference report
`opus` high, pinned judge - 1.5-2 h - after T5.1

- [ ] Each disposition, kind, model, effort and hold compared with Firstmate
- [ ] Each difference classified better, worse or neutral with quoted evidence
- [ ] Check: summary states its uncertainty
- [ ] Check: every "worse" has a fix or the developer's acceptance

## CP6: Replay go or no-go (developer, 20-30 min)

- [ ] Go or no-go judged from the difference report

---

## Federation: one board across hosts (beside waves E and F)

ADR 0003, accepted Tuesday 2026-11-03. The owner approved it; runs in one or two lanes beside the replay gate and must finish before T6.3. F2 to F4 run in order because they share `crates/daemon` and `crates/cli`; F5 runs beside them against the mocked API.

### F1 Land the federation spec
`sonnet` medium - 0.5 h - after the owner approves ADR 0003

- [x] ADR 0003 status set to accepted with the owner's three choices
- [x] `state.md`, `protocol.md`, `authority.md`, `recovery.md` and `cp2-open.md` edits merged as reviewed
- [x] Delivered

### F2 Pairing and keys
`opus` high - 2.5-3.5 h - after F1

- [ ] Migration with a down step: `hosts.peer_scope`, `hosts.peer_host_id`, `peer_keys`; new stale reasons
- [ ] `tiller peer add` over `ssh -t`, `peer accept`, `peer list`, `peer remove`, `peer revoke`; Ed25519 key file at mode 0600
- [ ] Config `federation.accept`; `peer.accept`, `peer.revoke`, `peer.pair`, `peer.remove` with their events
- [ ] Check: `migrations_run_both_ways_on_fixture` covers the new migration; owner-only checks refuse a session caller
- [ ] Delivered

### F3 Peer link and federated reads
`opus` high - 3-4 h - after F2

- [ ] `tiller peer serve`: start check, hello and welcome, snapshot, events, beats, `query`
- [ ] Daemon link client: one task per peer, link states, backoff, resume from `last_seq`, 2 s coalesced snapshot refresh, clock offset
- [ ] Federated reads tag `host`; `?host=`; snapshot `peers`; SSE `peer`, `link`, `peer_reset`; Machines row from the link; OpenAPI regenerated
- [ ] Check: `daemon::peer::snapshot_then_events_fill_cache`, `resume_from_last_seq_replays_gap`, `gap_over_5000_sends_snapshot`, `link_states_follow_frame_age`, `frozen_serve_reaches_down_and_reconnects`, `serve_never_opens_onward_link`, `cli::peer::serve_refuses_session_and_claude_ancestor`
- [ ] Delivered

### F4 Forwarded mutations
`opus` high - 3-4 h - after F3

- [ ] `mutate` frames signed and verified in the order `protocol.md` 10.3 gives; the allowlist on both hosts
- [ ] 502, 503 and 504 codes; idempotent retry; actor `board@<label>` with `via_peer`; the unauthorized-action detector accepts it
- [ ] Deny rule `Bash(tiller peer *)` and backstop `[peer]`; the G41, G43 and G44 tests not already in F3; live `sandboxed_role_cannot_reach_peer_ssh`
- [ ] Check: `daemon::peer::mutate_bad_sig_refused`, `mutate_replayed_ts_refused`, `mutate_wrong_host_id_refused`, `revoked_key_refused`, `read_scope_cannot_mutate`, `spawn_not_forwardable`, `forwarded_precondition_failure_passes_through`, `same_key_twice_applies_once`, `unreachable_returns_503_and_queues_nothing`, `forwarded_mutation_never_writes_viewer_db`, `frames_never_carry_cookie`; `agents::guards::peer_commands_denied`, `ssh_remote_tiller_denied`
- [ ] Delivered

### F5 Board: host chips and staleness
`sonnet` high - 2.5-3.5 h - after F1 (mocked API until F3)

- [ ] Host chip on cards, attention and agents; host-qualified display ids when ambiguous
- [ ] A peer that is not `live`: dimmed rows, "as of" with the age in Lisbon time, disabled controls with the reason
- [ ] "Sent, not confirmed" for 504; the project picker grouped by host; Machines link state; `read` pairings view only
- [ ] Check: screenshots in both themes at 1440 px and 390 px; perfect-load rules hold with peers in the snapshot
- [ ] Delivered

### F6 Two-host end to end
`sonnet` high - 1.5-2 h - after F4 and F5

- [ ] Two homes on one machine joined through an SSH shim; a board with both hosts' cards
- [ ] A forwarded answer reaches the peer's orchestrator through its outbox
- [ ] Check: with the peer frozen (`SIGSTOP`), the local board, its writes and its SSE are unaffected and the peer's cards show "as of"
- [ ] Delivered

## CP-F: Federated board (developer, 15 min)

- [ ] Developer uses the two-host board from F6: answers a peer's card, freezes the peer, sees the staleness

---

## Cutover (one host at a time)

Each flip needs every kept parity row for that host passing, or live-checked at CP7. Rollback triggers: lost work, an unauthorized action, a duplicate dispatch.

One-time owner setup, per host and repo (CP2, S03):

- [ ] Create one GitHub App owned by the owner's account (for example `tiller-merge`), with only the permissions `authority.md` lists for it; store its private key where only the daemon can read it (`authority.md`, Enforced merge gate)
- [ ] For each gated repo (delivery `no-mistakes` or `no-mistakes-prod-only`, plus any project opted in) on the host being cut over: install the App, add a default-branch ruleset whose only bypass actor is the App, and confirm `tiller doctor` shows `merge_gate = enforced`. Until then that repo runs as `detect`, the labelled fallback
- [ ] Choose the private remote the host's memory repo pushes to (`memory.remote`, C24)

### T6.1 Personal Mac
`opus` high, developer present - 1-2 h

- [ ] CP7 go for the personal Mac
- [ ] Parity rows for this host passing
- [ ] Firstmate dispatch frozen; in-flight work drained
- [ ] Export run with `MODE=final`, imported
- [ ] Owner marker flipped; dotfiles repointed through a PR; services started
- [ ] T0.5 demo deleted

### T6.2 Watch for 48 hours
`sonnet` medium

- [ ] Rollback triggers checked by the daemon, no token cost
- [ ] Day 1 summary sent
- [ ] Day 2 summary sent

### T6.3 Remote host
`opus` high - 1.5-2.5 h - after 2-3 stable days on the personal Mac and CP-F

- [ ] CP7 go for the remote host
- [ ] The sandbox starts on this host; until it does, the host is not cut over (CP2, C19)
- [ ] Parity rows for this host passing
- [ ] Same steps as T6.1
- [ ] `tiller peer add` from the personal Mac; the link is `live`; the remote host's cards appear on the personal Mac's board
- [ ] Live check: one forwarded answer reaches the remote orchestrator; with the link cut, the cards show "as of" and controls are disabled

### T6.4 Work Mac
`opus` high, agent session on the work Mac - 1-2 h

- [ ] CP7 go for the work Mac
- [ ] Parity rows for this host passing, using the reduced behavior the probe found
- [ ] Same steps as T6.1; data stays on that machine
- [ ] `federation.accept = false` set, unless the owner chose otherwise for ADR 0003

---

## After day one

### T7.1 Health depth
`sonnet` high, `dataviz` skill - 2-3 h

- [ ] Each time the developer was needed classified as correction, unplanned escalation or rework
- [ ] Relabeling from "Show the numbers"; sample size on every figure
- [ ] Same numbers computed over Firstmate's history for the T7.4 report, never shown on the Health page
- [ ] Delivered

### T7.2 Memory page
`opus` high - 6-9 h

- [ ] One editable document per project, "All projects (shared)" first
- [ ] Sections with one line of prose per entry; provenance hint (card id, date) opening source file, approval and loaded-by sessions
- [ ] Click to edit, add or remove a line; each change commits the real file under git through the scrub hook
- [ ] Agent proposals inline with Keep and Discard, no separate queue
- [ ] "Recent changes" rail with Undo, which reverts that commit; retiring keeps history
- [ ] Delivered

### T7.3 Learning loop
`fable` high design, `opus` high build - 5-8 h

- [ ] Sampled retros, full retros for incidents and surprising wins
- [ ] Evidence-backed proposals for mistakes and successes
- [ ] Config hash on every run; one change at a time or a named bundle
- [ ] Holdout eval before any change is approved; "nothing worth saving" is a normal result
- [ ] Delivered

### T7.4 The two-week check
`opus` high - 1 h - two weeks after the last cutover

- [ ] Tiller's health numbers compared with Firstmate's baseline within the stated tolerances
- [ ] Report with uncertainty sent

## CP8: Keep Tiller, delete Firstmate (developer, 20 min)

- [ ] Every parity row matched and tested, or its drop approved
- [ ] Every host runs Tiller with Firstmate stopped
- [ ] Two-week check passed: output and quality no worse than the baseline
- [ ] Firstmate deleted, or fallback chosen
