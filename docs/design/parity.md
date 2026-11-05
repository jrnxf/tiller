# Firstmate parity matrix

This is the checklist for "comparable" (build plan, test 2). Each Firstmate capability the developer actually uses has a row. A row has a Tiller owner task and the test that proves it, or it is a proposed drop for the developer to approve at CP1.

**Scope: host B only.** This matrix is built from host B's dry-run export (Sunday 2026-11-01). Host A's bundle does not exist yet. When it lands, re-run the same counts on it. Then add any row that host A uses and host B does not, and recheck each proposed drop: a drop proposed here can still be in use on host A. Host C runs its own Tiller later (T6.4) and is not covered.

Written Sunday 2026-11-01, Lisbon time. Every date below is a Lisbon date.

## How to read it

- **Evidence** is the use count over the bundle's window (Tuesday 2026-10-06 to Sunday 2026-11-01, 27 days, every orchestrator transcript on host B), plus the last date used. Tags give the source:
  - `[cap]`: the bundle's `capabilities.md` (counts Bash commands that name the tool, plus skill loads);
  - `[scan]`: a recount of the same transcripts over the same window, made for this matrix. It covers what `capabilities.md` does not list: skill loads by name, slash commands the developer typed, and scripts it missed;
  - `[rule]`: a standing rule in the bundle's `memory/rules.md`, with the date it was set. A rule is evidence of use even when no command counts it;
  - `[req]`, `[com]`, `[reg]`: the bundle's `memory/requirements.md`, `commitments.jsonl` and `regressions.md`;
  - `[cards]`: counts over the bundle's 713 cards.
- **Owner** is the plan task that builds it. "Spec" names the M1 spec that must describe it first (T1.6 checks this).
- **Test** is a proposed name, written as `crate::module::test_name` for Rust, `board/<file>: <name>` for the board, `evals/<name>` for role evals, and `live check at CP7` where only a person can confirm it. The owner task may rename a test, but every row keeps at least one test, and each test proves its own row.
- **Proof on main** is the checkable gate for the first cutover host, filled in Thursday 2026-11-05 (Lisbon time). It starts with a status word:
  - `tested`: the named Rust, board or CLI snapshot tests run in CI and prove the row;
  - `eval`: the named role eval proves it. Evals call real models, so they run only on demand (`evals/run.sh`); `evals/check.sh` validates their shape offline;
  - `live-check`: only a person can confirm it on the host at CP7. The cell gives the exact check;
  - `failing`: the row has no passing proof. The cell says what is tested and what is not built or not proven. Each one is a gap to close before cutover, not a pass;
  - `remote`: a remote-host row. Not tested for the first cutover host. The cell lists the tests on main, if any.
  Test names are `crate::module::test_name` without the leading `crates/` path. A name in the Test column is the original plan; the Proof column is what exists.
- **Decision**: `keep`, `keep (gap)` when no plan task names the work yet and the owner here is a proposal, or `drop?` for a proposed drop that waits for CP1. `keep (CP1)` and `drop (CP1)` record the CP1 outcome on a row that was proposed as a drop.

Project names are replaced with what each project is, so this file stays public-safe.

## Gate status for the first cutover host

The captain chose the Linux guest as the first cutover host, ahead of the personal Mac (Thursday 2026-11-05, Lisbon time); the Mac is the second host. The proofs below are scratch-database tests that run on Linux in CI, so they hold for either host. Only P52 is host-specific: its live check runs on the Linux guest first, then on the Mac. P62, D01 and D11 stay remote-host rows.

Parity rows, counted Thursday 2026-11-05 (Lisbon time). 71 kept rows (P01 to P68, plus D01, D04 and D11 kept at CP1); 12 dropped (D02, D03, D05 to D10, D12 to D15).

| Status | Rows | Count |
|---|---|---|
| tested | P01 to P06, P08, P12, P13, P17 to P32, P38, P39, P41, P42, P44, P45, P51, P53, P55, P57 to P61, P63, P64, P67, P68 | 43 |
| eval (on demand) | P16, P33, P37, P40, P54, P56, P66 | 7 |
| live-check | P52 | 1 |
| remote (not tested here) | P62, D01, D11 | 3 |
| failing | P07, P09, P10, P11, P14, P15, P34, P35, P36, P43, P46, P47, P48, P49, P50, P65, D04 | 17 |
| dropped | D02, D03, D05 to D10, D12 to D15 | 12 |

Two rows have notes the gate depends on:

- P02 lists four dispatch rules, but `config.rs` ships three plus the default (no opus-high rule). The tests cover the three that exist.
- P17 means the scrub list, quoted ask text and stale pipeline claims. It does not pattern-match local paths or LAN hosts; that check is `scripts/scrub-check.sh` on committed text only.

A `failing` row is not a test failure: its test suite is green. It means the capability is missing or only half built, so the checklist line "Parity rows for this host passing" is not met until each is built, or the owner drops or defers it at CP7.

## Rows

