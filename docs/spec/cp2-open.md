# CP2: open items from the spec review

This is the output of T1.6, a fresh-context review of the five specs (`state.md` with `schema.sql`, `recovery.md`, `protocol.md`, `authority.md`, `memory.md`) against the second opinion's 43 points, the parity matrix (every kept row, including D01, D04 and D11) and Firstmate's contract documents. Clear fixes went straight into the specs (section 2). Everything that needs the developer is in section 1, one numbered item each, for the CP2 review.

Written Monday 2026-11-02, Lisbon time (WET). Task T1.6.

How to read it: each item names the specs it touches, what conflicts or is missing, and a recommendation. "Carried" items are the per-spec "Open for CP2" lists, gathered here so CP2 had one list.

Status: CP2 done Monday 2026-11-02, Lisbon time. The owner answered every item on a review page in two rounds: round 1 (H01, C01 to C28, V01 to V13) and round 2 (S01 to S09, revising some round 1 picks). Each item below ends with its decision, and each spec's "Decided at CP2" section replaces its "Open for CP2" list. This file is kept as the record.

## Still needs the owner

None of these blocks Wave C.

1. **Which private remote the memory repo pushes to** (`memory.remote`, C24): chosen at cutover, per host.
2. **Host C's probe** (H01): the owner approved the minimum common behavior in `docs/design/host-capabilities.md` as the baseline, but host C has not run the probe yet. Any fallback it turns out to need is a separate decision.
3. **C12 reopened: one federated board** (ADR 0003, accepted Tuesday 2026-11-03): **Decided:** the owner approved the federated board with its three recommended choices: the personal Mac is the one board host and every other host its peer; host C (the work Mac) stays out with `federation.accept = false` and keeps its own Tiller; a mutation for an unreachable peer is refused at once and nothing is queued. It blocks only tasks F1 to F6 and T6.3 (`docs/plan/checklist.md`, Federation).

## H01: minimum common behavior across hosts

**Decided:** approved as the baseline (H01; S05 adds the per-route table in `authority.md` section 7). Host C must still run the probe; fallbacks are decided separately.

## 1. Decisions for CP2

Each item ends with the owner's decision. The id in brackets is the CP2 card: C01 to C28 match items 1 to 28, and round 2 cards S01 to S09 revise some of them; where they conflict, round 2 wins.

### Conflicts between specs

