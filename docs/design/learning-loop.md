# The learning loop

How Tiller learns from its own cards without letting an unapproved word reach a fleet role. This is the design T7.3 builds. It sits on `docs/spec/memory.md` (evidence, proposals, approvals; sections 6 and 7), `docs/design/plan.md` section 11 (the loop and its metric) and `docs/design/plan-v2.md` points 22 and 23 (attribution, successes, "nothing worth saving"). It adds no memory contract; it adds the tables and commands that feed memory's `evidence` and `proposals` from the record, and the gate that stands in front of an approval.

Written Wednesday 2026-11-04, Lisbon time. Task T7.3.

## 1. The loop in one page

| Step | Who | Tokens | What it writes |
|---|---|---|---|
| Capture | hooks, the tailer, reconcile | none | `events`, `observations`, `attention`, `reviews`, `runs` (already there) |
| Select | reconcile's retro sweep | none | one `retros` row per closed card: `light` or `full`, with the reason |
| Count | the sweep | none | `retros.counts`: the numbers scripts can compute |
| Retro | the curator, a headless Sonnet session | some, only for `full` | at most one learning, as an `evidence` row; a proposal through `tiller mem propose`; or nothing |
| Propose | the curator, agents, the owner | - | `proposals` with evidence and an `expected_effect` (memory.md section 6) |
| Bundle | the owner | none | `change_sets`: one change, or a named bundle |
| Gate | `tiller learn eval`, then `mem.decide` | the holdout run | `eval_runs`; an approval is refused without a passing run |
| Approve, apply | the board, the daemon | none | memory.md section 7 |
| Measure | the sweep, `tiller learn status` | none | the measuring set's window of cards; a worsened guardrail is reported with a revert offer |

Two things stay true at every step: nothing here renders into a fleet role (the curator is not a fleet role, and `tiller retro show` refuses a fleet session), and "nothing worth saving" is the normal end of a retro.

## 2. Selecting retros