### Dispatch and capacity

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P01 | Dispatch: turn one ask into a worker in its own worktree, with model and effort chosen from rules, not by the developer | 1,143 uses, last Sun 2026-11-01 `[cap]`; 441 cards landed in four weeks | T4.2 (spec T1.1, T1.3) | `agents::spawn::spawn_creates_worktree_pane_and_run` | **tested**: `agents::spawn::tests::spawn_creates_worktree_pane_and_run` | keep |
| P02 | Model and effort rules: four dispatch rules (haiku low, opus high, opus medium, default sonnet medium) choose for every task; never downgrade without the developer's permission | every dispatch: Firstmate's spawn refuses to start a worker without a harness resolved from the rules, so every spawn counted in P01 passes through them; the resolver script was also run directly 81 times, last Sun 2026-11-01 `[scan]`; rules in the bundle's `config.json` | T4.2 | `agents::spawn::dispatch_resolves_model_effort_from_rules` | **tested**: `spawn::tests::dispatch_resolves_model_effort_from_rules`, `orchestrator_downgrade_refused`, `refuses_model_below_rule_without_owner_permission`. Note: `config.rs` ships three rules plus the default, not the four this row lists (no opus-high rule) | keep |
| P03 | Lane capacity: refuse a new lane when the lane count (7), free RAM (3 GB) or free disk (40 GB) is short; one project is capped at 2 lanes | 2 direct uses, last Sun 2026-11-01 `[cap]`, but it gates every spawn; host froze and disk filled before limits existed `[reg]` | T4.2 | `agents::spawn::two_spawns_cannot_oversubscribe_capacity`, `agents::spawn::spawn_refuses_below_disk_floor` | **tested**: `spawn::tests::two_spawns_cannot_oversubscribe_capacity`, `spawn_refuses_below_disk_floor`, `spawn_refuses_below_ram_floor` | keep |
| P04 | Quota floor: read the Claude usage windows before a batch; stop dispatch under the floor | 13 manual `quota-axi` uses, last Sun 2026-11-01 `[cap]`; no floor configured, open question Q10 (3% hard floor, 20% warning) | T4.2, feed in T1.3 and T2.8 | `agents::spawn::spawn_refuses_below_quota_floor` | **tested**: `spawn::tests::spawn_refuses_below_quota_floor`, `quota_floor_is_the_account_floor` | keep |
| P05 | Fresh clones: fast-forward the project's default branch before work starts, and after a direct push | 405 uses, last Sun 2026-11-01 `[cap]` | T4.2 | `agents::spawn::spawn_starts_from_current_origin_default_branch` | **tested**: `spawn::tests::spawn_starts_from_current_origin_default_branch` | keep |
| P06 | Worktree pool through treehouse | 402 uses, last Sun 2026-11-01 `[cap]` | T4.2 | `agents::spawn::spawn_takes_treehouse_worktree_and_returns_it_on_cleanup` | **tested**: `spawn::tests::spawn_takes_treehouse_worktree_and_returns_it_on_cleanup`, `spawn::pool::tests::treehouse_takes_and_returns_with_the_lease_holder`, `treehouse_refuses_a_relative_path_and_a_failing_get` (the real `Treehouse` argv) | keep |
| P07 | Promote a scout to a ship in the same worktree when the developer says go | 68 uses, last Sat 2026-10-31 `[cap]`; `scout-completion` skill 14 loads `[scan]` | T4.2 (spec T1.1: card kinds and follow-ups) | `agents::spawn::promote_scout_reuses_worktree_as_ship_run` | **failing**: not built: `spawn` always takes a fresh pool worktree and there is no promote path (`authority.md` section on promotion). No test can pass until it exists | keep (gap) |

### Delivery and landing

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P08 | `no-mistakes` delivery: a ship card is done only when the pipeline has run and a PR exists. The orchestrator checks this itself and does not take the worker's word | 677 uses, last Sun 2026-11-01 `[cap]`; `validation-supervision` skill 17 loads `[scan]`; workers reported done with no pipeline three times `[reg]` | T2.7 (column derivation), T4.1 (builder role) | `reconcile::columns::ship_card_stays_working_without_pr` | **tested**: `reconcile::columns::ship_card_stays_working_without_pr`, `a_done_report_with_an_open_pr_and_no_pipeline_notes_the_orchestrator` | keep |
| P09 | Ask-user gate escalation: a pipeline ask-user finding goes to the developer, never answered by a worker | `ask-user-authority` skill 19 loads, last Sun 2026-11-01 `[scan]` | T3.3 (attention item), T4.1 | `reconcile::attention::nm_ask_user_finding_becomes_attention_item` | **tested**: `reconcile::attention::nm_ask_user_finding_becomes_attention_item` (a `no-mistakes` review with verdict `ask-user` opens one `decision` item on its card, once per review; other reviewers and verdicts raise nothing). Builder side: `evals/builder_escalates_ask_user_finding` (on demand) | keep |
| P10 | Project registry and delivery mode: per project, PR or straight to the default branch, which checks apply, and lighter checks for no-logic changes | 123 uses, last Sun 2026-11-01 `[cap]`; `project-management` skill 12 loads `[scan]`; `[rule]` Thu 2026-10-08 and Mon 2026-10-05; forge detection at project add, 7 uses `[scan]` | T2.1 (project record), T2.4 (import of the registry) | `core::project::delivery_mode_round_trips_from_import` | **failing** (partly tested): `core::import::tests::p10_import_stores_each_projects_delivery_and_registry_fields`, `cli` project add/list snapshots. Open: the export bundle has no `checks` field (contract change, owner call), and forge detection at add and lighter checks for no-logic changes are not built | keep |
| P11 | No-PR delivery: the worker runs the project's own checks, pushes to the default branch, the orchestrator syncs, then the deploy build is watched to its result | `[req]` mem-41; `[rule]` Mon 2026-10-05 for three projects; deploy-build skill 7 loads `[scan]` | T4.3 (land), T2.7 (watch the deploy) | `agents::land::direct_push_runs_project_checks_then_pushes` | **failing** (partly tested): `agents::land::tests::p11_direct_push_lands_only_after_the_project_checks_pass`, `direct_push_is_fast_forward_of_bound_sha`, `expired_direct_grant_lands_nothing`. Open: the deploy watch never resolves, `Store::deploy_record` has no caller outside tests | keep (gap) |
| P12 | Land a PR on the developer's word: re-check live checks and head SHA, merge, record the outcome. Local-only projects merge without a forge | 627 uses, last Sun 2026-11-01 `[cap]`; `ship-landing` skill 24 loads `[scan]`; local merge 18 uses, last Mon 2026-10-19 `[scan]` | T4.3 | `agents::land::land_rechecks_head_sha_before_merge`, `agents::land::local_only_project_merges_without_forge` | **tested**: `agents::land::tests::land_rechecks_head_sha_before_merge`, `merge_passes_bound_sha_to_forge`, `local_only_land_fast_forwards_the_checkout`, `cli/tests/land.rs` | keep |
| P13 | Standing merge grant: on most projects, merge green in-scope PRs without asking; a few projects always wait for the word | `[rule]` Tue 2026-10-06, narrowed Wed 2026-10-21 | T4.3 (spec T1.4: a standing grant as a bound approval scoped per project) | `agents::land::standing_grant_merges_only_green_in_scope_prs` | **tested**: `agents::land::tests::standing_grant_merges_only_green_in_scope_prs`, `p13_a_standing_grant_does_not_reach_another_projects_pr`, revoked and expired grant tests | keep (gap) |
| P14 | PR and CI watching: PR checks, review state and merges outside the fleet wake the orchestrator | `fm-pr-check.sh` 305 uses, last Sun 2026-11-01 `[scan]`; PR events are one of the wake sources in the watch row `[cap]` | T2.7 (PRs in the reconcile loop), T2.5 (pollers) | `reconcile::pr::ci_green_event_wakes_orchestrator_once` | **tested**: `reconcile::pr::ci_green_event_wakes_orchestrator_once` (`Store::pr_refresh` enqueues one `wake` to the live orchestrator when checks turn green or red or a review decision lands on an open PR; an unchanged poll wakes nobody). Closed, force-pushed and leaking PRs are covered by `reconcile::pr` tests | keep |
| P15 | Promises tied to an event: "report it ready once CI passes" is a record that the event retires | `[com]` 1 open promise on a new project's PR (Sun 2026-11-01) | T2.7 (spec T1.1: obligations as records) | `reconcile::obligations::ci_green_retires_report_promise` | **tested**: `reconcile::obligations::ci_green_retires_report_promise`, `promise_made_after_green_checks_retires_on_the_next_pass`, `other_pr_or_other_checks_leave_the_promise_open` (an event obligation `{"kind": "pr.checks"}` is met by a pass that sees the PR's checks green, with the note to the orchestrator); date-triggered: `reconcile::review::weekly_review_due_notes_orchestrator`, `cli` obligation open/cancel | keep |
| P16 | Standing duty after each merge: tag the next pre-release on one public project, until the developer says stop | `[com]` standing since Fri 2026-10-23; `[rule]` Fri 2026-10-23 | T4.1 (orchestrator follows memory rules), memory in T1.5 | `evals/orchestrator_tags_prerelease_after_merge` | **eval**: `evals/orchestrator_tags_prerelease_after_merge` (tuning, on demand, offline-validated by `evals/check.sh`) | keep |
| P17 | PR body scrub before posting: no quoted chat from the developer, no internal tooling text, no stale pipeline claims | `[rule]` learnings.md:19; a public PR leaked a raw ask on Sun 2026-10-25 `[reg]` | T4.3 (check before the PR is opened or merged) | `agents::land::pr_body_with_quoted_ask_is_refused` | **tested**: `agents::land::tests::p17_a_leaky_description_is_refused_before_the_forge_is_called` (bug found and fixed: publish had no scrub), `pr_body_with_quoted_ask_is_refused` at merge, `reconcile::pr::leaking_body_blocks_grant_match_and_request` | keep (gap) |

