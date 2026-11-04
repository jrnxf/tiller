# Tiller build plan

This is the executable plan for building Tiller to day one, cutting the fleet over to it, and finishing what comes after. It is written for the agent that runs the build over many days. Read all of it before starting.

Written Sunday 2026-11-01, late evening Lisbon time (WET).

## What Tiller is

Tiller replaces Firstmate as the developer's agent fleet system. It has five parts:

- **Orchestrators.** One per project. Each one runs in that project's herdr space and is the session the developer talks to about that project.
- **Cards.** Every ask becomes a card the moment it is sent. All state is durable: one SQLite database per host is the authority for workflow records.
- **The board.** A live local web app showing all projects at once, with an attention inbox and board actions. Keeping it live costs no model tokens.
- **The memory view.** Every memory the fleet loads, with its source and approval. Edits write back to the real files under git.
- **The learning loop.** Evidence-based proposals, approved by the developer, measured against a historical baseline.

## Why Tiller exists, and the bar it must clear

In the developer's words: "The more I work with [Firstmate] the more I wish I had full ownership and control of it. I'm up for creating my own system as long as it's feasible to create something comparable and my output will not be diminished. Output and quality - both very important."

That gives Tiller three tests. Every one of them must hold before Firstmate is deleted:

1. **Ownership and control.** The developer owns every line and every contract. Nothing important depends on a project they do not control. herdr, treehouse, no-mistakes and the axi tools stay, because they are standalone tools the developer already trusts.
2. **Comparable.** Every Firstmate capability the developer actually uses has a Tiller equivalent, or the developer has explicitly approved dropping it. The parity matrix (T0.6) is the checklist.
3. **Output and quality are not diminished.** Throughput, rework and escaped defects are no worse than Firstmate's historical baseline, within the stated tolerances, as measured by the two-week check (T7.4). If they are worse, fix Tiller or fall back. Firstmate is stopped but never deleted before CP8. The Health page shows only Tiller's own trends, as the developer asked; the baseline comparison is a report, not UI.

## Inputs (read these first)

| Doc | Lives at (moved here by task T0.2) |
|---|---|
| Original plan plus Revisions 1 and 2 | `docs/design/plan.md` |
| Second opinion | `docs/design/second-opinion.md` |
| Response to the second opinion, the current plan v2 | `docs/design/plan-v2.md` |
| This build plan | `docs/plan/build-plan.md` |
| Progress checklist (ticked as work lands) | `docs/plan/checklist.md` |
| Firstmate export prompt (the import contract) | `docs/firstmate-export.md` |
| Design basis (Linear tokens, working mock) | `docs/design/design-basis.html` (pending: the source is a Lavish file on the personal Mac; see `docs/design/README.md`) |
| UI mocks, approved at CP-UI | `docs/design/mocks.html` (pending, as above) |
| herdr layout recommendation | `docs/design/herdr-layout.md` |
| Firstmate contract docs (MIT): `captain-hold-lifecycle`, `arm-pretool-check`, `agent-control`, `subagent-guard`, `cd-guard`, `supervision-host`, `fleet-ledger`, `herdr-backend` | the Firstmate home's `docs/`: read in place; reference them, do not copy them |
| Board conventions reference: the developer's two TanStack apps, especially their `CLAUDE.md` loader and suspense rules and their lint, format and component setup | in place: read for conventions only; never name them in the repo |

When v2 and v1 conflict, v2 wins. When a task's acceptance criteria conflict with a doc, stop and ask (see "Stop and ask").

## Hard rules

1. **Public by default.** Write the repo as if it will become public. Nothing committed or posted may contain:
   - internal hostnames, LAN addresses or `lan.` links;
   - absolute local paths or machine names;
   - personal file names;
   - names of the developer's other private projects.

   Tests and examples use `example.com`, `192.0.2.x` and `/home/user`. Task T0.2 scrubs the design docs before they are committed.
2. **No secrets.** Never read `.env`, `.envrc`, key files, `*.pem`, `*.key` or vault output. When a command needs a secret, ask the developer to run it.
3. **Install only through the Nix dotfiles** (the `dotfiles-install` skill). Never use `brew install`, `npm i -g` or `cargo install` ad hoc. Tiller ships as a Nix flake package plus a home-manager module.
4. **Delivery path.** Each task is one branch, one worktree and one PR. Before the PR is opened, it gets an independent `/code-review`. When the task has a spec section, it also gets `mattpocock-skills:code-review` against that spec. `no-mistakes` is the shipping gate, not a substitute for review. Merges happen only when the developer says so.
5. **Writing style.** Never use the em dash; use "-". Never credit the agent in commits or PRs: no Co-Authored-By line and no "Generated with" line. Show every date in Europe/Lisbon time with its weekday.
6. **Quality.** Fix any lint error, test failure or flaky test you see, even when your task did not cause it. Treat UI as pixel-exact: check screenshots in both themes at 1440 px and 390 px wide.
7. **Firstmate is read-only** during the build, except for steps the developer approves at a checkpoint (the fleet ledger flag, the export runs, and the cutover itself).
8. **Quota.** Before starting each wave, check the quota with `quota-axi`. Never dispatch below the 3% weekly floor; hold the wave and tell the developer.
9. **Never trade output or quality for speed of the build.** When a shortcut would drop a capability in the parity matrix, or weaken a quality gate (review, tests, `no-mistakes`, bound approvals), stop and ask.

## Who runs this

The intended executor is **Firstmate's first mate**: Firstmate builds its own replacement. In Firstmate's terms:

- each task below is one `ship` task (or a `scout` task where marked), and its brief is the task block;
- each checkpoint is a captain hold;
- each lane is a crewmate.

Add `tiller` as a Firstmate project with `no-mistakes` delivery before task T0.1 runs.

The model and effort given for each task override Firstmate's crew-dispatch defaults. They follow the same policy:

| Work | Model, effort |
|---|---|
| Trivial and mechanical | `haiku` low |
| Settled build: the brief or spec says exactly what to build | `sonnet` medium, or `sonnet` high when the surface is large |
| Ambiguous, but easy to undo | `opus` medium |
| Risky: data, concurrency, delivery, machine setup, authority, hard to undo | `opus` high |
| The few design decisions everything else rests on | `fable` high. If Fable is not on the plan or quota is tight, use `opus` xhigh |

Rust work that involves async concurrency (the daemon, delivery, reconcile, spawn) never goes below `opus`. If a `sonnet` lane loops on the borrow checker for more than three compile cycles on the same error, hand that task to `opus`.

**Progress tracking.** `docs/plan/checklist.md` is the one place progress is recorded. Each task's PR ticks that task's boxes in the same commit that does the work, so the checklist on the default branch always matches what has merged. The developer ticks checkpoint boxes, or tells the executor to. Before T0.2 landed, the checklist lived outside the repo; T0.2 carried its ticks over.

A single Claude Code session can also run this plan:

- use a worktree per lane;
- delegate each task to a subagent with the given model;
- keep the same checkpoints;
- ask the developer before running more than four agents at once.

## Decisions taken as defaults

Proceed with these defaults unless the developer changes them at checkpoint CP0.