Every closed card except a cancelled one gets exactly one retro row per close. The sweep (`tiller retro sweep`, also reconcile's step 5a on every pass) finds closed cards with no retro for their `closed_at` and writes one. The host health card never closes, so it never gets one.

The depth is decided from the record alone, in this order:

| Depth | Reason | Rule |
|---|---|---|
| `full` | `incident` | the outcome is `failed`, `abandoned` or `superseded`; or a `guard.breached` event is on the card; or a run on the card ended `failed`, `abandoned` or `cut`; or the card is a revert (`category = revert`) or a fix (`fixes_card_id` set); or (later) a fix card points at this one, which re-opens the retro: see below |
| `full` | `surprising_win` | the outcome is `landed` or `reported`, on the first attempt of every role, with no attention item that was not withdrawn, no `high` or `medium` review finding, and an elapsed time (created to closed) at or under half the median of the last 20 landed or reported cards of the same size. Fewer than 5 such cards: no win is judged |
| `full` | `sample` | the first 8 bytes of SHA-256 over the card id, as a big-endian integer, modulo `learning.sample_every` (default 5) is 0 |
| `light` | `routine` | everything else, and every `question` card that is not an incident |

A `light` retro is done the moment it is written: counts only, outcome `nothing`. A `full` retro is `pending` until the curator finishes it.

**Escaped defects.** A fix card (`fixes_card_id` set) created after its origin closed means the origin's retro missed something. The sweep writes a second retro on the origin, `full` with reason `incident`, once per fix card (the fix card's id is in the retro's `trigger`). The origin's earlier retro stays.

## 3. The retro record

`retros`: one row per retro.

- `id`, `card_id`, `project_id`, `closed_at` (the close this retro is for), `trigger` (null, or the fix card that re-opened it).
- `depth` (`light`, `full`), `reason` (`incident`, `surprising_win`, `sample`, `routine`).
- `counts`: JSON, computed by the sweep, never by a model: `elapsed_minutes`, `attempts` (per role), `attention` (per kind, withdrawn excluded), `findings` (high, medium, low over the card's reviews), `guard_breaches`, `fix_cards` (cards pointing at this one), `runs` (count), `models` (distinct), `status_lines` (count). The Health page's classifier (T7.1) labels why the developer was needed; these are the raw counts it labels.
- `config_hashes`: JSON list of the distinct `config_hash` values over the card's runs and sessions, so a learning can be tied to the configuration that produced it (point 22).
- `state` (`pending`, `done`), `outcome` (`nothing`, `evidence`, `proposed`, `unfinished`; null while pending), `learning` (the one line the curator kept, or null), `evidence_id` (the evidence row that holds it), `proposal_id` (when a proposal was filed).
- `curator_session` (the harness session id of the curator run), `curator_started_at`, `finished_at`, `finished_by`, `created_at`, `rev`.

`observations` rows a retro cites are kept past the 30-day retention (`state.md`, Observation).

## 4. The curator

The curator is a headless Claude Code session (`claude -p`) that `tiller retro run <retro>` launches for one `full` retro. It is not a fleet role: it has no run, no pane, no card of its own, and it reads what fleet roles never see (evidence rows, transcripts, quoted asks). It is launched with `roles/curator.md` appended as the system prompt plus the retro brief, `roles/curator.settings.json` as the settings fragment, the packaged MCP config, `--permission-mode dontAsk`, `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, `TILLER_ROLE=curator` and `TILLER_RETRO_ID=<retro>`. The model is `learning.curator_model` (default `sonnet`); the run is cut at `learning.curator_timeout_minutes` (default 20). The curator runs in a process group of its own. A cut, an interrupt of `tiller retro run` (Ctrl-C, SIGTERM or SIGHUP; the curator's group no longer gets the terminal's Ctrl-C, so `tiller` catches it) or a failed wait kills that whole group, so nothing the curator started outlives the run, and the retro ends `unfinished`. A curator that exits by itself has its group killed too, before it is reaped, so nothing it left behind (a server, a `sleep`, anything holding its output open) lives on or holds the run open.

The brief, written by `tiller retro run` from the record: the card and its acceptance, the counts, every run with its model, attempt, outcome and last status line, every attention item with the owner's exact answer, the reviews, the card's events in order, the evidence rows already on the card, prior retros, and the transcript paths of the card's sessions, which the curator reads with `Read`. Transcripts stay where Claude Code wrote them; nothing is copied.

The curator's job (`roles/curator.md`):

1. Answer "what would a future agent still get wrong, or still do right, without a change?" (Every's counterfactual test). A fact already in approved memory, a one-off, or a thing a checkable command answers (a version, a path, a service's state) is not a learning (memory.md section 5, "checkable facts are queried").
2. Keep at most one learning. Write it as evidence: `tiller retro finish <retro> --learning <one line> --evidence <json pointers>`.
3. When it is backed by two cards, or one severe incident, and the exact change is clear, also propose it: `tiller mem note` records the evidence, `tiller mem propose <project|fleet> --add <section> | --rewrite <memory> | --retire <memory>` files the one-entry change against the Tiller document with `--evidence <id>...` and `--expected-effect`, then `tiller retro finish <retro> --proposed <proposal>`. A mechanical rule (a path never to touch, a command never to run) is proposed as a hook or lint change on a `ship` card, not as prose.
4. Otherwise `tiller retro finish <retro> --nothing`. This is the normal result; the brief says so.

The curator writes nothing else: its write set is its scratch directory, its tools are `Read`, `Grep`, `Glob` and Bash for `tiller retro`, `tiller mem`, `tiller card show`, `tiller run show` and `tiller events`, typed by the packaged path: the curator has no hooks, so nothing rewrites a bare `tiller` to the one path the sandbox exempts (`authority.md` section 7, "How roles reach the exempt binary"). The actor on everything it writes is `curator:<retro id>`, which `tiller` derives from `TILLER_RETRO_ID`; it is not an owner and not a session, so a proposal it files has no `authored_by_session_id` and `created_by = curator:<retro id>`.

A curator run that exits without finishing leaves the retro `done` with outcome `unfinished` and the exit in `learning`; the sweep does not retry it. The owner runs it again when it was the environment: `tiller retro run <retro>` in a terminal, or Run again on the board (the retro's row in Memory's Retros list, or the Health item that reports it), which claims it by the same rules and runs the curator on the daemon (`POST /api/v1/retros/{id}/run`, `protocol.md` section 7). Run again is refused while another retro of the same card runs, and a daemon that stops ends the curator it started, with all it started, so the retro ends `unfinished` again. A second failure shows its own reason, and nothing runs it again by itself. That start resets the retro to `pending` first: it clears `outcome`, `learning`, `finished_at` and `finished_by` and records them on a `retro.reset` event, then claims the retro as any start does. Only `unfinished` resets; a retro done with any other outcome is final and its start is refused. `tiller retro run` claims the retro before it clears and writes the curator's scratch directory, so a run refused because a curator is still running changes nothing; `--dry-run` is refused by the same rule and claims nothing. A claimed run whose curator cannot be prepared or started also ends `unfinished`.

`retro.finish` is keyed. A `light` retro cannot be finished; a `done` retro finished again with the same outcome is a replay, with another outcome a refusal.

## 5. Config hash

Every run and every session Tiller launches carries `config_hash` (`state.md`, Session and Run). Until now it was the SHA-256 of the settings fragment alone. From T7.3 it is the SHA-256 of a manifest, and the manifest is stored, so two hashes can be compared part by part:

| Part | Value |
|---|---|
| `role` | SHA-256 of the packaged role file |
| `settings` | SHA-256 of the rendered settings fragment |
| `mcp` | SHA-256 of the packaged MCP config |
| `model`, `effort` | as resolved by dispatch |
| `tiller` | this binary's version |
| `claude` | the Claude Code version, when `claude --version` answered within two seconds; else `unknown` |
| `memory` | SHA-256 over the sorted `(source_id, blob_sha)` pairs rendered into the prompt (memory.md sections 2 and 7) |

`config_manifests` holds one row per distinct hash (`config_hash`, `parts` JSON, `first_seen_at`), written by `config.manifest_record` on first sight. `tiller learn config <hash> [<other>]` prints a manifest, or the parts in which two differ. Skills, policies and the project's own files enter through `settings` (the fragment lists every hook and exclude) and `memory`; a change to any of them is a changed hash.

Orchestrator sessions get a manifest from `tiller orchestrator start`. A session started before the hash existed carries none, and `tiller learn status` says how many sessions in a window carry no hash.

## 6. One change at a time, or a named bundle

A change set is the unit the loop measures.

`change_sets`: `id`, `name`, `state` (`open`, `measuring`, `closed`), `window_cards` (default `learning.window_cards`, 10), `opened_at`, `measuring_from` (the first member's apply), `closed_at`, `close_reason` (`window_complete`, `abandoned`), `rev`. `change_set_members`: (`change_set_id`, `proposal_id`, `added_at`); a proposal belongs to at most one set.

- `tiller learn bundle <name> <proposal>...` opens a set with its members. `tiller learn add <set> <proposal>` adds one while the set is `open`; a member added after an eval run invalidates it (section 7). `tiller learn abandon <set>` closes it. A member that `mem.repropose` replaces leaves its set (`change_set.member_removed`, with `proposal` and `replaced_by`), and a set left empty closes `abandoned` (memory.md sections 6 and 12).
- A lone proposal is a set of one. `tiller learn eval --proposal <p>` opens it, named after the proposal.
- The sweep moves an `open` set to `measuring` once any member is `applied`, and closes it `window_complete` once `window_cards` cards have closed `landed` or `reported` since `measuring_from`.
- **The rule.** While a set is `measuring` and its window is not complete, no proposal outside that set can be accepted. The gate (section 7) says which set is measuring and how many cards remain. Nothing stops the owner from opening a second set and running its holdout meanwhile; only the approval waits. This is "one change at a time, or a named bundle" (point 22): the bundle is measured as a bundle, and its name is the attribution.

Role prompts, skills and settings live in this repository and reach a host through a PR and a rebuild, not through a proposal. They show in the config hash, and the Evals workflow's holdout run on the PR is their gate. A change that touches both a role file and memory is one bundle by name: the PR names the set, and the set's eval run is the one recorded for the PR.

## 7. The holdout gate

`eval_runs`: `id`, `change_set_id`, `split` (`holdout`), `members_sha256` (SHA-256 over the sorted `diff_sha256` of the set's members at run time), `candidate_sha256` (of the rendered candidate text), `cases_total`, `cases_passed`, `overall_score`, `cost_usd`, `partial`, `passed`, `claude_version`, `judge_model`, `result_path` (local, never committed), `ran_at`, `recorded_by`.

`tiller learn eval <set>`:

1. Renders the candidate: for each member, the target source's canonical file at the proposal's `base_blob` with the proposal's diff applied (`git apply` in a temporary directory), concatenated under a heading per source. A set whose members target the same file are applied in order on one copy. `--candidate-file <f>` substitutes a rendered candidate (for a role change that rides a PR, or a test).
2. Runs `evals/run.sh --split holdout --candidate <file> --out <dir>` from `learning.evals_dir`. `--candidate` is new in `run.sh`: the file is appended to every case's system prompt after the role body, under a `# Memory (candidate)` heading, the way the launcher renders approved memory. It is allowed on the holdout because it is the thing being measured; the tuning-only knobs stay refused. The runner runs in a process group of its own. An interrupt of `tiller learn eval` (Ctrl-C, SIGTERM or SIGHUP, which `tiller` catches because the group no longer gets the terminal's Ctrl-C), or a daemon shutdown for a board eval, kills that whole group, so nothing the runner started outlives the run, and nothing is recorded. A run that records nothing names the runner and its result by their place in the checkout and the run (`evals/run.sh`, `sealed/result.json`), never by an absolute path; `tiller learn eval` adds the run's directory as the hint.
3. Records the row from `<dir>/sealed/result.json`: `passed` is every case at or above the pass mark and the run not partial. `--result <json>` records a run made elsewhere (the Evals workflow's artifact) instead of running one.

`learning.gate(proposal)` is the check `mem.decide` runs before it accepts, and `tiller learn gate <proposal>` prints it:

1. The proposal is `proposed`.
2. It belongs to a change set.
3. The set's newest holdout run has `passed = 1` and a `members_sha256` equal to the set's current members. A member added or re-based since the run makes it stale: run again.
4. No other set is `measuring` with cards still to come.

**From the board.** The memory view's "Run holdout eval..." on a suggestion runs the same thing for the suggestion's change set, or for a set of one when it is in none (`GET` and `POST /api/v1/memory/proposals/{id}/eval`, protocol.md section 7.4). The `GET` is the plan the confirmation shows: the holdout cases (counted, never read), the case runs and judge runs, their sum as model runs, the weekly quota percent that is estimated to take (`model_runs × learning.eval_percent_per_run`), what it leaves of the weekly window with the least left, the floor, and the minutes it is estimated to take (`case_runs × learning.eval_minutes_per_run`). Both figures are estimates, rounded to a tenth. The `POST` refuses, with the first reason the plan gives, when: the config does not parse; the proposal is not `proposed`; another board eval is running (one at a time on a host); the text moved (re-propose first); a set is measuring; the gate needs no run; `learning.evals_dir` is unset or holds no holdout cases; or the run would leave the week at or under the quota floor for new work (the builder floor, raised by an away contract's floor), which fails closed on a missing or stale reading. A started run records its `eval_runs` row exactly as the command does. Its files go to `<home>/evals/<ulid>`; the daemon keeps the newest 20 of those directories and removes older ones when it starts a run, so an `eval_runs.result_path` from an older board run can name a directory that is gone (the row keeps the counts; the path is a local pointer only). One that records nothing (the runner fails, or leaves no result) keeps its reason in the daemon's memory, and the memory view shows it on the suggestion.

A refusal names the failing step and the command that clears it. The memory view shows the same verdict beside each proposal (`GET /api/v1/memory`, T7.2, from `Store::learning_gate`). A legacy placement plan (memory.md section 10) is one named bundle with one holdout run, so the owner's batch approval is one gate.

`mem.decide` (T7.2) calls `learning_gate` as one line in its precondition list; this document is the contract for it.

## 8. Measuring and "nothing worth saving"

`tiller learn status` prints: the retros of the last 30 days by depth, reason and outcome (the share ending `nothing` is reported as a plain number, not a warning); the pending retros; the open and measuring change sets with cards closed of `window_cards`; and for the measuring set the guardrails of its window against the same number of cards before it: attention items per accepted card, rework attempts, escaped bugs, review findings per PR. When any of the four is worse, the line `revert_suggested` names the set and the figures. The revert itself is a `retire` proposal the owner opens in the memory view (memory.md section 7); the loop never applies one.

A model upgrade changes the `claude` part of every new hash; `tiller learn status` lists the hashes seen in the window, so the owner sees when a window straddles an upgrade and re-runs the holdout (plan.md section 11, "model upgrades re-run the evals").

## 9. Mutations and events

| Mutation | Caller | Preconditions | Writes | Events |
|---|---|---|---|---|
| `retro.schedule` | reconcile sweep, `tiller retro sweep` (owner, daemon) | card closed, not cancelled, not the host health card; no retro for this close (or this fix card) | `retros` | `retro.scheduled` |
| `retro.start` | `tiller retro run` (owner, daemon), the board's Run again (owner) | retro `pending` and no curator started on it within `learning.curator_timeout_minutes`, or `done` with outcome `unfinished` | `retros.curator_session`, `curator_started_at`; for an `unfinished` retro first `state = pending` and `outcome`, `learning`, `finished_at`, `finished_by` cleared | `retro.reset` (an `unfinished` retro only), `retro.started` |
| `retro.finish` | the curator, the owner | retro `full` and `pending` (a `done` retro replays the same outcome); `--learning` and `--proposed` need their argument | `retros` (state, outcome, learning), `evidence` (for `--learning`) | `retro.finished`, `evidence.captured` |
| `change_set.create` | owner | each proposal exists, is `proposed` and in no set; name not used by an open set | `change_sets`, `change_set_members` | `change_set.created` |
| `change_set.add` | owner | set `open`; proposal as above | `change_set_members` | `change_set.member_added` |
| `change_set.close` | owner (`abandoned`), sweep (`window_complete`), `mem.repropose` (`abandoned`, a set it leaves empty) | set not `closed` | `change_sets` | `change_set.closed` |
| `change_set.measure` | sweep | set `open`, a member `applied` | `change_sets.state`, `measuring_from` | `change_set.measuring` |
| `eval_run.record` | `tiller learn eval` (owner) | set exists; result parses | `eval_runs` | `eval_run.recorded` |
| `eval_run.fail` | the daemon, when a holdout eval the board started records nothing | set exists; the reason is cut to 2,000 characters | none (the event is the record) | `eval_run.failed` |
| `config.manifest_record` | `tiller spawn` | none; a known hash writes nothing | `config_manifests` | `config.manifest_recorded` |

Every row is authoritative and backed up with `state.db` (`state.md` section 10). `result_path` and transcript paths are local pointers; nothing in these tables is committed to git.

## 10. Commands

| Command | Caller |
|---|---|
| `tiller retro sweep` | owner, daemon |
| `tiller retro list [--pending] [--project <p>]` | owner, curator |
| `tiller retro show <retro\|card>` | owner, curator; a fleet session is refused (exit 6), since the output carries evidence rows and quoted answers |
| `tiller retro run <retro\|card>` | owner, daemon |
| `tiller retro finish <retro> (--nothing \| --learning <t> [--evidence <json>] \| --proposed <proposal>) [--key <k>]` | curator, owner |
| `tiller learn bundle <name> <proposal>...`, `tiller learn add <set> <proposal>`, `tiller learn abandon <set>` | owner |
| `tiller learn eval <set> [--candidate-file <f>] [--result <json>] [--out <dir>]`, `tiller learn eval --proposal <p> ...` | owner |
| `tiller learn gate <proposal>` | owner, board |
| `tiller learn status` | owner |
| `tiller learn config <hash> [<other>]` | owner |

Config keys, in `[learning]` of `config.toml`: `sample_every` (5), `window_cards` (10), `curator_model` (`sonnet`), `curator_timeout_minutes` (20), `evals_dir` (the checkout that holds `evals/run.sh`; required by `tiller learn eval` without `--result` and by the board's eval), `eval_percent_per_run` (0.1, the weekly quota percent one model run of the holdout is estimated to take), `eval_minutes_per_run` (0.5, the minutes one case run is estimated to take, its judging included).

## 11. What is enforced and what is convention

| Guarantee | Label | Test |
|---|---|---|
| Every closed card except a cancelled one gets one retro, decided from the record, and the same card always gets the same depth | enforced | `learning::sweep_is_deterministic_and_complete` |
| An incident, a surprising win and a sample are `full`; the rest is `light` and done at once | enforced | `learning::incident_and_win_are_full`, `learning::sample_follows_the_hash` |
| A fix card re-opens its origin's retro once | enforced | `learning::escaped_defect_reopens_the_origin` |
| A `light` retro cannot be finished; a `full` one finishes once | enforced | `learning::finish_rules` |
| `tiller retro show` refuses a fleet session | enforced | `cli::learn::retro_show_refuses_a_fleet_session` |
| `--learning` writes exactly one evidence row with the retro as its pointer and the card as its independence key | enforced | `learning::learning_becomes_evidence` |
| The gate refuses a proposal with no set, with no passing run, with a stale run, or while another set measures | enforced | `learning::gate_rules` |
| A member added after the run makes the run stale | enforced | `learning::gate_rules` |
| The sweep moves a set to measuring on its first applied member and closes it when the window is complete | enforced | `learning::window_closes_after_n_cards` |
| Every spawned run carries a manifest-backed hash, and two manifests diff by part | enforced | `agents::spawn::config_hash_covers_the_manifest`, `learning::manifest_diff` |
| `run.sh --candidate` appends the candidate to every case, on both splits | enforced | `evals/check.sh` fixture build; the appended text is visible in the generated `case.yaml` |
| The curator keeps checkable facts out of memory and ends "nothing" on a routine card | convention: the role prompt | `evals/cases/*/curator_*` |
| `mem.decide` calls the gate | enforced | `state::memory::keep_waits_for_a_passing_holdout_run`, `state::memory::a_proposal_is_filed_with_its_evidence_and_kept_only_through_the_gate` |
| A proposal cites at least one evidence row on its project and states an `expected_effect` | enforced | `state::memory::a_proposal_without_backing_or_outside_its_project_is_refused`, `cli::mem::a_proposal_needs_evidence_and_a_line_that_passes_the_scrub` |

Not provable without live data, and so listed as unverified in the done line: a curator run on a real transcript, a holdout run with a real candidate, and a window measured over ten real cards.