### Supervision, wakes and liveness

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P18 | Watch workers and wake the orchestrator on status changes, PR events and timers, so the developer can walk away | 8,212 uses, last Sun 2026-11-01 `[cap]`; 1,949 supervision wake turns and 1,928 task-notification turns | T2.5 (outbox), T2.6 (observations), T2.8 (daemon) | `daemon::wake::worker_status_change_wakes_orchestrator` | **tested**: `delivery::outbox::status_change_types_one_wake_line_into_the_orchestrator_pane`, `reconcile::liveness` silence tests | keep |
| P19 | Scheduled wakes and check-ins: a worker's dated pause (`paused ... until <date>`) and a hold's re-ask date bring the card back on that date, with no alarm before | `[req]` mem-47; `[reg]` missed doorbell, Fri 2026-10-30; 14 dated holds `[cards]`, latest re-ask Tue 2026-11-17 | T2.7 (spec T1.1: `until` on runs and holds) | `reconcile::wake::dated_pause_silences_alarms_until_date` | **tested**: `reconcile::liveness::silent_alarm_returns_after_the_pause_date`, `dated_pause_silences_silent_escalation`, `reconcile::recovery::dated_hold_holds_until_its_date_then_asks_once` | keep |
| P20 | Worker outcome reports: a worker's done, blocked or decision reaches the orchestrator once, without flooding the dialog | 2,897 uses, last Sun 2026-11-01 `[cap]` | T2.6, T2.5 | `observe::outcome::worker_done_reaches_orchestrator_once` | **tested**: `delivery::outbox::status_change_types_one_wake_line_into_the_orchestrator_pane`, `one_line_per_batch_names_the_oldest` | keep |
| P21 | One owner per task: after a restart, exactly one supervisor steers each task | 418 uses, last Sun 2026-11-01 `[cap]` | T2.7 (spec T1.1: run ownership) | `reconcile::ownership::one_owner_per_run_after_restart` | **tested**: `reconcile::ownership::one_owner_per_run_after_restart` | keep |
| P22 | Read a worker's state from records, with a bounded pane tail | 3,124 uses, last Sun 2026-11-01 `[cap]` | T2.2 (`tiller` card and run reads), T3.2 (Agents view) | `cli::card::show_reports_run_state_and_freshness` | **tested**: `cli/tests/run_tail.rs::tail_is_the_last_lines_of_the_live_pane_without_escapes`, `tail_without_a_live_pane_is_unavailable_with_a_reason`, `tail_of_an_ended_run_has_no_live_session` (bug found and fixed: `tiller run show --tail` always answered `available: false`), `daemon::api::tests::pane_tail_drops_escapes_and_trailing_blank_lines` | keep |
| P23 | Steer a running worker durably, even mid-turn; detect an idle worker with unread messages and ring again | 785 uses, last Sun 2026-11-01 `[cap]`; 17 of 50 replay asks were follow-ups; `[req]` mem-47; `[reg]` missed doorbell | T2.5 | `delivery::outbox::idle_pane_with_unread_message_is_rung_again` | **tested**: `delivery::outbox::idle_pane_with_unread_message_is_rung_again`, `crash_between_store_and_send_*`, `busy_pane_*` | keep |
| P24 | Agent control: interrupt, restart or relaunch a stuck worker with a progress note | 73 uses, last Sun 2026-11-01 `[cap]`; `stuck-crewmate-recovery` skill 9 loads `[scan]` | T2.2 (command), T2.7 (detection) | `agents::control::relaunch_resumes_worker_in_own_worktree_with_note` | **tested**: `agents::control::tests::relaunch_resumes_worker_in_own_worktree_with_note` | keep (gap) |
| P25 | Reboot recovery: every worker comes back in its own worktree. No agent ever answers the external-import trust prompt for the developer | `[req]` mem-46; `[reg]` Tue 2026-10-06 and Sun 2026-11-01; `harness-adapters` skill 26 loads, last Sun 2026-11-01 `[scan]` | T2.7, T2.5 | `reconcile::recovery::reboot_resumes_workers_in_own_worktrees` | **tested**: `reconcile::recovery::reboot_resumes_workers_in_own_worktrees`, `resumed_session_at_trust_prompt_is_never_answered` | keep |
| P26 | Orchestrator never goes idle with work in flight (Firstmate's turn-end guard) | Stop hook, fires on every turn end `[cap]` | T4.4 (Stop hook in observe mode), T2.5 (daemon wakes the idle orchestrator) | `agents::intake::idle_orchestrator_is_woken_by_pending_outbox` | **tested**: `delivery::outbox::wake_waits_for_a_working_orchestrator_then_lands_once`, `idle_pane_gets_one_wake_line_naming_the_message` | keep |
| P27 | The supervisor knows the developer's latest words (Firstmate's main dialog mirror) | 1,304 mirror turns `[cap]` | T4.4 (every developer message is an ask record) | `agents::intake::every_prompt_records_an_ask` | **tested**: `agents::intake` redelivery and interleaving tests, `observe::tailer::transcript_intake_host_records_each_user_entry_as_one_ask` | keep |
| P28 | Repair outcomes that look finished but were never recorded | 15 uses, last Fri 2026-10-23 `[cap]` | T2.7 | `reconcile::recovery::merged_pr_without_record_closes_card` | **tested**: `reconcile::recovery::merged_pr_without_record_closes_card` | keep |

