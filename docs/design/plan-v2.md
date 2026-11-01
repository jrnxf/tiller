# Response to the second opinion on `docs/design/plan.md`

Written Sunday 2026-11-01, evening Lisbon time (WET). It answers `docs/design/second-opinion.md` one point at a time. It also applies Revision 1 of the plan (one cutover, not a strangler). The review was written against the original strangler plan, so some of its points no longer apply.

## Bottom line

1. **Most of the review is right.** It found real gaps in the contracts for state, delivery, approval, recovery and memory. 33 of its 43 points change the plan, and 6 confirm it.
2. **I disagree with four points.** We still cut over once per host, with no phase that builds on top of Firstmate. We still replace Firstmate rather than fork it. The orchestrator still never edits project code. And no tasks-axi authority period exists, because no migration period exists. The reasons are below.
3. **The safety net changes.** In place of a live shadow run and a parity gate, the plan has four parts:
   - a capability probe on every host before the build;
   - an offline replay of Firstmate's real history through the new orchestrator before cutover;
   - rollback triggers;
   - Firstmate kept stopped, not deleted, as the fallback.
4. **The estimate goes up.** Day one was 24-36 agent hours and is now 50-70. The review is right that recovery, concurrency, delivery and approval binding were not sized. Firstmate needs about 45 KB of documentation just for its hold lifecycle. That shows how much those contracts weigh.
5. **Good ideas taken further.** Five come from the review, and the rest are new:
   - The throwaway board demo stays up as the board until cutover.
   - Firstmate's fleet ledger is switched on now, so we get real event data before cutover.
   - Firstmate's contract documents become the spec, and its MIT code can be reused with attribution.
   - Session transcripts become a second source of state that does not depend on hooks.
   - The board measures how long you spend on each item that needs your attention, at no token cost.

## Where I disagree, and why

**One cutover per host, not "build on top of Firstmate first"** (points 1, 15, 21, 43). The review's rollout puts durable intake, action delivery, reconciliation and the attention queue on top of Firstmate, then decides later whether to replace it. Two problems with that:

- **The integration gets built twice.** Every one of those features would be written against Firstmate's internals: its markdown backlog, hold encoding, state files and pane conventions. Then it would be rewritten for the new system's state model.
- **You have already decided.** You chose to own the system and said you are not interested in a slow roll-off.

The review's goals still stand, so we reach them another way:

- **Visible value early.** The throwaway demo board keeps running until cutover.
- **Measurement first.** Turn on the fleet ledger now, and build the historical baseline from Firstmate's history.
- **A tested owner before full rollout.** An offline replay gate, rollback triggers, a single-owner marker, and personal hosts cut over before the work Mac.

**Replace, do not fork** (point 2). A fork gives you ownership and Firstmate's tested behavior on day one, which is a real advantage. But what you want changes the centre of the system:

- one orchestrator per project, instead of one captain and one interface;
- a transactional state store, instead of a markdown backlog;
- a structured intake protocol;
- a memory model that requires approval.

A fork would diverge at its core on the first day and still carry everything it ships that you never use. So we take the fork's main advantage without forking:

- Firstmate is MIT-licensed. Its contract documents become the spec: `captain-hold-lifecycle`, `arm-pretool-check`, `agent-control`, `subagent-guard`, `cd-guard`, `supervision-host`, `fleet-ledger`.
- The export's `regressions.md` turns its field incidents into tests.
- Specific scripts can be reused with the copyright notice kept.

| Option | Ownership | Fits per-project and memory approval | Output risk at switch | Upkeep | Exit |
|---|---|---|---|---|---|
| Pinned Firstmate plus config | low | partly (secondmates) | none | low while pinned | easy |
| Fork with a small patch set | high on paper | no; the centre gets rewritten | low at first | high: a large codebase you did not write | hard |
| Owned replacement that reuses Firstmate's specs | full | yes | highest; handled by replay, rollback and fallback | what you build | easy (you own it) |

**No tasks-axi authority period** (point 6). This only applied to the strangler plan. The new system owns its records from day one. The review's general rule stands: each fact has one owner, and everything else is a rebuildable projection.

**The orchestrator never edits project code** (point 33). A small-change path is fine, as a fast, low-effort worker. Letting the orchestrator edit code would mix supervision with implementation, fill the orchestrator's context with code, and weaken the authority boundary in point 17.

## Point by point

Verdicts:
- **Adopt**: take the fix as written.
- **Adapt**: take the goal with a different fix.
- **Hold**: keep the plan as it was.
- **Already**: Revision 1 already covers it.
- **Correct**: the review caught an error in the plan.