| Decision | Default | Why |
|---|---|---|
| Language | **Decided by the developer, Sunday 2026-11-01:** Rust for the core (CLI, daemon, hooks, herdr adapter, SQLite); TypeScript for the board (Vite, React, TanStack Router and Query, no SSR). See "Stack" below | The core is the authority for state and approvals and must never destroy live work: the compiler forces error handling, exhaustive state matches and typed JSON at every boundary, which is where agent-written TypeScript most often fails silently. Rust is also first-class in Nix and ships as one 2 MB binary. Agents build UI far better in React. Measured cost of Bun was never the reason: a Bun CLI call took 9.5 ms against 2.0 ms for Rust. |
| Repo | `tiller`, private at first, under the developer's GitHub account; MIT license | Ready to make public. |
| Remote lanes | Read-only in v1, plus "Open in herdr" through herdr's saved machine. **Accepted Tuesday 2026-11-03 (ADR 0003):** one federated board; peers are read live over SSH, and the owner's board mutations forward to the owning host, signed and re-checked there. Fleet roles never act across hosts | A new control route needs an explicit change to the constraints. ADR 0003 is that change, asked for by the owner. |
| Work data | The work Mac runs its own Tiller; its board and data never leave that machine. Under ADR 0003 it sets `federation.accept = false` unless the owner chooses otherwise | Data boundary. |
| Multi-host | Each project has exactly one owner host; running a project on several hosts at once is not allowed in v1 | Simplicity. |
| Auto memory for fleet roles | Off (`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`); approved memory loads through CLAUDE.md and imports | Nothing loads without approval. |
| Worker launch form | `claude --append-system-prompt-file <role>.md`; never `--agent`, which replaces the default system prompt | Documented behavior. |
| Worktrees | Treehouse owns the worker worktree lifecycle; native `isolation: worktree` is not used for workers | One owner. |

## Stack

### Core (Rust)

- **Toolchain.** Rust stable, edition 2024, pinned in `rust-toolchain.toml` and provided by the Nix dev shell (no rustup). `sccache` is the `RUSTC_WRAPPER` in the dev shell, so parallel worktrees do not each pay a cold build.
- **One binary, `tiller`.** It holds `tiller daemon`, `tiller hook <event>` and every AXI command. Hooks write to SQLite directly, so they still work while the daemon is down.
- **Libraries:** tokio; axum for HTTP and SSE; rusqlite with bundled SQLite, plus rusqlite_migration; serde and serde_json; clap; utoipa for the OpenAPI contract; rust-embed for the board's assets; tracing; thiserror in libraries and anyhow only in the binary; ulid; notify for transcript tailing; jiff for time (every displayed time is Europe/Lisbon). Add others only when a task needs them.
- **Quality bar:**
  - clippy runs with `-D warnings`, and `unwrap_used` and `expect_used` are denied outside tests;
  - rustfmt formats the code;
  - `cargo nextest` runs the tests, insta snapshots the CLI's JSON output, and proptest drives the property tests.
- **Workspace layout:**
  ```
  crates/core        state, ids, schema, migrations (T2.1); backup (T2.3); import (T2.4)
  crates/herdr       herdr socket and CLI adapter, layout rules (T2.5)
  crates/delivery    outbox (T2.5)
  crates/observe     hook mapping, transcript tailer, liveness (T2.6)
  crates/reconcile   recovery table, column derivation (T2.7)
  crates/agents      spawn, guards, land, intake, handoff (T4.2-T4.5)
  crates/daemon      axum server, SSE, OpenAPI, board embedding, service files (T2.8)
  crates/cli         the `tiller` binary; wires every crate (T2.2)
  crates/replay      replay harness (T5.1)
  board/             the web app (M3)
  contract/          generated openapi.json
  ```
  The T2.1 interface step creates every crate and module as stubs and pins all known dependencies in `[workspace.dependencies]`. That way, parallel lanes fill in their own files and do not fight over `Cargo.toml` or `lib.rs`.

### Board (TypeScript)

- **The app.** Vite, React 19, TanStack Router (file-based routes, router plugin) and TanStack Query. It is a single-page app with no SSR. It is built to static files, embedded in the `tiller` binary, and served by the daemon on localhost.
- **Why not TanStack Start.** Start's SSR needs a JavaScript server running beside the Rust daemon: a second runtime to supervise and package, and server functions that would duplicate the Rust API. The boot snapshot (see M3) gives the same perfect first load without it.
- **Libraries, matching the conventions reference:** Tailwind v4; Base UI with shadcn components; cmdk; sonner; lucide-react; `@tanstack/react-hotkeys`; TanStack Virtual; dnd-kit for dragging cards.
- **Tooling:** bun as the package manager and script runner; `tsc` in strict mode; oxlint; oxfmt; knip; react-doctor; vitest.
- **The contract.** The daemon exports OpenAPI through utoipa to `contract/openapi.json`, including the SSE event schemas. `openapi-typescript` generates `board/src/lib/api/schema.gen.ts` from it. Hand-written query option factories per domain (`cards.list.queryOptions()`) wrap that schema. CI fails when either generated file is stale.

## Checkpoints

At each checkpoint, stop, put a hold on the next wave, and message the developer in this shape:

1. One line saying what is ready.
2. What they need to do, as numbered steps with a time estimate.
3. Evidence: test output, screenshots, links.
4. One risk line: `Risk: two-way door` or `Risk: one-way door - <what cannot be undone>`.

Keep each list to five items or fewer, with no preamble and no recap.

| CP | After | The developer decides | Their time |
|---|---|---|---|
| CP0 | before T0.1 | the defaults above; turning on the Firstmate fleet ledger; the optional alpha secondmate trial; repo creation | 5-10 min |
| CP1 | wave A | the probe results from all three hosts (they run it on the work Mac); the dry-run bundle's open questions; the board demo; the minimum common behavior across hosts; the parity matrix, approving each proposed drop or sending it back | 40-50 min |
| CP-UI | mocks reviewed | **Done Sunday 2026-11-01:** the developer approved the third pass of the mocks. M3 and M7 below follow them | done |
| CP2 | wave B | approves the specs: state, recovery, protocol, authority, memory. This is the last cheap moment to change the contracts | 30-45 min |
| CP3 | wave D core | the core end to end: `tiller` plus the daemon against the imported dry-run bundle, with the restore drill and recovery tests passing | 15 min |
| CP4 | wave E board | uses the live board against the daemon | 15-20 min |
| CP5 | wave E agents | a supervised live trial on a throwaway sandbox repo: ask, card, worker, PR, bound approval, merge | 30-45 min |
| CP6 | wave F | replay gate go or no-go, judged from the difference report | 20-30 min |
| CP-F | federation (F6) | uses the two-host board: answers a peer's card, freezes the peer, sees the staleness (ADR 0003) | 15 min |
| CP7 | before each host flips | cutover go for that host | 10 min per host |
| CP8 | 2 weeks after the last cutover | the two-week check report (T7.4) against the baseline. Delete Firstmate, or fall back | 20 min |

## Stop and ask

Put a hold on the work and ask the developer when any of these happens:

- a contract in a spec has to change after CP2;
- a task fails its acceptance criteria twice;
- a host cannot reach the minimum common behavior;
- anything would write to Firstmate, the developer's dotfiles, GitHub settings or another repo beyond a normal PR;
- quota is below the floor;
- a test flakes and the cause is not in this repo.