### Holds, decisions and away mode

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P29 | Captain holds: park a card on a question with a reason. Nothing closes while a question is open | 354 uses, last Sun 2026-11-01 `[cap]`; 162 held cards `[cards]`: 135 held for the developer (P30, P31) and 27 blocked-by (P32); `captain-hold-lifecycle` skill 9 loads `[scan]` | T3.3 (spec T1.1: attention and decision states) | `reconcile::holds::card_cannot_close_while_decision_open` | **tested**: `core::state::tests::close_needs_no_open_attention_and_no_active_run` | keep |
| P30 | Dated holds re-ask on their date | 14 of the 135 developer holds are dated `[cards]`; `[com]` 2 open (one due Sun 2026-11-01, the day of the export; one Sat 2026-11-07) | T2.7, T3.3 | `reconcile::holds::dated_hold_resurfaces_on_its_date` | **tested**: `reconcile::recovery::restart_turns_executing_uncertain_and_fires_timers_once`, `dated_hold_holds_until_its_date_then_asks_once` | keep |
| P31 | Undated holds wait for the developer's word and never nag | 121 of the 135 developer holds are undated `[cards]`; `[com]` 1 open (a release held for the developer's explicit word); open question Q9 | T3.3 | `board/attention.test.ts: undated hold shows under needs and never alarms` | **tested**: `reconcile::recovery::undated_hold_never_nags_across_days`, `board/src/components/attention/inbox.test.tsx` | keep |
| P32 | Blocked-by dependencies: a card starts when the card it waits on lands or reports | 27 blocked-by holds `[cards]`, 25 of them since done; `[com]` 2 open at export (T0.2 waits on T0.1; T0.6 waited on T0.4, which closed when T0.4 reported) | T2.7 (spec T1.1: dependencies) | `reconcile::deps::blocked_card_queues_when_dependency_lands` | **tested**: `core::state::tests::dependencies_release_on_a_satisfying_close` | keep |
| P33 | An investigation that ends with choices files its own held decision | `[req]` mem-42 | T4.1 (scout role handoff format) | `evals/scout_with_choices_files_a_decision` | **eval**: `evals/scout_with_choices_files_a_decision`, `evals/scout_ends_with_choices_as_needs_decision` (on demand) | keep |
| P34 | Away mode (AFK): leave with standing orders and a spend cap, get a return brief | 9 uses, last Sat 2026-10-31 `[cap]`; 5 contracts; `[com]` the last one closed when the developer returned | T1.4 (an away grant scoped to the contract), T4.2 (spend cap), T4.5 (return brief) | `agents::handoff::away_contract_caps_spend_and_writes_return_brief` | **tested**: `agents::handoff::tests::away_contract_caps_spend_and_writes_return_brief`, `away_quota_floor_binds_when_the_standing_floor_is_looser`, `away_scope_that_cannot_be_enforced_is_refused`, `reconcile::approvals::away_with_no_merges_stops_a_standing_grant_matching`, and the `cli` away steps (`away_start`, `away_brief`, `away_end`). Standing orders reach the orchestrator through `tiller handoff show`; the return brief is `tiller away brief` and the output of `tiller away end` | keep |
| P35 | Recap and walk through unanswered decisions on request (`/ahoy`) | typed 7 times, last Sat 2026-10-31 `[scan]` | T3.3 (attention inbox), T4.5 | `board/attention.test.ts: inbox lists every open decision with its evidence` | **tested**: `cli::ahoy::ahoy_lists_every_open_decision_with_its_evidence_most_blocking_first`, `cli::ahoy::ahoy_drops_answered_items_and_walks_down_the_rest` (`tiller ahoy` returns the recap and every open decision with its evidence, most blocking first; the orchestrator role walks them one at a time); the board inbox lists open decisions (`inbox.test.tsx`) | keep |
| P36 | Notify the developer only when something new needs them: a PR ready for their merge, or a new decision. Routine turns never notify. Opening the notification jumps to the orchestrator | `[rule]` Wed 2026-10-21; herdr notification commands 51, last Sun 2026-11-01 `[scan]` | T2.5 (herdr adapter sends it), T3.3 (what counts as new) | `delivery::notify::new_attention_item_notifies_once` | **tested**: `delivery::notify::new_attention_item_notifies_once` (a new decision or a `merge` item calls notify once, a restart never repeats it), `delivery::notify::routine_turns_and_settled_items_never_notify`; one notify on a reboot restore (`reconcile::recovery::reboot_resumes_workers_in_own_worktrees`) | keep (gap) |
| P37 | Refused actions: when the session's permission check refuses something the developer approved, do not retry; hand them one runnable command | `[reg]` Sun 2026-11-01 | T4.1 (orchestrator role) | `evals/refused_action_hands_back_one_command` | **eval**: `evals/refused_action_hands_back_one_command` (quota refusal), `evals/permission_refusal_hands_back_one_command` (permission refusal) (on demand) | keep |