| # | Point | Verdict | Change to the plan |
|---|---|---|---|
| 1 | Migrate step by step; keep a fallback | Hold, plus Adopt | Keep one cutover per host. Keep the fallback: Firstmate stopped, not deleted, for 2 weeks. Keep the demo gate. |
| 2 | Alternatives rejected too confidently | Hold, plus Adapt | Replace, but reuse Firstmate's MIT specs and scripts. The scored table is above. |
| 3 | Per-project chat arrives late; phases 0-2 "solve" interleaving | Correct, plus Adapt | Fix the wrong claim: the card protocol is what fixes interleaving. Per-project orchestrators now ship on day one. Optional extra: run one Firstmate secondmate for alpha for 2-3 days during the build, to test whether three conversations really cost less attention than one plus the board (15-20 min to set up). |
| 4 | Measure before changing the workflow | Adopt | Turn on Firstmate's fleet ledger now (`touch config/fleet-ledger`, a flag you can remove). Build the historical baseline in the dry run. Record a config version on every card. Ship the scorecard before the memory editor. |
| 5 | Treat the work Mac as a feasibility gate | Adopt (high) | Phase -1: a capability probe on all three hosts before any build. If managed settings block user hooks, the work Mac reads state from herdr plus transcript tailing instead. That reduced behavior is written down, not discovered later. |
| 6 | One tracker can still have a database | Adopt (tasks-axi part: Hold) | Workflow records live in one SQLite database per host (WAL mode, transactions). Memory stays in its canonical files under git. A fact-ownership table lists which record owns each fact; everything else is a rebuildable projection. |
| 7 | Concurrent writes are unspecified | Adopt (high) | Every mutation goes through `tiller`, in a SQLite transaction. Editable records carry a `rev`, and a stale board edit gets a readable conflict. The card update and its event row commit in the same transaction, so no half-written state. Memory commits are serialized per repo with `flock` and commit only their own paths. |
| 8 | Saved to disk is not the same as durable | Adopt | Every record is classed as authoritative (state.db, memory files), rebuildable (projections, observations) or disposable (logs). Nightly `sqlite3 .backup` stays inside each host's boundary. Schema migrations are versioned. A restore drill is a day-one acceptance test: restore an acknowledged card along with its pending decision. |
| 9 | Work state is mixed up with process state | Adopt (high) | Store the underlying facts: work status, agent activity with `observed_at`, attention, dependencies, PR state. Derive the column from them with an explicit precedence table. Each card type has its own definition of done. Cancelled, failed and abandoned are outcomes with their own badges. |
| 10 | Needs identities and relationships | Adopt | Stable internal ids (ULIDs) behind short display ids. Ask-to-card links are many to many. A card has many runs; each run has a session and maybe PRs. Each project has exactly one owner host, and running a project on several hosts at once is not allowed in v1. |
| 11 | Hooks are observations, not state | Adopt (fact-check below) | Each event type is handled on its own, and "unknown" and "stale" are explicit states. Hooks are combined with process liveness, herdr status and transcript tails. Tests cover interrupt, API failure, permission prompt, background work and session end. |
| 12 | Tags should not carry the protocol | Adopt | `tiller ask resolve <ask> new\|followup:<card>\|chat\|cancelled` records where each ask went. Tags stay for readability. The Stop hook starts in observe mode, and blocking is allowed once only after its false-block rate is measured. An ask that cannot be resolved stays visible as unresolved. |
| 13 | Typing into a pane is not durable delivery | Adopt (high) | Outbox pattern: the message is stored first, and the pane only gets a wake-up line with its id (`tiller inbox 01J...`). Delivery has states: pending, delivered, acked, failed. Retries are idempotent and check the target session id. The hook recognizes wake-up lines, so a board-created card is not created twice. |
| 14 | Recovery needs acceptance criteria | Adopt | A recovery table covers daemon restart, orchestrator crash, worker crash, reboot, remote disconnect, duplicate events and a PR merged outside the system. Each row says whether the system repairs it automatically or asks you. It is seeded from `regressions.md`, and each row has an end-to-end test. |
| 15 | Migration needs one active owner | Adopt (no shadow run) | A per-host owner marker file. Each system refuses to mutate anything while the marker names the other. The fallback procedure flips the marker before restarting Firstmate. The live shadow run is dropped. The offline replay gate is read-only by construction. |
| 16 | Approvals must bind to the exact action | Adopt (high) | A merge approval binds repo, PR, head SHA and action. A memory approval binds the diff and its base revision. Both are rechecked when they run, and a stale approval is refused. Outcomes are recorded as requested, accepted, executing, succeeded, failed or uncertain. Each click carries an idempotency key. This matches Firstmate's mode-matched idempotent hold closing. |
| 17 | Authority guards need a boundary that is enforced | Adopt | Merges run only through the daemon, after a bound approval. Agents never merge. Guards are permission deny rules plus a PreToolUse backstop, and alternate paths are tested (`gh api`, `curl`, `git push`). The plan says plainly what is enforced and what is convention: with one OS user and one `gh` token, full enforcement is not possible, so the guarantee is layered defense plus audit. |
| 18 | Remote control and work-data boundaries | Adopt | Remote lanes are read-only in v1, plus jump-to-pane through herdr's saved machine. Any new control route is a constraint change you would decide on. The work board and work data stay on the work Mac. "Same on both Macs" means the same interaction model. |
| 19 | Captain touches should not be the only metric | Adapt | The scorecard has accepted outcomes per week, avoidable interventions per accepted outcome (broken out into corrections, unplanned escalations and rework), and defects that slip through. Attention time is measured without tokens: the board logs how long each attention item stays open until you answer it. You can relabel the classifier's labels. |
| 20 | Stronger output and quality guardrails | Adopt | Record category, rough size (S, M or L at intake), acceptance criteria, review coverage, finding severity and every attempt. Failed and abandoned attempts stay in the denominator. Later defects link back to the card they came from. Compare like with like. |
| 21 | Ten cards "no worse" is weak | Already, plus Adopt | The parity gate is gone. Rollback triggers fire immediately on lost work, unauthorized actions or duplicate dispatch. The 2-week check uses stated tolerances and reports its uncertainty. |
| 22 | Experiments need attribution | Adopt | A config hash on every run covers roles, skills, policies, memory revision, model and CLI version. Change one thing at a time, or measure a named bundle. Pin the judge and its rubric. Keep a holdout set of eval cases. |
| 23 | The learning loop should also capture successes | Adopt | Proposals for repeatable wins are allowed when backed by evidence and an expected effect. "Nothing worth saving" is a normal result. Full retros only for incidents, surprising wins and a sample of routine cards. |
| 24 | Automatic notes are already applied learning | Adopt (high) | Nothing loads into a fleet role's context until approved. Captured evidence goes into `observations` in state.db, which is never loaded. Fleet roles launch with `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` (documented, see below). Your own interactive sessions keep native auto-memory. Approvals can be batched so the cost stays low. |
| 25 | Memory needs scope and validity, not age decay | Adopt | Each memory gets a scope, evidence references, validity conditions and a review status. Temporary facts expire on their condition, and durable preferences come up for review rather than being deleted. Only independent evidence counts toward promotion. Nothing is promoted across the personal and work boundary. |
| 26 | The preceding message is not the reason for a memory | Adopt | Evidence, rationale, authoring session and approval are separate fields. Imported memories are marked `provenance: legacy-import` and get no invented rationale. The export prompt is updated to match. |
| 27 | Auto-commits are not a full transaction | Adopt | A source map gives each editable memory its canonical file and how a change gets applied. Files managed by dotfiles are edited in the dotfiles repo and rebuilt, never in the Nix store. Edits made outside the system are reconciled with honest provenance. |
| 28 | Rollback is more than `git revert` | Adopt | Rolling back means retiring a memory. The reversal and any conflicts are previewed. The config hash shows which running sessions loaded the revision, so the system can offer to send a correction or restart them. |
| 29 | Transcripts and provenance need a publication policy | Adopt | Raw transcripts and quotes stay in state.db, never in git. Memory commits pass a scrub hook that blocks hostnames, local paths and private project names. Eval runs use `--no-publish` explicitly. The import bundle is marked local-only. |
| 30 | One role file does not behave the same in every launch form | Adopt | Test every launch form you use. Workers launch as normal main sessions with `--append-system-prompt-file <role>.md`, never through `--agent`, which replaces the default prompt (fact-checked below). |
| 31 | Forked skills and reviewer memory need care | Adopt | Skills that depend on context run inline or take an explicit brief. Read-only reviewers get approved memory as input and have no memory tool. Tool access is tested, not trusted from the role's prose. |
| 32 | Test native memory through the real launcher | Adopt | Part of the capability probe: launch a real worker through herdr, write a note, restart it, and check the note persists in the right scope. Repeat on each host. |
| 33 | Thresholds are starting heuristics | Adapt | Keep them as defaults you can tune, and measure their effect. A small-change path means a fast worker, not the orchestrator editing code. Treehouse alone creates and cleans up worktrees; native `isolation: worktree` is not used for workers. |
| 34 | A context reset needs a record to resume from | Adopt | `tiller handoff` writes the active cards, pending decisions, workers, conclusions and next action. Most of it is already in state.db. A fresh orchestrator must be able to resume from it in a test. The agent signals the breakpoint, and only you clear. |
| 35 | Capacity and quota need reservations | Adopt | Capacity is reserved atomically in state.db when a worker spawns, and released on reconcile. Quota floors are per account, with a fixed share for each host on the same account. Room is kept for review and recovery. Stale quota data is shown, and when the quota check fails, dispatch stops (fail closed). |
| 36 | The attention queue should do more than show status | Adopt (high) | Each attention item shows the question, the evidence, a recommendation, the consequence and whether it is still current. Permission prompts are kept separate from decisions. Board actions show receipts: recorded, delivered, done. A global attention view sits beside the project lanes. |
| 37 | Keep herdr | Agreed, plus refinements | herdr sits behind an adapter with a single poller and backoff, and the board shows when each source last updated. No latency promise until it is measured under load. |
| 38 | Scripts do the mechanics, no model calls for rendering | Agreed, plus refinements | `tiller` follows the AXI standard: structured output, stable ids, exit codes, safe retries. Daemon load is measured. The board keeps rendering when models are down. |
| 39 | Narrow roles and observable evidence | Agreed, plus refinements | Infrastructure tests are kept apart from behavior evals. Each skill gets a baseline run without it. A maintenance edit does not need a made-up failing case. |
| 40 | Memory earns its place | Agreed, plus refinements | Evaluate the context each role actually receives, including imports and skills. |
| 41 | Language and estimate are premature | Adapt | **Decided by the developer:** Rust for the core, and a TypeScript board (TanStack Router, no SSR). The board and core share one schema through a generated OpenAPI contract. The build plan carries the estimate. |
| 42 | Label facts versus judgment | Adopt | Claims are tagged tested, documented, inferred or unverified, with version and environment. The "fine for a private tool" assurance about Linear is dropped. The visual direction stays. Adopted sources are pinned with their licenses. |
| 43 | Suggested rollout | Adapt | Steps 1-2 are adopted as written. Steps 3-5 are replaced by building the owned system with these contracts, a replay gate and one cutover per host. The revised sequence is below. |