## Parallel waves

The critical path is: T1.1, then the specs, then T2.1, T2.5 and T2.6, then T2.7, then T4.2, then T5.1, then T6.1. Run at most four lanes at once (five if lane capacity and quota allow). Two lanes must never edit the same module; each task names the files it owns.

```
Wave A (day 1)      T0.1 bootstrap | T0.3 probe | T0.4 Firstmate dry run | T0.5 board demo
                    T0.2 doc import (after T0.1) | T0.6 parity matrix (after T0.4)
                                                                   -> CP0 at the start, CP1 at the end
Wave B (day 1-2)    T1.1 state spec (alone)
                    T1.2 recovery | T1.3 protocol | T1.4 authority | T1.5 memory (in parallel)
                    T1.6 spec review                               -> CP2
Wave C (day 2-3)    T2.1 state core (interface first, about 1.5 h, then the rest)
                    T2.2 CLI | T2.3 backup | T2.5 delivery and herdr | T2.6 observations (after the T2.1 interface)
                    T3.1 board shell (against the mocked API from spec) | T4.1 role drafts (prompts only)
Wave D (day 3-5)    T2.4 importer | T2.7 reconcile | T2.8 daemon and service
                    T3.2 board views | T3.3 attention and actions
                    T4.2 spawn and layout | T4.3 guards | T4.4 intake wiring -> CP3
Wave E (day 5-6)    T3.4 degraded states | T3.5 health page, then T3.6 UI pass -> CP4
                    T4.5 handoff | T4.6 evals                       -> CP5
Wave F (day 6-7)    T5.1 replay harness, then T5.2 replay run      -> CP6
Federation          F1 spec, then F2 pairing, F3 link, F4 forwarding | F5 board UI (mocked API), then F6 e2e -> CP-F
  (beside E and F, after the owner approves ADR 0003; done before T6.3)
Cutover             T6.1 personal Mac, T6.2 watch, T6.3 remote Linux host (after CP-F), T6.4 work Mac -> CP7 per host
After               T7.1 health depth | T7.2 memory view, then T7.3 learning loop -> CP8
```

The days are a rough guide only. The calendar depends on how fast checkpoints get answered.

## Sizing

Every estimate below is agent wall-clock time for one lane, including that task's review and CI.

| Scope | Agent-hours, summed | Critical path with 4 lanes |
|---|---|---|
| Day one (M0 to M5) | 88-117 h | 40-56 h |
| Federated board (F1 to F6, ADR 0003) | 13-17.5 h | 10.5-14 h, beside waves E and F, off the day-one critical path |
| Cutover, all three hosts | 4.5-6.5 h | 4.5-6.5 h, plus stability days between hosts |
| After day one (M7) | 14-21 h | 10-14 h |

Day one is higher than the 50-70 h in plan v2 for three reasons: this plan adds the spec phase (M1), counts review and CI on every task, and adds about 8-12 h for the Rust core (compile loops and async work) and the two-toolchain setup. The approved mocks moved the Health page into day one (T3.5, 2-3 h) and shrank the after-day-one health work (T7.1).

---

## M0: Foundations

### T0.1 Repo bootstrap
- **Goal:** Set up the `tiller` repo, following "Stack":
  - a Cargo workspace with the `cli` crate only, plus clippy, rustfmt and `cargo nextest`;
  - `board/`: a Vite and TanStack Router app with a placeholder route, set up with bun, oxlint, oxfmt, knip and vitest the way the conventions reference does;
  - a Nix flake:
    - crane builds `tiller` with the board's built assets embedded;
    - the board builds in its own derivation from `bun.lock`;
    - a dev shell provides the Rust toolchain, bun, sccache and sqlite3;
    - a home-manager module stub;
  - CI on GitHub Actions running clippy, the format checks, nextest, and the board's lint, typecheck and tests;
  - `no-mistakes` configuration;
  - an MIT `LICENSE`, a `README.md`, and an `AGENTS.md` that holds this plan's hard rules and the repo commands for both halves.
- **Depends on:** CP0.
- **Model, effort:** `sonnet` high (two toolchains and Nix).
- **Estimate:** 2-3 h.
- **Owns:** the repo root, `flake.nix`, `.github/`, `board/` configuration.
- **Acceptance:**
  - CI is green.
  - `tiller --version` works, and `tiller daemon` serves the placeholder board from the embedded assets.
  - `nix build` produces one binary on macOS, and on Linux through the remote host's normal build path.
  - A cached rebuild of one crate inside a worktree finishes in under 10 s with sccache warm.
  - **Tripwire:** if building the board inside Nix (bun2nix) fights for more than about 1 h, fall back to a fixed-output derivation that runs `bun install --frozen-lockfile` with a pinned output hash. Report which path was taken.

### T0.2 Import and scrub the design docs
- **Goal:** Copy the inputs table into `docs/design/` and scrub them so the public-by-default rule holds:
  - replace machine names with roles (host A is the personal Mac, host B the remote Linux host, host C the work Mac);
  - remove local paths and private project names;
  - keep the meaning.
  Also add `docs/adr/0001-replace-firstmate.md` and `docs/adr/0002-sqlite-authority.md`.
- **Depends on:** T0.1.
- **Model, effort:** `opus` medium.
- **Estimate:** 1-1.5 h.
- **Owns:** `docs/`.
- **Acceptance:**
  - A scrub check script (`scripts/scrub-check.sh`) finds no hostnames, paths or private names.
  - The script runs in CI from now on.

### T0.3 Capability probe
- **Goal:** Write `probe/tiller-probe.sh`. It is read-only and writes a JSON plus markdown report. It checks:
  - whether user, project and plugin hooks fire under managed settings (`allowManagedHooksOnly`, `disableAllHooks`), tested in a throwaway project directory with a harmless hook that only touches a temp file;
  - whether a launchd or systemd user service is allowed;
  - binding a localhost port;
  - herdr CLI and `api snapshot` access;
  - how auto-memory loads, and whether `CLAUDE_CODE_DISABLE_AUTO_MEMORY` is honored in a herdr-launched session;
  - `claude --append-system-prompt-file`;
  - `claude plugin eval --no-publish`;
  - `sqlite3` and the `tiller` binary running from the Nix store (the T0.1 build, or a hello-world binary if T0.1 has not landed);
  - transcript paths.

  Run it on the personal Mac and the remote host. Give the developer the one-line command for the work Mac.
- **Depends on:** nothing (it can live in the repo once T0.1 lands).
- **Model, effort:** `opus` high (it runs on a managed machine).
- **Estimate:** 1-1.5 h, plus about 10 min per host.
- **Owns:** `probe/`.
- **Acceptance:**
  - The probe changes nothing outside a temp directory, and a before and after diff proves it.
  - Reports exist for both personal hosts.
  - The minimum common behavior is drafted in `docs/design/host-capabilities.md`.