1. **Worker edits to project memory files.** `memory.md` section 9 says a fleet role cannot write a source-map path (`AGENTS.md`, `CLAUDE.md`, `.claude/rules/*.md`) with Edit or Write, labels it enforced by deny rules and names the test `agents::guards::deny_memory_paths_edit`, but `authority.md` section 8 carries no such rule and allows a builder `Edit(<worktree>/**)`. `memory.md` section 7 also expects worker PRs that touch those paths (the "memory change" flag on the merge approval). Recommendation: let builders edit project memory files in their worktree through the normal PR path, since the launch check, the memory-change flag and reconcile already catch any drift; replace the two Edit and Bash guarantees in `memory.md` section 9 with "a fleet role cannot change what loads except through a PR the owner accepted as a memory change", and keep `.claude/**` denied as `authority.md` has it. The alternative is to add the deny rules to `authority.md` and forbid workers from touching project memory at all, which blocks ordinary "fix the wrong line in `AGENTS.md`" cards. **Decided:** Allow edits through a normal PR (C01), amended by S04: builders may edit `AGENTS.md` and `CLAUDE.md` in their worktree, and under the new approved-snapshot rule no memory file loads natively, so an edit loads nowhere until the owner accepts the PR as a memory change; `.claude/rules/*.md` stays denied with the rest of `.claude/**` and changes only through `tiller mem propose`. In `memory.md` sections 1, 7 and 9 and `authority.md` section 8.
2. **Done for `direct-push` projects.** `state.md` section 7 says a `ship` card on a `direct-push` project is done only when the deploy build was watched to a result, but `approval.finish` (section 8) closes the card `landed` the moment the `land_direct` push succeeds, and no spec names the source that watches the deploy (P11's "deploy build is watched"). Recommendation: `approval.finish` ends the run `landed` but, for `direct-push` projects with a configured deploy workflow, opens an `event` obligation ("deploy of <sha> finished") instead of closing the card; the forge poller (T2.5) reads the default branch's workflow runs through `gh-axi run list`, writes a `deploy.finished` event, and `obligation.meet` closes the card; a failed deploy opens a `recovery` item. Needs a line in `protocol.md` section 2 of the pollers and a `deploy_workflow` field on `projects`. **Decided:** Watch the deploy, then close (C02), amended by S07: the watch matches the pushed commit and the project's environment, and missing, cancelled, superseded and timed-out deploys are defined; `projects.deploy_environment` and `deploy_timeout_minutes` sit next to `deploy_workflow`. In `state.md` section 7 (`deploy.record`), `protocol.md` 4.6 and `schema.sql` (`card_column` shows the card in Landing as `deploying`).
3. **The owner's own Claude session is never the owner principal.** `authority.md` section 3 treats any shell with `CLAUDECODE` set as a non-owner, so `! tiller approve <id>` typed into the developer's own interactive session is refused (exit 6), and the intended path is the chat route (`tiller approve --ask`, G06). `recovery.md` R17 hands the developer "one runnable command (`! <command>` in their own session)"; that works only for commands that are not owner-only `tiller` commands. Recommendation: confirm this is acceptable (the board and a plain terminal remain the owner surfaces), and reword R17 to say the command is never an owner-only `tiller` command. The alternative is an owner token file under `~/.tiller` that an interactive session may present; it weakens G22. **Decided:** Accepted, R17 reworded (C03), amended by S05: the owner principal also needs no Claude Code process among its ancestors, and `authority.md` section 7 now has the per-route table (shell, file tools, hooks, MCP, daemon). In `authority.md` sections 3 and 7 and `recovery.md` R17.

### Gaps: kept capabilities with no spec home

4. **P02, the model and effort rules.** No spec defines the four dispatch rules (haiku low, opus high, opus medium, default sonnet medium) or where they live; `authority.md` G28 only says spawn refuses a downgrade without the owner's permission. Recommendation: a `[dispatch]` table in `~/.tiller/config.toml`, rules keyed by card kind and size with an owner-only override per card (`tiller spawn --model --effort` by the orchestrator is refused below the rule, allowed above it), resolved inside `run.start` and written into the `run.started` payload. One paragraph in `protocol.md` section 6.4 before T4.2 starts. **Decided:** A `[dispatch]` table in config (C04): rules by card kind and size, resolved in `run.start` and written into `run.started`; the orchestrator may only raise them; the owner overrides one card with `cards.dispatch_override`. In `protocol.md` 6.4 and `state.md` Card. T4.2 enters Firstmate's four rules as the defaults.
5. **P47, review-page answers reaching the owning worker.** The parity matrix gives T2.5 "a poller source for page answers" with the test `delivery::sources::page_answer_reaches_owning_worker`, but no spec names how an answer on a live review page becomes an outbox message. Recommendation: a T2.5 poller over the page tool's feedback queue that runs `outbox.enqueue` (`steer`, actor `script`) to the run that published the artifact (`artifacts.run_id`), keyed by the feedback entry id so a re-poll never double-delivers. Decide whether this is v1 or whether the hosting worker keeps polling the page itself, as today. **Decided:** In v1 (C05): the T2.5 poller steers the run that published the artifact, keyed by the feedback entry. The actor is `daemon`, not `script` as recommended, because the role matrix gives the daemon the steer right and gives scripts none. In `protocol.md` 3.5.
6. **The "aged" hold bucket.** Firstmate's hold lifecycle surfaces an undated hold older than 14 days as `aged`, in a different place from live calls. `state.md` replaces the snapshot buckets with the derived column plus age, so an old undated question sits in Needs you sorted last. Recommendation: keep it as is; the Needs you sort by age and the attention time measurement (point 19) cover the same need. Confirm. **Decided:** Kept as is (C06): no separate aged bucket; Needs you sorts by age.

### Untestable or under-specified

7. **`StopFailure` retry policy.** `protocol.md` 4.2 says "retry policy is T1.2's"; `recovery.md` R12 defines only the quota case (`quota_auto_resume_disabled` is never retried) and "three `StopFailure` in ten minutes make the note urgent, nothing ends the run". Recommendation: state it plainly in R12 as the policy: the daemon never retries or resumes a session for an API error (Claude Code's own retry is the only one); a session that stays `errored` for 30 minutes is treated as silent (R12's ladder). Confirm. **Decided:** Never retry (C07): the daemon never retries or resumes a session for an API error, and 30 minutes `errored` is treated as silent. In `recovery.md` R12 and `protocol.md` 4.2.
8. **The owner's forge login.** R07 asks the owner to confirm every merge made outside Tiller because no record holds the owner's login (recovery.md, carried item 1 below). If the config records it (`forge_login`), a merge by that login could close the card `landed` without an item. Recommendation: record it in config, keep asking in v1 (the login is also what a worker's `gh` token merges as, so it does not distinguish the owner's hand from a worker's script), and revisit with authority item 15. **Decided:** Record it in config, keep asking in v1 (C08): `forge_login` is recorded and shown on the R07 item, which still asks. In `recovery.md` R07.

### Carried from the specs' own "Open for CP2" lists

`state.md`:

9. Whether `question` is a card kind or every question is an attention item on an `ops` card. The draft keeps the kind so a bare question has a display id. **Decided:** Kept as a card kind (C09). In `state.md` Card.
10. The staleness thresholds per role and the idle re-ring interval (now proposed in `recovery.md`: 15 and 30 minutes; 2, 5, 15, 30, 60 minutes; see item 23). **Decided:** Accepted as starting values (C10). In `recovery.md` section 2.
11. Whether `hosts.lane_capacity` and the floors live in `state.db` or in the config file. The draft keeps them in the database so a spawn event can quote the value it checked. **Decided:** In the database (C11), with the reason corrected by S09: the check runs inside `run.start`'s transaction, and the view that reads the same rows is named `capacity` in `schema.sql` (the round 2 card called it `host_free`). In `state.md` Host.

`protocol.md`:

12. Remote homes (D01): whether `tiller ask note --host <peer>` becomes a control route, which would change the read-only default for remote lanes (point 18). **Decided:** Stay read-only for v1 (C12). In `protocol.md` section 10. **Reopened** Tuesday 2026-11-03, Lisbon time: the owner asked for one board across hosts that can answer, steer and act on any card. ADR 0003 (accepted Tuesday 2026-11-03) keeps one owner host per project and one `state.db` per host, reads peers live over SSH, and forwards the owner's board mutations, signed and allowlisted, to the owning host, which re-checks them. The fleet route (`tiller ask note --host`) stays off. In `protocol.md` section 10, `authority.md` G41 to G44 and `state.md` principle 1.
13. Other harnesses (D04): which harness gets an adapter first, and whether any is needed before cutover. `authority.md` G35 refuses a role on a harness without a pre-tool hook transport and a sandbox equivalent. **Decided:** None before cutover (C13). In `protocol.md` section 10.
14. The Stop hook switching rule: 200 orchestrator stops over 7 days with a false-block rate under 2% are proposed starting values. **Decided:** Accepted (C14), and S06 kept: `protocol.md` 2.5 already defines the false-block test, its denominator and the owner-only switch.
15. How long the owner cookie lives before `tiller board` must be run again (the draft keeps it across daemon restarts). **Decided:** Survives restarts, expires after 30 days (C15). In `protocol.md` 7.1 and `authority.md` section 7.
16. Default TOON output for agents with `--json` for scripts and tests, versus JSON everywhere. **Decided:** TOON default, `--json` on request (C16). In `protocol.md` 6.1.

`authority.md`:

17. Workers in `dontAsk`: an action outside a worker's allow rules is denied, never left waiting, so the permission-prompt section of the board serves only the orchestrator. The alternative is the default mode, which lets a worker stall on a dialog nobody watches. **Decided:** Keep `dontAsk` for workers (C17). In `authority.md` section 7.
18. Making G10 enforced: drop the forge API host from the sandbox allow list for workers on `no-mistakes` projects, or give workers a separate GitHub identity with a branch ruleset only the daemon's identity bypasses (changes GitHub settings). The draft keeps both off and relies on detection. **Decided:** Enforced (C18 picked detection only; S03 overrides it): one GitHub App owned by the owner's account (for example `tiller-merge`), installed on each gated repo as the only ruleset bypass actor, with a private key only the daemon can read; workers merge nothing. Detection-only stays as the explicitly labelled fallback (`projects.merge_gate = detect`) until the App is installed for a repo. Creating and installing the App is a one-time owner setup step in the checklist's Cutover section. In `authority.md` section 6, "Enforced merge gate", and G10.
19. The sandbox on the remote Linux host: if bubblewrap, socat and the seccomp filter cannot start there, G13, G17, G20, G24 and half of G29 run as convention on that host. Decide whether that is acceptable for its cutover. **Decided:** Do not cut that host over until the sandbox works (C19). In `authority.md` section 1.
20. `tiller` as the only command excluded from the sandbox, versus fleet roles reaching the daemon over a socket (keeps every agent process inside the sandbox, makes status reporting depend on the daemon). **Decided:** Keep `tiller` as the one exception (C20), amended by S05: the per-route table names what the exempt `tiller` can do and how the daemon identifies its caller, and T4.3 must prove two conditions before relying on it; a failure is a stop-and-ask. In `authority.md` section 7, "Routes and their controls".

`memory.md`:

21. The launch check refuses on any unapproved byte, so the owner's own hand edit to the dotfiles holds fleet launches until accepted with one click. The alternative, trusting commits signed by the owner's key, is rejected by the draft because fleet roles commit as the owner too. **Decided:** Keep the one-click accept (C21), amended by S04: under the approved-snapshot rule the owner's edit no longer holds launches; launches render the previous approved revision until the edit is accepted. In `memory.md` section 9.
22. A memory change to a PR-delivered project needs the memory approval and then the project's merge approval; a grant that lands an applied memory PR would make it one click. **Decided:** One combined action (C22 picked a one-click grant; S01 reinterprets it): "Approve memory change and merge" records the merge approval and the memory approval against the exact reviewed revision. A new commit makes both stale, required checks still apply, and there is no standing memory grant. In `memory.md` section 7, `state.md` Approval and `schema.sql` (`approvals.memory_paths`, `linked_approval_id`).
23. The default review interval for durable entries (180 days) and whether it differs by class. **Decided:** Different by class (C23), refined by S02, values decided by the owner: `rule` 180 days, `preference` 365 days, `note` 90 days, overridable per class in config; review is also triggered by contradicting evidence or a project change; an overdue entry is only flagged, never erased, and never relaxes a safety rule; checkable facts are queried, not stored. In `memory.md` section 5.
24. Whether a personal host's memory repo pushes to a private remote, and which. **Decided:** Yes, a private remote (C24): `memory.remote`. In `memory.md`. Which remote is chosen at cutover (top of this file).

`recovery.md`:

25. Auto-close on the owner's own merge (see item 8). **Decided:** Keep asking in v1 (C25). In `recovery.md` R07.
26. Thresholds: the staleness, re-ring and escalation values are starting values (point 33), tunable in config, with T7.4 reporting how often each escalation fired. **Decided:** Confirmed; tune after T7.4 counts (C26). In `recovery.md` section 2.
27. Worker relaunch after a crash: every relaunch stays the orchestrator's decision (as `agent-control` and `authority.md` have it); an automatic resume when the worktree holds work would remove a 30-minute wait at the cost of quota nobody decided to spend. **Decided:** Always the orchestrator (C27). In `recovery.md` R03.
28. A `scrub_state` cache column on `prs` (with `merged_by`) if T2.7 finds re-deriving the scrub result from `pr.changed` payloads awkward. **Decided:** Only if T2.7 needs it (C28). In `recovery.md` R19.

## 2. Fixes applied by T1.6 (veto at CP2)

Each was a clear conflict or an unstated departure. They are in the specs now; CP2 could reverse any of them. The owner kept all thirteen (V01 to V13); S04 reworded V09, and S08 confirmed V01 does not conflict with C07 or C27.

1. **The daemon's resume right** (`authority.md` role matrix, `recovery.md` R02 and R04, `protocol.md` `tiller agent resume`). The matrix had no right for the daemon to bring a session back after a crash or a reboot, which R02, R04 and P25 need. Added one narrow row: resume with the same Claude session id, in the same worktree, under the floor checks, never a new attempt, never after a worker crash. `tiller agent resume` now names its mutation (`session.resume`) and the `session.resume_attempted` event the rate limit counts. **CP2:** kept (V01).
2. **Commands `recovery.md` relied on that `protocol.md` lacked:** `tiller migrate up|down`, `tiller owner set tiller|firstmate` (the `tiller owner` namespace defined nothing), the orchestrator form `tiller run pause <run> --until`, and `tiller done <run>` so the orchestrator can end a crashed worker's run (R03). **CP2:** kept (V02).
3. **`tiller done` default outcomes and the builder rule** (`protocol.md` 6.4, `state.md` `run.end`). Without `--outcome`, scout, reviewer and verifier runs end `reported`; a builder's run ends `landed` only through `approval.finish`, so a builder's `tiller done` with an unmerged PR is refused unless it gives `failed` or `abandoned`. R13 said this; the command and the mutation did not. **CP2:** kept (V03).
4. **One live session per run** (`schema.sql` index `sessions_live_run`, `state.md` invariant 5 and `run.relaunch`, `recovery.md` R10). `recovery.md` section 2 relied on a "one-session-per-run rule" that no index held. `run.relaunch` now requires the old session to be proven stopped (the ladder's *dead* verdict or a confirmed exit, as Firstmate's `agent-control` does) and ends its row in the same transaction; a restore rebinds the run's existing session row instead of inserting a second. **CP2:** kept (V04).
5. **Empty composer before typing** (`protocol.md` 3.3, `recovery.md` R09). Firstmate's herdr backend types only into a composer verified empty and never uses Ctrl+C; no spec said so, and a wake-up line typed into the owner's half-written draft would corrupt both. Delivery now reads the composer, types only on `empty`, defers on text or an unreadable read, never clears it, and escalates after 10 minutes like a missed doorbell. **CP2:** kept (V05).
6. **Notifications (P36)** (`protocol.md` 3.4). The parity row had an owner but no spec text. Added the rule: one notification per new attention item, per prompt waiting 30 minutes, per reboot restore and per read-only episode; never for routine turns; recorded in the causing event so a restart cannot repeat it. **CP2:** kept (V06).
7. **Fresh clones (P05)** (`state.md` Run). `base_sha` is now defined as the remote default branch head after a fetch made by the spawn, so every run starts from current origin. **CP2:** kept (V07).
8. **The `card_column` view** (`schema.sql`, `state.md` section 6). The run badge took `min()` over the activity text, so `errored` beat `waiting_permission` and `idle` beat `working` alphabetically whenever a run had two session rows; it now ranks per session and compares ranks. The `paused` badge was unreachable for an idle paused worker (idle ranked above paused); paused now ranks above idle, and section 6 says so. Both views still load (`python3` with `sqlite3`, 35 tables, every view queried). **CP2:** kept (V08).
9. **`mem.load` and the hook budget** (`memory.md` sections 7 and 12, `protocol.md` 4.2). `memory.md` had the `SessionStart` hook writing `session_loads`, which means hashing every loadable file inside a hook with a 10 ms budget. The daemon now does it on the `SessionStart` observation; the hook records only the trigger. **CP2:** kept (V09), reworded by S04: a `session_loads` row proves which approved revision Tiller rendered into the session's prompt at launch, not a later hash of files on disk (`memory.md` section 7).
10. **The launch check's attention item** (`memory.md` section 9) pointed at "the project's `ops` card", which does not exist; it now opens on the card the run is for, or `fleet-1` for an orchestrator launch. **CP2:** kept (V10).
11. **`tiller agent interrupt` reports its result** (`protocol.md` 6.4): `cancel: confirmed` only when the tailer or a hook shows the turn ended within 10 s, else `unconfirmed`, as `agent-control` requires; busy state is never rewritten as proof. **CP2:** kept (V11).
12. **Two Firstmate departures that were real but unstated**, now in `authority.md`'s contract table: `mcp__` tool names are classified (Firstmate never classifies them), and a malformed command containing `cd` fails closed for the orchestrator (Firstmate's cd guard fails open). **CP2:** kept (V12).
13. **Small consistency fixes:** `authority.md` section 6 named `tiller land <card>`, the command takes an approval; the board path example lacked `/api/v1`; `protocol.md` section 10 and `parity.md` now both record the CP1 drop decisions (D01, D04, D11 kept, the other twelve dropped, Monday 2026-11-02); `recovery.md` no longer lists the daemon resume right as open. **CP2:** kept (V13).

## 3. Coverage

### The second opinion's 43 points

Every point has a home. The plan-level points (1 to 4, 39, 41, 43) are decided in `docs/design/plan-v2.md` and `docs/plan/build-plan.md` and need no spec text.

| Point | Spec home |
|---|---|
| 1 | plan v2 (one cutover per host, Firstmate kept stopped for two weeks, the demo gate); build plan M6 and CP8 |
| 2 | plan v2 and ADR 0001 (replace, reuse Firstmate's MIT specs); the contract tables at the end of every spec |
| 3 | plan v2 (per-project orchestrators on day one); `state.md` (one live orchestrator session per project) |
| 4 | build plan T0.4 (the historical baseline, the ledger); `state.md` and `memory.md` (`config_hash` on every session) |
| 5 | `docs/design/host-capabilities.md`; `protocol.md` 2.2 and 4.5 (`intake_source = transcript`, herdr and liveness sources on a host where hooks are blocked) |
| 6 | `state.md` sections 1 and 9 (one authority per host, the fact-ownership table) |
| 7 | `state.md` sections 1 and 8 (`BEGIN IMMEDIATE`, `rev`, readable conflicts); `protocol.md` 6.2 and 7.2 (exit 4, 409 with `current`); `memory.md` section 7 (per-repo `flock`, temporary index) |
| 8 | `state.md` section 10; `recovery.md` R10 and R11 |
| 9 | `state.md` sections 5 to 7 |
| 10 | `state.md` sections 2 to 4 |
| 11 | `state.md` Observation; `protocol.md` sections 4 and 5; `recovery.md` section 2 |
| 12 | `state.md` Ask; `protocol.md` 2.4 and 2.5 |
| 13 | `state.md` Outbox; `protocol.md` section 3; `recovery.md` R06 and R09 |
| 14 | `recovery.md` (the table, section 6, section 8) |
| 15 | `state.md` Host (the owner marker); `authority.md` G26; `recovery.md` R20 |
| 16 | `state.md` Approval; `authority.md` section 6 and G02; `recovery.md` R08; `memory.md` section 7 |
| 17 | `authority.md` sections 1, 5, 9, 10 and 11 |
| 18 | `authority.md` G25 and section 4; `protocol.md` sections 9 and 10; `memory.md` section 8 (`meta.boundary`) |
| 19 | `state.md` Attention (`opened_at`, `answered_at` give attention time at no token cost); build plan T3.5 and T7.4 (the scorecard) |
| 20 | `state.md` Card (`category`, `size`, `acceptance`, `fixes_card_id`), Run (every attempt), Review |
| 21 | `recovery.md` section 8 (rollback triggers as detections); build plan T7.4 (tolerances and uncertainty) |
| 22 | `state.md` Session and Run (`config_hash`); `memory.md` section 7 (the memory part of the hash); build plan T4.6 (pinned judge, holdout cases) |
| 23 | `memory.md` section 6 (proposals for successes, `expected_effect`, "nothing worth saving") |
| 24 | `memory.md` sections 1, 2 and 9; `state.md` Observation; `authority.md` G35 (auto memory off, checked at launch) |
| 25 | `memory.md` sections 5, 6 and 8 |
| 26 | `memory.md` sections 5 and 10 |
| 27 | `memory.md` sections 3, 4 and 7 |
| 28 | `memory.md` section 7 (retire, undo, revoke, `session_loads`) |
| 29 | `memory.md` sections 6 and 8 (excerpts stay local, the scrub hook); `authority.md` G30 and G31; `protocol.md` 4.1 (no reply or tool text in observations) |
| 30 | `authority.md` section 7 (one launch form, never `--agent`); build plan T4.1 (every launch form tested) |
| 31 | `authority.md` section 8 (the reviewer has no write tools); `memory.md` L2 |
| 32 | `memory.md` section 9 (the launch check verifies L1 on every launch); host-capabilities (the probe) |
| 33 | `authority.md` G15 (the orchestrator never edits code); `recovery.md` carried item 2 (thresholds as tunable defaults); plan v2 (treehouse alone owns worktrees) |
| 34 | `state.md` Handoff; `recovery.md` R02 (resume from the handoff record) |
| 35 | `state.md` Reservation; `protocol.md` section 8 (the feed fails closed when stale); `authority.md` G28; `recovery.md` R16 |
| 36 | `state.md` Attention; `protocol.md` 7.2 (receipts), 7.3 (the attention read, permission prompts apart) |
| 37 | `protocol.md` 7.3 (`/health`), section 9; `recovery.md` R05 and R18 (one poller per source, backoff) |
| 38 | `protocol.md` sections 1, 6 and 8 (AXI output, exit codes, idempotency classes, no model calls) |
| 39 | build plan T4.6 (infrastructure tests apart from behavior evals); `authority.md` section 12 (classifier, fragment and live tests kept apart) |
| 40 | `memory.md` section 6 (the budget is computed on the context each role actually loads) |
| 41 | build plan (the stack is decided; the estimate lives there) |
| 42 | `authority.md` section 2 (every Claude Code claim tagged); `protocol.md` (the [D], [T], [I] tags) |
| 43 | plan v2 (the revised rollout) |

### The parity matrix

Every kept row has spec text behind it. The two gaps section 1 found (P02, P47) were closed at CP2 (items 4 and 5). Rows where the spec adds nothing beyond the role or the build task rest on those (P40, P45, P65 and P66 are role evals; P52 is a live check). 43 of the matrix's 71 test names appear in no spec; they belong to the build tasks the matrix names (T4.2 spawn tests, T3.x board tests, T4.1 evals), and the matrix rule stands: the owner task may rename a test, but every row keeps at least one.

| Rows | Spec home |
|---|---|
| P01, P03, P06 | `state.md` Reservation and `run.start`; `recovery.md` R16; `authority.md` G17 and G28 |
| P02 | `protocol.md` 6.4, dispatch rules (item 4); `state.md` Card |
| P04 | `protocol.md` section 8; `state.md` Reservation (checked live at spawn) |
| P05 | `state.md` Run (`base_sha`, applied fix 7) |
| P07 | `state.md` Run (a new run of another role on the same card and worktree); `authority.md` section 4 notes |
| P08, P13 | `state.md` section 7 and Approval; `recovery.md` R13; `authority.md` section 6 (grants) |
| P09 | `authority.md` G33 |
| P10, P11, P12 | `state.md` Project (`delivery`); `authority.md` section 6 (`merge`, `land_direct`, local-only) |
| P14, P28 | `state.md` `pr.refresh`; `recovery.md` R07 |
| P15, P16 | `state.md` Obligation |
| P17 | `authority.md` G30 and G31; `recovery.md` R19 |
| P18, P20 | `state.md` `run.status` and Outbox; `protocol.md` 3.3 (one line per batch) |
| P19, P30 | `state.md` Hold and Run (`pause_until`); `recovery.md` thresholds and step 3 |
| P21 | `state.md` Run (`supervisor_session_id`); `recovery.md` R02 |
| P22 | `protocol.md` reads (`card show`, `run show --tail`) |
| P23 | `state.md` Outbox rules; `protocol.md` 3.3; `recovery.md` R09 |
| P24 | `state.md` `run.relaunch`; `protocol.md` `agent relaunch` and `agent interrupt`; `recovery.md` R03 and R12 |
| P25 | `recovery.md` R04 and section 2; `authority.md` G29 and the resume row |
| P26 | `protocol.md` 2.5 (idle orchestrators are woken by the outbox) |
| P27 | `state.md` Ask; `protocol.md` 2.2 |
| P29, P31, P35 | `state.md` Attention; `protocol.md` 7.3 |
| P32 | `state.md` Dependency |
| P33 | `state.md` Attention (`report`) and section 7 |
| P34 | `state.md` Approval (`away`); `authority.md` section 6 (away mode) |
| P36 | `protocol.md` 3.4 (applied fix 6) |
| P37 | `authority.md` G34; `recovery.md` R17 |
| P38, P39, P46 | `protocol.md` 7.3 and 7.6 (`/board`, `/snapshot`, the PR cache per project) |
| P40 | role eval (T4.1); the stage events are `state.md` Event |
| P41 | `state.md` Handoff; `recovery.md` R02 |
| P42 | `memory.md` section 6 (the budget) and section 9 |
| P43 | `protocol.md` `tiller doctor` and `/health`; `recovery.md` R01 |
| P44 | `state.md` Event |
| P45 | build plan T3.5 and T7.4; the counts come from `state.md` records |
| P47 | `protocol.md` 3.5 (item 5); the artifact record is `state.md` Artifact |
| P48, P49 | `recovery.md` R16 and R17 |
| P50 | `state.md` Artifact |
| P51, P53 | `state.md` `run.clean`; `authority.md` G18; `recovery.md` R15 |
| P52 | `recovery.md` R16 (Tiller reports; the sweep is the dotfiles') |
| P54, P55, P56, P57 | `recovery.md` R19, R14, R12, R01; `authority.md` G40 |
| P58, P59 | `authority.md` G15 and G16 |
| P60 | `protocol.md` 2.2 (`tiller ask note`) |
| P61, P62 | `docs/design/herdr-layout.md`; `state.md` caches; `protocol.md` section 9 and `agent focus`; `recovery.md` R05 and R18 |
| P63, P64, P67, P68 | `authority.md` section 7 (the launch form) and section 8 (allow lists); G39 |
| P65, P66 | role evals (T4.1) |
| D01, D04, D11 | `protocol.md` section 10; `authority.md` G25 and G35; items 12 and 13 |

### Firstmate contracts

Each spec's contract table states its departures with a reason. The review read the nine contract documents (`captain-hold-lifecycle`, `supervision-host`, `agent-control`, `subagent-guard`, `cd-guard`, `arm-pretool-check`, `fleet-ledger`, `herdr-backend`, `turnend-guard`) plus `VISION.md` against them and found four departures that no spec stated; two were real gaps and are applied fixes 4 and 5 (one live session per run with a proven stop before relaunch; the empty-composer rule before typing), and two were documentation (applied fix 12). The `aged` hold bucket is item 6.