## The revised plan (v2)

| Step | What | Agent wall-clock |
|---|---|---|
| -1 | **Capability probe** on the personal Mac (host A), the remote Linux host (host B) and the work Mac (host C). It checks: whether user, project and plugin hooks fire under managed settings; launchd or systemd user services; binding a localhost port; how auto-memory loads through a herdr-launched worker; whether `claude plugin eval` is available; and herdr CLI and snapshot access. It writes a report and changes nothing. You run it on the work Mac. | 45-60 min to write, about 10 min per host |
| 0 | **Measure now.** Turn on the fleet ledger. Run the export prompt with `MODE=dry`, which produces the historical baseline. | 5 min, plus 30-60 min |
| 1 | **Throwaway board demo**, then leave it running as the board until cutover. Optional: the alpha secondmate test. | 60-75 min |
| 2 | **Day-one build** (below), run through the normal delivery path with review | 50-70 h |
| 3 | **Replay gate.** Feed about 30-50 historical asks from the bundle through the new intake and orchestrator in a sandbox. Compare dispositions, kinds and holds with what Firstmate did. You review the differences. | inside step 2 |
| 4 | **Cut over each host.** Personal Mac (host A) first, then the remote Linux host (host B), then the work Mac (host C). Freeze dispatch, drain, `MODE=final` export, import, flip the owner marker, repoint dotfiles. Rollback triggers are live. | 1-2 h per host |
| 5 | **After cutover:** the scorecard and a manual proposal flow (3-4 h), then the memory view with the source map (6-9 h), then the learning loop with config hashes and holdout evals (5-8 h) | 14-21 h |
| 6 | **The 2-week check** against the historical baseline, with stated tolerances. Firstmate is deleted only after it passes. | - |