### T0.4 Firstmate dry-run export (`scout`)
- **Goal:** Run the export prompt (`docs/firstmate-export.md`) inside Firstmate with `MODE=dry`, on the personal Mac and on the remote host. Include Part C, which produces the baseline and replay set. If the developer approves the fleet ledger at CP0, turn it on first, so the next weeks produce real event data.
- **Depends on:** CP0.
- **Model, effort:** the first mate runs it. The Part C scout uses `opus` medium.
- **Estimate:** 1-1.5 h per host.
- **Owns:** nothing in the repo. The bundle stays local, under `~/.tiller/import/`.
- **Acceptance:**
  - Every Part G check passes and is shown.
  - The open questions are listed for CP1.

### T0.5 Throwaway board demo
- **Goal:** Build the gate 2 demo. It is a single local server (one Bun script is fine; it is throwaway and not part of the Rust core) that polls Firstmate's fleet snapshot and herdr (locally and on the saved machine), and pushes updates over SSE into one Linear-styled page with all projects visible. It is read-only and has jump-to-pane for local panes. It stays running as the interim board until cutover.
- **Depends on:** CP0.
- **Model, effort:** `sonnet` medium.
- **Estimate:** 1-1.25 h.
- **Owns:** `demo/`. It is not part of the product and is deleted at cutover.
- **Acceptance:**
  - It runs from one command.
  - Real backlog data shows.
  - A screenshot in both themes is attached.

### T0.6 Firstmate parity matrix (`scout`)
- **Goal:** Write `docs/design/parity.md`, the checklist for "comparable". It has one row per Firstmate capability the developer actually uses. Build it from:
  - the dry-run bundle's `capabilities.md` and `memory/requirements.md`;
  - `commitments.jsonl` and `regressions.md`;
  - Firstmate's contract docs and `VISION.md`.

  Each row gives:
  - the capability, with evidence of real use (how often and when it was last used, from the bundle);
  - its Tiller owner task and the test that proves it;
  - or "drop", with the reason, for the developer to approve at CP1.

  Typical rows include: captain holds and dated holds, AFK mode, scheduled wakes and check-ins, secondmates, quota floors, crew dispatch and capacity, `no-mistakes` delivery, PR and CI watching, supervision and liveness, the fleet snapshot, remote lanes through herdr, and the skills the first mate calls. Only rows backed by real use count.
- **Depends on:** T0.4.
- **Model, effort:** `opus` high (missing a row is how output quietly drops).
- **Estimate:** 1-1.5 h.
- **Owns:** `docs/design/parity.md`.
- **Acceptance:**
  - Every capability in the bundle maps to a row.
  - Every row has an owner task and a test name, or is a drop awaiting the developer's approval at CP1.

---

## M1: Specs (the contracts everything else depends on)

Every spec lives in `docs/spec/`. Each one ends with a coverage list naming which of the second opinion's 43 points it covers, and which Firstmate contract docs it matches or deliberately departs from.

### T1.1 State model
- **Goal:** Write `docs/spec/state.md` plus a draft `schema.sql`, covering:
  - the entities and their stable ids (ULIDs) and display ids (`alpha-42`);
  - the relationships: asks to cards many to many, card to runs, run to session, run to PRs, and the project's owner host;
  - the lifecycle facts: work status, agent activity with `observed_at`, attention, dependencies, PR state;
  - the column precedence table and each card type's definition of done;
  - `rev` columns, and events written in the same transaction;
  - the outbox states (pending, delivered, acked, failed);
  - the approval states and their binding (repo, PR, head SHA, action, or diff plus base revision);
  - capacity reservations;
  - the fact-ownership table from plan v2.
- **Depends on:** CP1.
- **Model, effort:** `fable` high.
- **Estimate:** 3-4 h.
- **Owns:** `docs/spec/state.md`, `docs/spec/schema.sql`.
- **Acceptance:**
  - Every fact has exactly one owner.
  - Every column is derived, never stored.
  - Every mutation names its transaction and its event.

### T1.2 Recovery table
- **Goal:** Write `docs/spec/recovery.md`. Each row covers one scenario: daemon restart, orchestrator crash, worker crash, host reboot, remote disconnect, duplicate events, a PR merged outside Tiller, a stale approval, a failed delivery, a truncated backup, a schema migration. For each row give:
  - how the scenario is detected;
  - the invariants that must hold;
  - what is repaired automatically, and what goes to the developer;
  - the name of its end-to-end test.

  Seed the rows from the bundle's `regressions.md` and from Firstmate's hold-lifecycle and supervision docs.
- **Depends on:** T1.1 (a draft is enough).
- **Model, effort:** `fable` high.
- **Estimate:** 2-3 h.
- **Owns:** `docs/spec/recovery.md`.
- **Acceptance:**
  - No row can destroy unlanded work automatically.
  - Every regression in the bundle maps to a row.

### T1.3 Protocol and CLI
- **Goal:** Write `docs/spec/protocol.md`, covering:
  - ask intake, and `tiller ask resolve <ask> new|followup:<card>|chat|cancelled`;
  - the wake-up line format, and how hooks recognize it;
  - the mapping from every hook event to an observation (including all 13 `notification_type` values, `StopFailure`, and interrupts detected from transcripts);
  - the Stop hook in observe mode, with the rule for switching to a single block;
  - transcript tailing;
  - the full `tiller` command surface in the AXI style: structured output, stable ids, exit codes, idempotency keys, safe retries;
  - the board API:
    - HTTP endpoints for reads and mutations, with mutations carrying `rev`;
    - one SSE stream whose events carry entity, id and `rev`, and resume with `Last-Event-ID`;
    - the boot snapshot embedded in `index.html`;
  - the quota feed: the daemon runs `quota-axi --full` every 5 minutes (a script, so it costs no model tokens), drops the `accounts` block before storing anything, and exposes the windows (label, percent remaining, resets at, pace, reserve, burn multiple, elapsed percent, projected exhaustion). No email or account id ever leaves the daemon;
  - the machine summary per host: health, running agents, capacity, free slots, last seen, and a stale reason (herdr link down, Mac asleep).
- **Depends on:** T1.1.
- **Model, effort:** `opus` high.
- **Estimate:** 2-3 h.
- **Owns:** `docs/spec/protocol.md`.
- **Acceptance:**
  - Every command is idempotent or states why not.
  - Every hook event maps to exactly one handler.

### T1.4 Authority and guards
- **Goal:** Write `docs/spec/authority.md`, covering:
  - a matrix of which role can do what;
  - for each guard, whether it is enforced or only a convention;
  - permission deny rules per role, and the PreToolUse backstop;
  - merges done only by the daemon, after a bound approval;
  - the alternate paths to test (`gh api`, `curl`, raw `git push`, writing to the filesystem outside the worktree, deleting worktrees);
  - review, execute and merge kept as three separate decisions.
- **Depends on:** T1.1.
- **Model, effort:** `opus` high.
- **Estimate:** 1.5-2 h.
- **Owns:** `docs/spec/authority.md`.
- **Acceptance:** Every guarantee is labeled enforced or convention, and every enforced one has a test name.

### T1.5 Memory
- **Goal:** Write `docs/spec/memory.md`, covering:
  - the source map: each editable memory's canonical file and how it is applied (files managed by dotfiles are edited in their source and rebuilt);
  - scope, evidence, validity conditions, review status and provenance (including `legacy-import`);
  - the `observations` and `proposals` tables, which are never loaded into context;
  - approval binding to the diff plus its base revision;
  - retiring a memory, and showing which sessions loaded a revision;
  - the scrub hook on memory commits;
  - no promotion across the personal and work boundary.