### Status surfaces and session life

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P38 | Fleet snapshot: one structured read of every project, card, hold and lane | 75 snapshot uses, last Sat 2026-10-31 `[scan]`; feeds the T0.5 demo until cutover | T2.8 (boot snapshot), T2.2 | `daemon::api::snapshot_lists_every_open_card_and_hold` | **tested**: `daemon::api::tests::the_boot_snapshot_holds_exactly_what_each_get_returns`, `cli` snapshot test | keep |
| P39 | One-screen fleet board on request (`/bearings`) | 177 uses, last Sat 2026-10-31 `[cap]`; typed 33 times `[scan]` | T3.2 | `board/board.test.ts: all projects view shows every open card` | **tested**: `board/src/components/board/views.test.tsx`, `board/model.test.ts` | keep |
| P40 | Every update names its project first and groups by project. Every stage change of a build is reported, not only PR open and merge | `[rule]` Thu 2026-10-08 (both) | T4.1 (orchestrator reply shape), T2.7 (stage events) | `evals/orchestrator_reports_each_stage_by_project` | **eval**: `evals/orchestrator_reports_each_stage_by_project` (tuning, on demand, offline-validated) | keep |
| P41 | Stow and session start: save context at breakpoints, restore the fleet picture after `/clear` or a reboot | 45 uses, last Sun 2026-11-01 `[cap]`; `/stow` typed 22 times `[scan]` | T4.5 | `agents::handoff::fresh_orchestrator_resumes_from_handoff_record` | **tested**: `agents::handoff::tests::fresh_orchestrator_resumes_from_handoff_record`, `cli/tests/handoff.rs` | keep |
| P42 | Lean startup memory: always-loaded memory stays under a budget (7,500 tokens today) | 91 uses, last Sun 2026-11-01 `[cap]`. Tiller needs the outcome, not Firstmate's script | T1.5, T4.1 | `agents::roles::startup_context_stays_under_budget` | **tested**: `agents::roles::tests::startup_context_stays_under_budget`, `core::state::memory_tests` budget tests | keep |
| P43 | Startup health checks: missing or outdated tools, network checks, a validation daemon older than its CLI | deferred startup checks 21 uses, last Sat 2026-10-31 `[scan]`; `[reg]` daemon and CLI mismatch, Mon 2026-10-05 | T2.8 (health endpoint), T3.4 (degraded states) | `daemon::health::reports_missing_and_outdated_tools` | **tested**: `daemon::health::reports_missing_and_outdated_tools`, `daemon::health::a_daemon_at_its_cli_version_is_no_skew`, `reconcile::recovery::tool_version_skew_degrades_the_host_and_opens_recovery_after_an_hour`, `reconcile::tools` unit tests, `cli::doctor::tests::no_mistakes_daemon_older_than_its_cli_warns`, `cli/tests/doctor.rs`, `board health.test.tsx` | keep (gap) |
| P44 | Fleet activity ledger: dispatch, cleanup and outcome events for metrics | ledger on since about Tue 2026-10-20; `task.dispatched` and `task.cleaned_up` events `[cap]` | T2.1 (events written in the same transaction) | `core::events::every_mutation_writes_its_event` | **tested**: `core::events::every_mutation_writes_its_event`, `core::state::tests::a_write_without_its_event_is_rolled_back` | keep |
| P45 | Outcome metrics: count what landed | 3 uses, last Sun 2026-11-01, all in one task `[cap]` | T3.5 (Health page), T7.4 | `board/health.test.ts: shipping counts landed cards from state` | **tested**: `core::state::signals::shipping_counts_landed_cards_by_the_week_they_closed`, `board health.test.tsx` | keep |
| P46 | Published PRs and issues per project (contributions view) | 19 uses, last Fri 2026-10-30 `[cap]` | T3.2 (card detail and filters) | `board/src/components/board/contributions.test.tsx: project filter lists published PRs`; `crates/daemon` `contributions_list_published_prs_and_cards_list_their_pages` | **tested**: `GET /api/v1/contributions?project=` and the board's Contributions screen list the published PRs per project; the schema has no issues table, so only PRs are listed | keep |

### Review pages and artifacts

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P47 | Live review pages: hand the developer a live link to see and annotate a mock or report; the hosting worker owns the feedback rounds | 527 uses, last Sun 2026-11-01 `[cap]`; `[rule]` Sat 2026-10-10 and Sun 2026-10-18; `[req]` mem-42 | T4.1 (worker role), T2.5 (a poller source for page answers) | `delivery::sources::page_answer_reaches_owning_worker` | **failing**: not built: no feedback poller (`crates/delivery/src/sources.rs` is a doc comment); `protocol.md` section 3.5 is unimplemented | keep (gap) |
| P48 | Review listeners belong to a task and are retired at its cleanup; a respawn storm trips a rate limit, not the host | `[req]` mem-43; `[reg]` host froze Sun 2026-11-01 | T4.2 (cleanup), T2.5 | `agents::spawn::cleanup_retires_task_review_listeners` | **failing**: not built: no review listener registry and no spawn rate limit | keep |
| P49 | The review server always starts with the hostname the developer uses | `[reg]` 403 on every link, Sun 2026-10-11 | T4.2 (launch environment for workers) | `agents::spawn::worker_env_sets_review_allowed_hosts` | **failing**: not built: the worker launch environment sets no allowed hosts and nothing preflights them | keep |
| P50 | Pages index: every page or mockup made for the developer stays findable later | `[rule]` Sat 2026-10-10; 122 index rows, last Sun 2026-11-01; 103 index writes `[scan]` | T3.2 (card detail lists its pages; spec T1.1: an artifact record) | `board/src/components/board/contributions.test.tsx: card detail lists its published pages`; `crates/daemon` `contributions_list_published_prs_and_cards_list_their_pages` | **tested**: artifacts are recorded and idempotent (`cli` `card_show` snapshot, `same_key_twice`); `ArtifactRow` carries the URL and the card page lists its pages | keep (gap) |