**Day one in detail (50-70 h):**

- **State core** (10-14 h): the SQLite schema covering ids, relationships, lifecycle facts, revisions, outbox, bound approvals, reservations, and events written in the same transaction; the `tiller` CLI to the AXI standard; backup and restore.
- **Daemon** (8-12 h): the herdr adapter, hook intake, transcript tailing, the reconcile engine that implements the recovery table, and the push to the board.
- **Board** (6-9 h): the Linear-style UI with project lanes, the global attention queue, actions with receipts, and freshness indicators.
- **Intake protocol** (3-5 h): ask disposition, tags, the Stop hook in observe mode, and wake-up lines.
- **Roles and guards** (12-18 h): orchestrator, scout, builder, reviewer and verifier, with every launch form tested; deny rules plus the PreToolUse backstop; merges only through the daemon; quota and capacity reservations.
- **Importer and replay gate** (5-8 h).
- **Acceptance** (4-6 h): end-to-end recovery tests, the restore drill, and the regression cases from the bundle.

The total build is about 65-95 agent hours. These are agent hours, not calendar days; review rounds and your decisions set the calendar time.

## Fact-ownership table (day one)

| Fact | Owner (authoritative) | Projections |
|---|---|---|
| Asks, cards, runs, links, dispositions | state.db | the board, `tiller` output, JSONL export |
| Approvals and their outcomes | state.db (bound to SHA or diff revision) | board receipts |
| Agent activity | none; it is observed. Latest observation plus `observed_at` in state.db | board column (derived) |
| PR state | GitHub; cached with a fetch time | board column (derived) |
| Panes and sessions | herdr; cached with a fetch time | jump links |
| Worktrees | treehouse plus git | reconcile input |
| Approved memory and rules | canonical files (CLAUDE.md, AGENTS.md, memory dirs, the dotfiles source), under git | the memory view |
| Unapproved evidence and proposals | state.db (`observations`, `proposals`); never loaded | the proposal queue |
| Raw transcripts | Claude Code's own JSONL files; read only, never committed | baseline, retros |