- **Depends on:** T1.1.
- **Model, effort:** `opus` medium.
- **Estimate:** 1.5-2 h.
- **Owns:** `docs/spec/memory.md`.
- **Acceptance:** Nothing unapproved can reach a fleet role's loaded context.

### T1.6 Spec review
- **Goal:** Do an adversarial review of T1.1 to T1.5 with fresh context. Check them for:
  - full coverage of the second opinion;
  - full coverage of the parity matrix: every kept capability is specified somewhere;
  - conflicts between the specs;
  - departures from Firstmate's contracts that have no stated reason;
  - anything a test cannot verify.

  Fix what is clear, and list the rest for CP2.
- **Depends on:** T1.2 to T1.5.
- **Model, effort:** `fable` high (a different session from the author).
- **Estimate:** 1.5-2 h.
- **Acceptance:** The coverage table is complete, and the open issues are listed for the developer.

---

## M2: Core

### T2.1 State core
- **Goal:** Build `crates/core`:
  - schema and migrations in WAL mode, with `busy_timeout`;
  - domain types where every lifecycle state is an enum and each transition is a function that returns `Result`, so an illegal transition cannot compile or silently pass;
  - a repository layer where every mutation runs in a `BEGIN IMMEDIATE` transaction and writes its event row;
  - `rev` checks with readable conflict errors;
  - minting of ULIDs and display ids;
  - reservations.

  Publish the interface first, about 1.5 h in, so other lanes can start. The interface step also creates every crate and module in the workspace layout as stubs and pins the known dependencies (see "Stack").
- **Depends on:** CP2.
- **Model, effort:** `opus` high.
- **Estimate:** 6-8 h.
- **Owns:** `crates/core` (except `backup/` and `import/`), the workspace `Cargo.toml`.
- **Acceptance:**
  - A property test with N concurrent writer processes shows no lost updates.
  - A test that kills a write mid-transaction leaves no half state.
  - Migrations run in both directions on a fixture.

### T2.2 `tiller` CLI
- **Goal:** Implement every command in `protocol.md` on top of the state core, to the AXI standard.
- **Depends on:** the T2.1 interface.
- **Model, effort:** `sonnet` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/cli`.
- **Acceptance:**
  - Every command has an insta snapshot of its JSON output and a test of its exit code.
  - Retrying a command with the same idempotency key has no second effect.
  - Startup stays under 10 ms on the personal Mac.

### T2.3 Backup, restore and export
- **Goal:** Build a nightly `sqlite3 .backup` that stays inside each host's boundary, `tiller restore`, a JSONL export and import, and the schema migration tool.
- **Depends on:** the T2.1 interface.
- **Model, effort:** `opus` medium.
- **Estimate:** 2-3 h.
- **Owns:** `crates/core/src/backup/`.
- **Acceptance:** The restore drill recovers an acknowledged card along with its pending decision.

### T2.4 Importer
- **Goal:** Build `tiller import <bundle>` for `tiller-import/v1`. A dry run prints the full plan. A real run writes to state.db, and turns memory into a placement plan for the developer to approve, never direct writes. Imported memory gets `legacy-import` provenance.
- **Depends on:** T2.1, and the T0.4 bundle.
- **Model, effort:** `opus` medium.
- **Estimate:** 2-3 h.
- **Owns:** `crates/core/src/import/`.
- **Acceptance:**
  - Importing the dry-run bundle reproduces every card count from its manifest.
  - Running the import a second time changes nothing.

### T2.5 Delivery and herdr adapter
- **Goal:** Build the outbox (store first, then send a wake-up line with the message id, then track delivery states), idempotent retries, and a check that the target pane still hosts the expected session. Build the herdr adapter, with one poller per source, no overlapping polls, backoff on failure and last-success timestamps. Apply the herdr layout rules from `docs/design/herdr-layout.md`.
- **Depends on:** the T2.1 interface.
- **Model, effort:** `opus` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/delivery`, `crates/herdr`.
- **Acceptance:** End-to-end tests cover a busy pane, a permission dialog showing, a pane that has gone away, a duplicate send, and a crash between the store and the send.

### T2.6 Observations
- **Goal:** Build `tiller hook <event>` as a fast path that writes observation rows straight to SQLite (no daemon round trip), a transcript tailer, and process liveness checks. Map every hook event exactly as `protocol.md` says. Hook input structs tolerate unknown fields, so a new Claude Code field never breaks a hook.
- **Depends on:** the T2.1 interface.
- **Model, effort:** `opus` medium.
- **Estimate:** 3-4 h.
- **Owns:** `crates/observe`.
- **Acceptance:**
  - A hook call finishes in under 10 ms at p95, including while the daemon is down.
  - Interrupts, `StopFailure`, permission prompts and session end are each detected in tests.

### T2.7 Reconcile engine
- **Goal:** Implement `recovery.md` as a reconcile loop over cards, runs, panes, worktrees and PRs, and derive the board columns.
- **Depends on:** T2.5, T2.6.
- **Model, effort:** `fable` high, or `opus` xhigh.
- **Estimate:** 4-6 h.
- **Owns:** `crates/reconcile`.
- **Acceptance:**
  - Every row of the recovery table has a passing end-to-end test.
  - Conflicting evidence ends up as `needs` or `unknown`, never as a destructive action.

### T2.8 Daemon and service
- **Goal:** Build the daemon process:
  - a single-instance lock;
  - it runs the pollers and the reconcile loop;
  - it serves the board API from `protocol.md` with axum: HTTP for reads and mutations, and one SSE stream for pushes;
  - it serves `index.html` with the boot snapshot inlined (see M3), and the board's hashed assets from the binary with `Cache-Control: immutable`;
  - it exports `contract/openapi.json` through utoipa, and CI fails when that file is stale;
  - it serves a health endpoint, on localhost only.

  Add a home-manager module that installs a launchd agent on macOS and a systemd user unit on Linux, wired into the dotfiles through a PR made with the `dotfiles-install` skill.
- **Depends on:** T2.1. It integrates T2.5 to T2.7 as they land.
- **Model, effort:** `opus` high (it changes how the machine is set up).
- **Estimate:** 2-3 h.
- **Owns:** `crates/daemon`, `contract/`, `nix/module.nix`.
- **Acceptance:**
  - The daemon survives a kill and a reboot on both personal hosts.
  - Its idle CPU and memory are measured and reported.
  - Browsers resume the SSE stream after the daemon restarts, with no reload and no missed event.

**Checkpoint CP3 follows.**

---

## M3: Board

The board is built from the mocks the developer approved at CP-UI (`docs/design/mocks.html`) and the tokens in `docs/design/design-basis.html`. **All projects is the default view.** There are seven screens: Board, Agents, Inbox, Card detail, Memory, Health and the degraded states. There is no wall view; the developer removed it.