### Workers and cleanup

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P51 | Clean up a finished task: return the worktree, close the pane, archive the card; never tear down unlanded work | 595 uses, last Sun 2026-11-01 `[cap]` | T4.2 (`tiller done`, cleanup) | `agents::spawn::cleanup_refuses_with_unlanded_commits` | **tested**: `agents::spawn::tests::cleanup_refuses_with_unlanded_commits`, `reconcile::safety::no_automatic_repair_removes_unlanded_work` | keep |
| P52 | Reclaim build output from worktrees no live task owns | `[reg]` disk filled three times, Wed 2026-10-21 | T6.1, T6.3 (each cutover confirms the dotfiles' daily slot sweep and Nix garbage collection, which do this on host B today, still run). T4.2's disk floor (P03) is the backstop | `live check at CP7: the daily slot sweep still runs on each host after cutover` | **live-check**: after cutover, on each host, wait one daily slot sweep, then check that build output under an unowned worktree is gone and `tiller doctor` reports the disk floor met. P03 `spawn_refuses_below_disk_floor` is the tested backstop | keep |
| P53 | Cleanup of a task with no card leaves no marker that fails later | `[req]` mem-56; `[reg]` Sun 2026-11-01 | T4.2 | `agents::spawn::cleanup_without_card_leaves_no_marker` | **tested**: `agents::spawn::tests::cleanup_without_card_leaves_no_marker` | keep |
| P54 | Workers resolve conflicts with a merge or the pipeline's reconcile path, never a hand rebase of published commits | `[reg]` Mon 2026-10-12 and Sun 2026-11-01 | T4.1 (builder role) | `evals/builder_merges_default_branch_instead_of_rebasing` | **eval**: `evals/builder_merges_default_branch_instead_of_rebasing` (on demand). No code guard against a hand rebase | keep |
| P55 | A pipeline fix agent cut at 30 minutes loses no partial work; the run reports the cut | `[reg]` recurring through October | T2.7 (run state shows the cut), T4.1 | `reconcile::runs::cut_fix_agent_reports_partial_work` | **tested**: `reconcile::runs::cut_fix_agent_reports_partial_work`, `cut_is_recorded_only_from_harness_evidence` | keep |
| P56 | Screenshot steps write to files, use a kill timeout and kill only their own browser | `[reg]` Sun 2026-11-01 | T4.1 (builder role brief rules) | `evals/screenshot_brief_finishes_inside_timeout` | **eval**: `evals/screenshot_brief_finishes_inside_timeout` (tuning, on demand, offline-validated) | keep |
| P57 | Private state directories whatever the umask | `[reg]` Tue 2026-10-06 | T2.1 (state directory creation) | `core::state::state_dir_is_private_under_umask_002` | **tested**: `cli::state_modes::p57_init_creates_owner_only_state_under_a_permissive_umask` (bug found and fixed: under umask 002 the state dir was 0775 and the database 0664), `core::backup` owner-only tests | keep |

### Guards

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P58 | The orchestrator stays in its home and never edits project code (cd guard; the first mate never does the work itself) | PreToolUse hook; 27 "blocked by" refusals, last Sun 2026-11-01 `[cap]` | T4.3 (spec T1.4) | `agents::guards::orchestrator_edit_outside_home_is_denied` | **tested**: `agents::guards::tests::orchestrator_persistent_cd_into_checkout_is_denied`, `orchestrator_edit_outside_home_is_denied` | keep |
| P59 | No unsupervised subagents from the primary session | PreToolUse hook, same 27 refusals `[cap]` | T4.3 (spec T1.4) | `agents::guards::orchestrator_subagent_spawn_is_denied` | **tested**: `agents::guards::tests::orchestrator_subagent_spawn_is_denied`, `nested_agent_and_scheduler_denied` | keep |

### Inbox, tools and skills

| # | Capability | Evidence | Owner | Test | Proof on main | Decision |
|---|---|---|---|---|---|---|
| P60 | Out-of-band notes: scripts and the developer drop a note for the orchestrator. A weekly dotfiles job writes here, so cutover must repoint it | 50 uses, last Sun 2026-11-01 `[cap]` | T2.2 (`tiller` ask intake from a script), T1.3 | `cli::ask::note_from_script_creates_ask` | **tested**: `cli` `ask_note` snapshot, behavior and idempotency tests | keep |
| P61 | herdr: every worker lives in a pane the developer can watch | 601 uses, last Sun 2026-11-01 `[cap]` | T2.5 (adapter, layout) | `herdr::layout::worker_workspace_nests_under_its_orchestrator` | **tested**: `agents::spawn::tests::spawn_creates_worktree_pane_and_run`, `herdr::layout` `worker_workspace_nests_under_its_orchestrator` | keep |
| P62 | Remote lanes through herdr: watch host B's panes from host A | Firstmate's remote lanes: 0 uses `[cap]` (0 saved machines on host B). The developer does reach host B's herdr through herdr's own machine support (a card fixed a notification's Open button that could not find host B's machine, Wed 2026-10-21 `[cards]`) | T3.2 ("Open in herdr" through the saved machine), T3.1 (Machines) | `board/agents.test.ts: open in herdr switches to the saved machine first` | **remote**: remote-host row, not tested here. On main: `daemon::api::tests::focus_says_what_it_did_on_this_machine`, `herdr::poller` remote tests, `board herdr.test.ts` machine-selection case | keep (view only, per the plan's default) |
| P63 | Browser checks through chrome-devtools-axi before a UI change is called right | 134 uses, last Sun 2026-11-01 `[cap]` | T4.1 (role tool list) | `agents::roles::builder_tools_include_browser_cli` | **tested**: `agents::roles::tests::builder_tools_include_browser_cli` (role tool list only) | keep |
| P64 | GitHub work through gh-axi | 632 uses, last Sun 2026-11-01 `[cap]` | T4.1, T4.3 | `agents::roles::roles_use_gh_axi_not_gh` | **tested**: `agents::roles::tests::roles_use_gh_axi_not_gh`, the merge-denied guard tests | keep |
| P65 | Firstmate skills the first mate loads, as Tiller behavior. Each one maps to its Tiller row: `harness-adapters` 26 (P25), `ship-landing` 24 (P12), `ask-user-authority` 19 (P09), `validation-supervision` 17 (P08), `scout-completion` 14 (P07), `project-management` 12 (P10), `diagnostic-reasoning` 11 (P66), `process-event-sources` 11 (P47), `captain-hold-lifecycle` 9 (P29), `stuck-crewmate-recovery` 9 (P24), `bearings` 8 (P39), `stow` 5 (P41), `session-start-recovery` 4 (P41), `afk` 3 (P34), `firstmate-self-repo` 2 (D14), `ahoy` 1 (P35) | `[scan]` skill loads, last Sun 2026-11-01 | T4.1 (orchestrator role carries the behavior, not the skill files) | `evals/orchestrator_role_covers_firstmate_skill_triggers` | **failing**: blocked by the rows it maps to that are failing: P07, P09, P34, P35, P47. The proposed umbrella eval does not exist; each other mapped skill row is covered by its own row | keep |
| P66 | Diagnose before fixing: scope a reported bug and reproduce it before acting on a report | `diagnostic-reasoning` skill 11 loads, last Tue 2026-10-27 `[scan]` | T4.1 (orchestrator and scout roles) | `evals/scout_reproduces_bug_before_proposing_fix` | **eval**: `evals/scout_reproduces_bug_before_proposing_fix` (on demand) | keep |
| P67 | The developer's own skills stay callable from Tiller's roles: publishing links, deploy builds, installs through the dotfiles, PR screenshots, reply style and others | `[scan]` 6, 7, 6, 3 and 3 loads, last Sun 2026-11-01. They live in the dotfiles, not Firstmate | T4.1 (role launch keeps user skills loaded) | `agents::roles::role_startup_lists_user_skills` | **tested**: `agents::spawn::tests::launch_keeps_the_developers_skills_callable` (no `--bare`, no skills off switch in the launch or fragment) | keep |
| P68 | Phone channel: Remote Control in the Claude app is on for the developer's own sessions and off for workers | `[rule]` Mon 2026-10-05, Fri 2026-10-16 | T4.2 (launch form per role) | `agents::spawn::worker_launch_has_remote_control_off` | **tested**: `agents::spawn::tests::worker_launch_has_remote_control_off`, `agents::roles::tests::fragments_set_the_launch_guards` | keep |

### Proposed drops (for CP1)

Each row waited for the developer's approval at CP1. A drop sends the capability nowhere: Tiller does not build it.

**CP1 outcome (Monday 2026-11-02, Lisbon time):** D01, D04 and D11 are kept; the other twelve are dropped. T1.6 recorded the outcome in the Decision column. The three kept rows have their spec home in `docs/spec/protocol.md` section 10 (remote homes as read-only peers, made a federated board by ADR 0003 (accepted Tuesday 2026-11-03), `tiller hook --harness`, `tiller config diff`) and `docs/spec/authority.md` (G25, G35); CP2 (Monday 2026-11-02) decided their open design: remote homes stay read-only in v1 and no other harness gets an adapter before cutover (`docs/spec/cp2-open.md`, items 12 and 13).

| # | Capability | Evidence | Reason to drop | Decision |
|---|---|---|---|---|
| D01 | Secondmates and remote homes | 0 uses since Tue 2026-10-06 `[cap]`; 1 incidental read, Wed 2026-10-28 `[scan]` | Never used on host B. Tiller's per-project orchestrators and the per-host daemon cover the same need. Recheck against host A's bundle | keep (CP1): spec `protocol.md` section 10 and `authority.md` G25; owner T3.1 (Machines). ADR 0003 (accepted Tuesday 2026-11-03) makes it a federated board: `protocol.md` 10.1 to 10.6, `authority.md` G41 to G44; owner F2 to F6, and the peer link (F3) replaces T2.5's peer summary poll. Proof on main: **remote**, not tested for the first cutover host |
| D02 | Backlog handoff between homes | 0 uses `[cap]` | Never used. One owner host per project (plan default) | drop (CP1) |
| D03 | Orca and Codex app backends | 0 uses `[cap]` | Never used. Tiller runs on herdr only | drop (CP1) |
| D04 | Other harness adapters (six non-Claude harnesses) | 0 uses `[cap]`; harness detection 14 uses, all reporting Claude `[scan]` | Tiller is Claude Code only in v1 | keep (CP1): spec `protocol.md` section 10 and `authority.md` G35; owner T2.6 (an adapter table per harness). Proof on main: **tested**: `observe::harness::every_harness_event_has_exactly_one_mapping`, `observe::harness::native_names_take_the_claude_event_and_shape`, `cli::hook::another_harness_writes_the_same_observations`, `agents::spawn::refuses_harness_without_pretool_transport` |
| D05 | Quota-array dispatch and the usage warner | 0 uses `[cap]` | Never used. P04 keeps the quota floor and T3.5 shows the quota | drop (CP1) |
| D06 | Quiet mode | 0 uses: its script never ran and its skill never loaded `[cap]`; the developer typed `/quiet` once, Sun 2026-10-18 `[scan]` | Away mode (P34) covers the developer's real use | drop (CP1) |
| D07 | X (Twitter) follow-ups and mail | 0 uses `[cap]` | Never used | drop (CP1) |
| D08 | Public follow-up collection | 0 uses `[cap]` | Never used | drop (CP1) |
| D09 | herdr project groups and lab sessions | 0 uses `[cap]` | The plan picks nested workspaces, not herdr groups (herdr layout rules) | drop (CP1) |
| D10 | Approval failover | 0 uses `[cap]` | Never used. Approvals are bound records in state.db | drop (CP1) |
| D11 | Config push to secondmate homes | 0 uses `[cap]` | Follows D01 | keep (CP1): spec `protocol.md` section 10 (`tiller config diff <peer>`); owner T2.2. Proof on main: **remote**, not tested for the first cutover host |
| D12 | Pipeline spend accounting | 0 uses; flag present but empty `[cap]` | Never used. T3.5 reads the quota feed instead | drop (CP1) |
| D13 | Watcher arm guard | PreToolUse hook `[cap]` | It guards a Firstmate mechanism (the orchestrator arming its own watcher) that Tiller does not have: the daemon owns watching. P58 and P59 keep the other two guards | drop (CP1) |
| D14 | Self-update of Firstmate | 3 uses, last Sat 2026-10-31 `[cap]` | Firstmate-only, like work on the Firstmate repo itself (`firstmate-self-repo`). Tiller updates through the Nix flake and the dotfiles | drop (CP1) |
| D15 | Mac deploy helper | 40 uses, last Wed 2026-10-28 `[cap]` | It belongs to the dotfiles, not the orchestrator. It stays on PATH and P67 keeps it callable; Tiller builds nothing for it | drop (CP1) |

## Gaps for CP1

Rows marked `keep (gap)` are capabilities in real use that no plan task names yet. The owner shown is a proposal. At CP1 the developer confirms each owner, or moves the row to the drops:

1. AFK away mode (P34), notifications (P36) and the startup health checks (P43).
2. Review page answers reaching the worker (P47) and the pages index (P50).
3. Agent relaunch (P24) and promoting a scout (P07).
4. Delivery: no-PR delivery (P11), the standing merge grant (P13) and the PR body scrub (P17). P13 needs T1.4 to allow a per-project standing approval. Without that, every merge waits for the developer's word, which is more friction than today.

## Coverage

Each bundle input maps to at least one row.

### `capabilities.md` (47 entries: 35 used, 12 unused)

| Entry | Row |
|---|---|
| Dispatch a worker into its own worktree | P01, P02 |
| Watch workers and wake the orchestrator | P18, P19 |
| Read a worker's current state | P22 |
| Steer a running worker | P23 |
| Captain holds and decisions | P29, P30, P31, P32 |
| Backlog and task records | T2.1 and T2.4 own it through every row; P44 |
| Validation pipeline (no-mistakes) | P08, P09 |
| Land a PR | P12, P13 |
| Clean up a finished task | P51, P52 |
| Supervision branch reports | P20 |
| Per-task supervision leases | P21 |
| Project clones kept fresh | P05 |
| Project registry and delivery mode | P10, P11 |
| Promote a scout to a ship | P07 |
| Live review pages (lavish) | P47, P48, P49 |
| Bearings board | P39 |
| Stow and session start | P41 |
| Startup memory budget | P42 |
| Away mode (AFK) | P34 |
| Host and lane capacity | P03 |
| Outbound checks for the developer's machines (Mac deploy) | D15 |
| Browser checks (chrome-devtools-axi) | P63 |
| GitHub operations (gh-axi) | P64 |
| Terminal multiplexer control (herdr) | P61 |
| Worktree pool (treehouse) | P06 |
| Captain inbox notes | P60 |
| Agent control plane | P24 |
| Contributions view | P46 |
| Inactive outcome reconcile | P28 |
| Quota reading | P04 |
| Self-update | D14 |
| Outcome metrics | P45 |
| Pre-tool guards (cd, watcher arm, subagent) | P58, P59, D13 |
| Turn-end guard | P26 |
| Main dialog mirror | P27 |
| Unused: remote secondmates and remote homes | D01 |
| Unused: backlog handoff between homes | D02 |
| Unused: Orca and Codex app backends | D03 |
| Unused: other harness adapters | D04 |
| Unused: quota-array dispatch and usage warner | D05 |
| Unused: quiet mode | D06 |
| Unused: X follow-ups and mail | D07 |
| Unused: public follow-up collection | D08 |
| Unused: herdr project groups and lab sessions | D09 |
| Unused: approval failover | D10 |
| Unused: config push to secondmate homes | D11 |
| Unused: pipeline spend accounting | D12 |

"Backlog and task records" has no row of its own on purpose. It is the card store itself, which T1.1, T2.1 and T2.4 build and every other row tests. T2.4's acceptance (the import reproduces every card count in the manifest) is its test.

### `memory/requirements.md` (6)

| Requirement | Row |
|---|---|
| mem-41 no-PR delivery | P11 |
| mem-42 investigations file their own decision; the page owner handles rounds | P33, P47 |
| mem-43 review listeners retired at cleanup | P48 |
| mem-46 reboot recovery without answering the trust prompt | P25 |
| mem-47 doorbell redelivery and dated pauses | P19, P23 |
| mem-56 cleanup without a card leaves no marker | P53 |

### `commitments.jsonl` (8)

| Commitment | Row |
|---|---|
| Parked product app, re-ask due Sun 2026-11-01 | P30 |
| Parked pre-launch review, re-ask Sat 2026-11-07 | P30 |
| Release held for the developer's explicit word, no date | P31 |
| T0.2 starts when T0.1 lands | P32 |
| T0.6 starts when T0.4 reports | P32 |
| Standing: tag a pre-release after every merge on one project | P16 |
| Away contract, closed on return | P34 |
| Report a new project's PR ready once CI is green | P15 |

### `regressions.md` (14)

| Regression | Row |
|---|---|
| Stale review-page listeners froze the host | P48 |
| Review links answered 403 "forbidden host" | P49 |
| Restored workers in the wrong checkout at a trust prompt | P25 |
| Missed steering doorbell | P19, P23 |
| Disk filled from build output in returned worktrees | P03, P52 |
| Public PR body leaked a raw ask | P17 |
| Workers reported done without running the pipeline | P08 |
| Rebased PR blocked the validation gate | P54 |
| Validation daemon older than its CLI | P43 |
| Group-writable state directory refused listeners | P57 |
| Fix agents cut at 30 minutes | P55 |
| Headless Chrome hung workers past their timeout | P56 |
| Cleanup without a card left a failing marker | P53 |
| Permission check refused approved actions | P37 |

T1.2 also maps every regression to a recovery-table row. This matrix only checks that each one has a kept capability behind it.

### Firstmate contract docs

| Doc | Rows |
|---|---|
| captain-hold-lifecycle | P29, P30, P31, P32, P38 (snapshot buckets) |
| arm-pretool-check | D13 |
| agent-control | P24, P25 |
| subagent-guard | P59 |
| cd-guard | P58 |
| supervision-host | P18, P20, P26, P27, P34 |
| fleet-ledger | P44 |
| herdr-backend | P61, P62, D09 |

### VISION.md

| Principle | Rows |
|---|---|
| One interface: workers report only through the orchestrator; escalate only real decisions | P20, P36, P40 |
| Authority is explicit: merges and destructive acts need the developer's word; standing autonomy is scoped per project | P12, P13, P34, P37 |
| The first mate never does the work itself | P58 |
| Scripts own the mechanics; token efficiency | P18 (watching costs no tokens), P42 |
| A restart is a non-event: obligations close only by records | P15, P21, P25, P41 |
| Delegation with a spine: unlanded work is never torn down | P08, P51 |
| Quota, model and effort stay owned by the developer | P02, P04 |

These rows hold the principles as tests. Two principles need no row. "The fleet outlives any vendor" is replaced on purpose: Tiller is Claude Code and herdr only (D03, D04). "Opt-in features" becomes a T1.4 rule, not a capability.