## Fact checks on the review's Claude Code claims

All nine claims were checked against code.claude.com/docs on Sunday 2026-11-01. All hold. Each one changes the design as follows:

| Claim | Docs | Design consequence |
|---|---|---|
| Managed settings can block user, project, local and plugin hooks | `allowManagedHooksOnly` blocks them. Exception: plugins force-enabled in the managed `enabledPlugins` (settings-reference) | The probe reads the work Mac's managed settings. If the flag is set, the work Mac's state comes from herdr plus transcript tailing, and the reduced behavior is written down. Asking IT to force-enable a plugin is the only way to get hooks there. |
| `Stop` skips user interrupts; API errors fire `StopFailure` | hooks, "Stop" | Interrupts are detected from transcripts and herdr. `StopFailure` maps to `activity: errored`. |
| `Notification` carries `notification_type` (13 types) | hooks, "Notification input" | Each type is mapped separately: `permission_prompt` and `elicitation_*` to attention, `agent_needs_input` to attention, `idle_prompt` to idle, `quota_auto_resume_*` to quota state, `auth_success` ignored. |
| Auto-memory toggles and their limits | `autoMemoryEnabled`, `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, `autoMemoryDirectory` (memory). Sessions started by another session cannot turn it back on | Fleet roles launch with `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`. Approved memory reaches them through CLAUDE.md and imports, which load either way. This is deterministic and also settles point 24. |
| `--agent` replaces the default system prompt | sub-agents, "Run the whole session as a subagent" | Workers launch as `claude --append-system-prompt-file <role>.md`, which keeps the default prompt (cli-reference). `--agent` is used for nothing. |
| Forked skills get no conversation history | skills, "Run skills in a subagent" | As planned in point 31. |
| Subagent `memory` turns on Read, Write and Edit | sub-agents, "Enable persistent memory". It has no effect when auto memory is off | Reviewers get no `memory` field, and with auto memory off it would do nothing anyway. |
| `claude plugin eval` publishes reports | It publishes a private artifact when signed in with a subscription. Runs started by a session stay local unless `--publish-report` (plugin-evals) | Eval scripts pass `--no-publish` explicitly, so the behavior does not depend on how they were started. |
| `--append-system-prompt-file` exists | cli-reference | It is the role injection mechanism. |

## Open decisions for you

1. Turn on Firstmate's fleet ledger now (`touch config/fleet-ledger` in the Firstmate home): yes or no. Recommended: yes.
2. Run the alpha secondmate test during the build: yes or no. Recommended: yes. It is cheap and tests the one UX premise nobody has tried yet.
3. Language: **decided** (Sunday 2026-11-01). Rust for the core; TypeScript for the board (TanStack Router single-page app, embedded in the binary, no SSR). See `docs/plan/build-plan.md`, under "Stack".
4. Remote lanes: read-only in v1 (now recommended), or give them a new control route as a constraint change.
5. Name: Tiller, CLI `tiller` (chosen).