The sidebar has:
- the view knob (All projects or Focus) and a visibility toggle per project;
- **Machines**, one row per host: a health dot, running agents against capacity ("3 running, 2 free"), and a quiet "view only" tag on remote hosts that are not paired for acting (ADR 0003 adds a link state for paired peers, task F5). Clicking a row filters Board and Agents to that host and shows a removable chip in the header. A stale host shows "last seen N min ago" in amber, with the reason in its tooltip;
- **a quota footer** in place of any status text: a status dot, a tiny meter, "27% week left" and "resets Tue 14:00". The dot is green on pace, amber when spending ahead of pace, red near exhaustion. The daemon's live or stale state is in the dot's tooltip only. Clicking it opens Health.

**Perfect loads.** The developer wants every load to be perfect: no spinners, no skeletons, no layout shift and no theme flash. Every board task follows these rules:

- **Boot snapshot.** The daemon renders `index.html` with the current dehydrated query state inlined in `<script type="application/json" id="tiller-boot">`, built by the same handlers the API uses. The client hydrates the QueryClient from it before it creates the router, so the first render is complete.
- **No theme flash.** An inline script sets the theme class and background before any CSS loads.
- **Loaders.** Route loaders `await context.queryClient.ensureQueryData(...)`, and components read with `useSuspenseQuery`, as the conventions reference does. Use `defaultPreload: "intent"` and structural sharing. A pending state must never become visible for a local fetch.
- **Live updates.** The SSE stream writes each event into the Query cache with `setQueryData` and ignores any event whose `rev` is older than the cached one. After a gap, the snapshot is refetched silently while the UI stays put, and only the degraded pill shows.
- **Mutations.** Mutations are optimistic and carry `rev`. On a conflict, the change rolls back and a readable conflict message shows.

### T3.1 App shell
- **Goal:** Build:
  - the sidebar with the view knob, project toggles, the Machines section with its filter chip, and the quota footer;
  - file-based routing;
  - theme (following the system by default) and density;
  - the Cmd+K palette;
  - the QueryClient with boot-snapshot hydration, the SSE cache writer, and the query option factories over the generated schema.

  Use fixtures that follow the OpenAPI contract until T2.8 lands.
- **Depends on:** CP2, CP-UI.
- **Model, effort:** `sonnet` high.
- **Estimate:** 2.5-3.5 h.
- **Owns:** `board/src/routes/__root.tsx`, `board/src/components/shell/`, `board/src/lib/`.
- **Acceptance:**
  - Screenshots match the mocks in both themes at 1440 px and 390 px.
  - A performance trace of a cold load shows one complete first render under 100 ms on the personal Mac, a layout shift (CLS) of 0, and no pending or fallback UI.
  - The quota footer's dot turns amber and red from fixtures, and no fixture or response contains an email or account id.

### T3.2 Board views
- **Goal:** Build:
  - the all-projects swimlane board, with the All / Active / Needs you filter in the header on desktop and as a tab row on phones;
  - the Agents view: each orchestrator with its workers nested under it as `├`/`└` rows, the way herdr nests workspaces, showing role, model, status, the last output line and an "Open in herdr" button (key O, tooltip "Focus this agent's terminal in herdr"; for a remote agent it opens herdr on that machine first). A detail panel repeats the explanation, shows the `herdr agent focus <card>-<role>` command, and lists project, card, branch, worktree, workspace label, freshness, state and recent events;
  - the card detail page, with its timeline, runs, PR and head SHA, bound approval, events and freshness.
- **Depends on:** T3.1.
- **Model, effort:** `sonnet` high.
- **Estimate:** 3-4 h.
- **Owns:** the board, agents and card routes under `board/src/routes/`, and `board/src/components/board/`.
- **Acceptance:**
  - With 4 projects and 40 cards, all four lanes fit at 1440x900 without page scroll, in both densities.
  - "Open in herdr" focuses the right pane for a local agent, and switches to the saved machine first for a remote one.
  - Live updates arrive with no reload and no flicker.
  - Moving between views never shows a pending state.

### T3.3 Attention inbox and actions
- **Goal:** Build:
  - the global attention queue, where each item shows the question, evidence, recommendation, consequence and whether it is still current;
  - permission prompts kept separate from decisions;
  - the actions: answer, approve (bound to the head SHA), reprioritize, comment, add card, open in herdr, each with receipts showing recorded, delivered and done;
  - a stale-approval refusal shown in the UI.
- **Depends on:** T3.1, T2.2.
- **Model, effort:** `opus` medium.
- **Estimate:** 3-4 h.
- **Owns:** the attention route under `board/src/routes/`, and `board/src/components/attention/`.
- **Acceptance:**
  - A second click repeats nothing.
  - Approving a SHA that has gone stale is refused with a readable message.

### T3.4 Degraded states
- **Goal:** Show herdr disconnected, stale lanes with the last-updated time, a stale machine in the sidebar with its reason, unknown agent state, a reconnecting banner, and empty projects.
- **Depends on:** T3.2.
- **Model, effort:** `sonnet` medium.
- **Estimate:** 1.5-2 h.
- **Owns:** `board/src/components/states/`.
- **Acceptance:** Each state can be triggered from a test fixture and has a screenshot.

### T3.5 Health page
- **Goal:** Build the Health page from the mocks, with no comparison to Firstmate anywhere:
  - a plain verdict line ("Working. Cards are landing, you were needed less, and nothing was lost.") plus one thing to watch;
  - four signals, each with a plain reading, a status (Good or Watch), small chips and an 8-week trend: Shipping (cards landed), Your time (times you were needed, median time to answer), Quality (rework, reverts, escaped bugs), Safety (lost work, duplicate dispatches, unauthorized actions, machines live);
  - the Claude quota block: "27% of this week left. Resets Tue 14:00. On pace to last until reset.", a meter of used against elapsed, the other windows smaller beside it, and reserve and burn chips. Times in Lisbon time with the weekday;
  - "Show the numbers", collapsed by default, with the weekly charts and the per-project breakdown.

  The signals are simple counts from state.db; T7.1 adds the deeper classification later.
- **Depends on:** T3.1.
- **Model, effort:** `sonnet` high, following the `dataviz` skill.
- **Estimate:** 2-3 h.
- **Owns:** the health route under `board/src/routes/`, and `board/src/components/health/`.
- **Acceptance:**
  - Every number on the page traces to a query over state.db or the quota feed, with a test.
  - No email or account id appears anywhere.
  - Screenshots match the mocks in both themes at 1440 px and 390 px.

### T3.6 UI pass
- **Goal:** Do a pixel review of every screen in both themes at 1440 px and 390 px, check keyboard navigation and focus rings, and check contrast. Fix everything found.
- **Depends on:** T3.2 to T3.5.
- **Model, effort:** `sonnet` high.
- **Estimate:** 1.5-2 h.
- **Acceptance:** Screenshots are attached, nothing is clipped or overflowing, and the console has no errors.

**Checkpoint CP4 follows.**

---

## M4: Agents

### T4.1 Roles
- **Goal:** Write the role files in `roles/`: orchestrator, scout, builder, reviewer, verifier. Follow the role standard in the plan (section 12 of v1, as amended by v2):
  - each role has a narrow job, its tools, its model and effort defaults, its done evidence and its handoff format;
  - the orchestrator never edits project code;
  - a reviewer has no memory tool.

  Each role gets a matching settings fragment with its deny rules.
- **Depends on:** CP2. Drafting can start in wave C.
- **Model, effort:** `opus` medium for the orchestrator, `opus` high for the rest.
- **Estimate:** 4-6 h.
- **Owns:** `roles/`.
- **Acceptance:**
  - Each role is launched through its real launch form, and its effective instructions, tools, model and memory are checked at startup and on resume.

### T4.2 Spawn, capacity and layout
- **Goal:** Build `tiller spawn`:
  - it reserves capacity atomically;
  - it checks quota against each account's floor, with a fixed share per host, and stops dispatch when the quota check fails;
  - it creates a treehouse worktree;
  - it opens a herdr pane following the herdr layout rules (each project's orchestrator and its workers grouped together);
  - it injects the role and brief;
  - it records the run.

  Also build `tiller done`, which refuses while there are unlanded commits.
- **Depends on:** T2.5, T2.7, T4.1.
- **Model, effort:** `opus` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/agents/src/spawn/`.
- **Acceptance:**
  - Two spawns started at once cannot oversubscribe capacity.
  - Cleanup never removes unlanded work.

### T4.3 Guards and landing
- **Goal:** Build the PreToolUse backstop hooks and deny rules from `authority.md`. Build `tiller land`, run only by the daemon, which merges after a bound approval and checks the head SHA again just before merging.
- **Depends on:** T1.4, T2.1.
- **Model, effort:** `opus` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/agents/src/guards/`, `crates/agents/src/land/`.
- **Acceptance:**
  - Every alternate path in `authority.md` has been tested, and each one is either blocked or documented as convention.
  - A push made after approval invalidates that approval.

### T4.4 Intake wiring
- **Goal:** Build the UserPromptSubmit hook that records an ask, `tiller ask resolve`, the Stop hook in observe mode, and the card tags in replies. An ask that cannot be resolved stays visible.
- **Depends on:** T2.6, T1.3.
- **Model, effort:** `opus` medium.
- **Estimate:** 2-3 h.
- **Owns:** `crates/agents/src/intake/`.
- **Acceptance:**
  - Three interleaved asks map to the right cards in a test.
  - A board-created card is not created a second time.

### T4.5 Handoff and resume
- **Goal:** Build `tiller handoff`, which records the active cards, pending decisions, workers, conclusions and next action. The orchestrator signals breakpoints; only the developer clears a session.
- **Depends on:** T2.2.
- **Model, effort:** `sonnet` high.
- **Estimate:** 1.5-2 h.
- **Owns:** `crates/agents/src/handoff/`.
- **Acceptance:** A fresh orchestrator resumes from the handoff record alone and gets three scripted questions about the state right.

### T4.6 Evals
- **Goal:** Write skill-trigger evals (`claude plugin eval --no-publish`), role behavior evals with a holdout set, a no-skill baseline for each skill, and a pinned judge with a pinned rubric. Keep infrastructure tests separate from these evals.
- **Depends on:** T4.1.
- **Model, effort:** `opus` medium.
- **Estimate:** 2-3 h.
- **Owns:** `evals/`.
- **Acceptance:** Every eval runs in CI on demand, and the holdout set is never used for tuning.

**Checkpoint CP5 follows.** It is a supervised live trial on a throwaway sandbox repo, not on a real project.

---

## M5: Replay gate

### T5.1 Replay harness
- **Goal:** Feed the asks in the bundle's `baseline/replay.jsonl` through intake and the orchestrator in sandbox mode. Dispatch, landing and pane sends are stubbed, and every decision is recorded.
- **Depends on:** M4.
- **Model, effort:** `opus` high.
- **Estimate:** 2-3 h.
- **Owns:** `crates/replay`.
- **Acceptance:** It runs 30-50 asks with no side effects, proved by an audit of the sandbox.

### T5.2 Replay run and difference report
- **Goal:** Run the replay. Compare each disposition, kind, model, effort and hold with what Firstmate did. Classify each difference as better, worse or neutral using the pinned rubric, quoting the evidence for each.
- **Depends on:** T5.1.
- **Model, effort:** `opus` high, as the pinned judge.
- **Estimate:** 1.5-2 h.
- **Acceptance:**
  - The report lists every difference, with a summary that states its uncertainty.
  - Every "worse" either has a fix or is accepted by the developer.

**Checkpoint CP6 follows.**

---

## M5F: Federated board

ADR 0003 (accepted Tuesday 2026-11-03) gives the owner one board across hosts: each host keeps its own `state.db` and its own projects; the board host reads every paired peer live over SSH and forwards the owner's mutations to the owning host, which re-checks them. The contracts are `protocol.md` section 10, `authority.md` G41 to G44, `state.md` principle 1 and `recovery.md` R05. These tasks run beside M4's tail and M5, in one or two lanes, and finish before T6.3. F2 to F4 run in order: they share `crates/daemon` and `crates/cli`.

### F1 Land the federation spec
- **Goal:** Record the owner's decision on ADR 0003 and its three choices, and merge the spec edits as reviewed.
- **Depends on:** the owner's approval of ADR 0003.
- **Model, effort:** `sonnet` medium.
- **Estimate:** 0.5 h.
- **Owns:** `docs/adr/0003-federated-board.md`, the amended spec sections.
- **Acceptance:** The ADR says accepted, with the choices; every spec status line names it.

### F2 Pairing and keys
- **Goal:** `tiller peer add|accept|list|remove|revoke`, the `peer_keys` table, `hosts.peer_scope` and `peer_host_id`, the Ed25519 key file, and config `federation.accept`.
- **Depends on:** F1.
- **Model, effort:** `opus` high.
- **Estimate:** 2.5-3.5 h.
- **Owns:** `crates/core/migrations/` (one new migration), `crates/core/src/state/peers.rs`, `crates/cli/src/cmd/peer.rs` (the existing `peer add` moves there from `admin.rs`).
- **Acceptance:**
  - The migration runs both ways on the fixture.
  - Pairing over `ssh -t` needs the owner on both hosts; a session caller is refused on both.
  - A second `peer add` for the same peer revokes the old key; `federation.accept = false` refuses pairing.

### F3 Peer link and federated reads
- **Goal:** `tiller peer serve` and the daemon's link client (`protocol.md` 10.2), and the federated reads, snapshot and SSE additions (7.3, 7.5, 7.6).
- **Depends on:** F2.
- **Model, effort:** `opus` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/daemon/src/peer/` (new), host tagging in `crates/daemon/src/views.rs`, the SSE additions in `crates/daemon/src/sse.rs`, `contract/openapi.json`.
- **Acceptance:**
  - A peer's cards appear within one frame of a change on the peer, and a resume after a gap misses nothing.
  - Link states follow frame age; a link that hangs never blocks a local write or a local SSE push.

### F4 Forwarded mutations
- **Goal:** Signed `mutate` frames, the allowlist on both hosts, the new HTTP codes, `board@<label>` with `via_peer`, the `[peer]` backstop and deny rule.
- **Depends on:** F3.
- **Model, effort:** `opus` high.
- **Estimate:** 3-4 h.
- **Owns:** `crates/daemon/src/peer/forward.rs`, the mutation routing in `crates/daemon/src/api.rs`, the `[peer]` rule in `crates/agents/src/guards/bash.rs`.
- **Acceptance:** Every G41 to G44 test passes (with F3's `peer serve` tests); a mutation for an unreachable peer writes nothing on either host; a retried 504 applies once.

### F5 Board: host chips and staleness
- **Goal:** `protocol.md` 10.5 on the board: host chips, "as of" with the age, disabled controls with the reason, "sent, not confirmed", the project picker grouped by host, the Machines link state.
- **Depends on:** F1; the mocked API until F3 lands.
- **Model, effort:** `sonnet` high.
- **Estimate:** 2.5-3.5 h.
- **Owns:** a host chip and peer-state hooks under `board/src/components/board/`, the Machines row in `board/src/components/shell/`, the stale states in `board/src/components/states/`.
- **Acceptance:** Screenshots match in both themes at 1440 px and 390 px; the perfect-load rules hold with peers in the snapshot; no screen changes for a host with no peers.

### F6 Two-host end to end
- **Goal:** Two homes on one machine, joined through an SSH shim, drive the whole path.
- **Depends on:** F4 and F5.
- **Model, effort:** `sonnet` high.
- **Estimate:** 1.5-2 h.
- **Owns:** `crates/daemon/tests/peer_e2e.rs`, `scripts/two-host-demo.sh`.
- **Acceptance:**
  - A board answer for the peer's card reaches the peer's orchestrator through its outbox.
  - With the peer frozen (`SIGSTOP`), the local board, its writes and its SSE are unaffected, and the peer's cards show "as of".

**Checkpoint CP-F follows.** The developer uses the two-host board from F6.

---

## M6: Cutover

Each host goes through the bundle's `CUTOVER.md`. Rollback triggers go live at the moment of the flip: lost work, an unauthorized action, or a duplicate dispatch.

Before each flip, every kept row in the parity matrix that applies to that host has a passing test, or a live check recorded at CP7. A missing capability blocks the flip.

- **T6.1 Personal Mac.**
  - Steps: freeze Firstmate dispatch; drain in-flight work; run the export with `MODE=final`; import it; flip the owner marker; repoint the dotfiles through a PR; start the services. Delete the T0.5 demo.
  - Model, effort: `opus` high, with the developer present at CP7.
  - Estimate: 1-2 h.
- **T6.2 Watch for 48 hours.**
  - The daemon checks the rollback triggers at no token cost.
  - Once a day, send a summary in the checkpoint shape.
  - Model, effort: `sonnet` medium.
- **T6.3 Remote host.** The same steps as T6.1, after 2-3 stable days on the personal Mac and after CP-F. Then pair it from the personal Mac (`tiller peer add`), confirm the link is `live`, and live-check one forwarded answer and one cut link.
  - Model, effort: `opus` high.
  - Estimate: 1.5-2.5 h.
- **T6.4 Work Mac.** It runs on the work Mac under the work account, by an agent session there. It uses the reduced behavior the probe found, and its data stays on that machine: it sets `federation.accept = false` unless the owner chose otherwise for ADR 0003.
  - Model, effort: `opus` high.
  - Estimate: 1-2 h.

**Fallback:** stop Tiller on that host, flip the owner marker back, and restart Firstmate. Firstmate stays stopped but not deleted until CP8.

---

## M7: After day one

### T7.1 Health depth
- **Goal:** Deepen the Health page (T3.5) without changing its high-level look:
  - classify each time the developer was needed as a correction, an unplanned escalation or rework, so "Your time" and "Quality" count avoidable interventions per accepted outcome;
  - the developer can relabel the classifier's labels from "Show the numbers";
  - each figure under "Show the numbers" states its sample size;
  - the same numbers, computed the same way over Firstmate's history, feed the T7.4 report. They never appear on the Health page.
- **Model, effort:** `sonnet` high, following the `dataviz` skill.
- **Estimate:** 2-3 h.

### T7.2 Memory view
- **Goal:** Build the Memory page from the approved mocks:
  - one readable, editable document per project, with "All projects (shared)" first in the list;
  - plain sections ("How we work here", "Watch out for", "Preferences"), one line of prose per entry;
  - a quiet provenance hint on each line (card id and date). It opens the source file, its approval and which sessions loaded that revision;
  - click a line to edit it in place, add a line, or remove one. Each change is a commit to the real file under git, through the scrub hook;
  - agent proposals show inline in the text, the way suggestion mode works in a shared document (tinted for a new line, struck old text plus tinted new for a rewrite), with Keep and Discard. There is no separate approval queue;
  - a "Recent changes" rail with Undo on each entry. Undo reverts that commit.

  Retiring a memory keeps its history.
- **Model, effort:** `opus` high (it writes to real files).
- **Estimate:** 6-9 h.

### T7.3 Learning loop
- **Goal:** Build the learning loop:
  - sampled retros, with full retros for incidents and surprising wins;
  - evidence-backed proposals for both mistakes and successes;
  - a config hash on every run;
  - one change at a time, or a named bundle;
  - a holdout eval run before any change is approved.

  "Nothing worth saving" is a normal result.
- **Model, effort:** `fable` high for the design, `opus` high for the build.
- **Estimate:** 5-8 h.

### T7.4 The two-week check
- **Goal:** Compare Tiller's health numbers (T7.1) with Firstmate's baseline using the stated tolerances, and report the results with their uncertainty. This report is what CP8 is decided on.
- **Model, effort:** `opus` high.
- **Estimate:** 1 h.
- **Then:** **Checkpoint CP8**, where the developer decides whether to delete Firstmate.

---

## herdr layout

Each project's orchestrator and its workers are grouped together in herdr. The full rules are in `docs/design/herdr-layout.md`. In short:

- **Workspaces, not tabs.** Tiller uses Firstmate-style nested workspaces, as the developer decided. Each project's orchestrator gets its own workspace, labelled with the project id.
- **Worker workspaces.** Each worker gets its own workspace, labelled `├ <card> <role>`, with `└` on the last one. It is moved directly under its project's orchestrator through the socket API's `workspace.move`. Its agent name is `<card>-<role>`.
- **Labels stay clean.** No identity token appears in a label; identity lives in state.db and in herdr pane metadata.
- **Reconcile keeps the layout correct.** It repairs block order and tree glyphs after every spawn, cleanup and reconnect.
- **Row tokens survive restarts.** `$project`, `$card`, `$role` and `$model` are reported again after every herdr reconnect.
- **The Agents panel is sorted by spaces**, so it follows the block order.
- **Safe cleanup.** A worker's pane is closed only after its run has ended and it has no unlanded work. A live pane is never closed.
- **No herdr worktree groups**, because treehouse owns worktrees.

T0.1 also updates herdr from 0.9.3 to 0.9.5 through the dotfiles and adds the herdr config rows. T2.5 and T4.2 implement the layout. If any rule turns out to be wrong, stop and ask.

## Done means

- Every task's acceptance criteria are met, through its normal delivery path.
- Every checkpoint is answered by the developer.
- Every row in the parity matrix is matched and tested, or its drop was approved.
- Every host runs Tiller with Firstmate stopped.
- The two-week check has passed: output and quality are no worse than the baseline within tolerance. Only then can Firstmate be deleted.
