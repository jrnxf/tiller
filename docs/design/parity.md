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
- **Decision**: `keep`, `keep (gap)` when no plan task names the work yet and the owner here is a proposal, or `drop?` for a proposed drop that waits for CP1.

Project names are replaced with what each project is, so this file stays public-safe.

## Rows

### Dispatch and capacity

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P01 | Dispatch: turn one ask into a worker in its own worktree, with model and effort chosen from rules, not by the developer | 1,143 uses, last Sun 2026-11-01 `[cap]`; 441 cards landed in four weeks | T4.2 (spec T1.1, T1.3) | `agents::spawn::spawn_creates_worktree_pane_and_run` | keep |
| P02 | Model and effort rules: four dispatch rules (haiku low, opus high, opus medium, default sonnet medium) choose for every task; never downgrade without the developer's permission | every dispatch: Firstmate's spawn refuses to start a worker without a harness resolved from the rules, so every spawn counted in P01 passes through them; the resolver script was also run directly 81 times, last Sun 2026-11-01 `[scan]`; rules in the bundle's `config.json` | T4.2 | `agents::spawn::dispatch_resolves_model_effort_from_rules` | keep |
| P03 | Lane capacity: refuse a new lane when the lane count (7), free RAM (3 GB) or free disk (40 GB) is short; one project is capped at 2 lanes | 2 direct uses, last Sun 2026-11-01 `[cap]`, but it gates every spawn; host froze and disk filled before limits existed `[reg]` | T4.2 | `agents::spawn::two_spawns_cannot_oversubscribe_capacity`, `agents::spawn::spawn_refuses_below_disk_floor` | keep |
| P04 | Quota floor: read the Claude usage windows before a batch; stop dispatch under the floor | 13 manual `quota-axi` uses, last Sun 2026-11-01 `[cap]`; no floor configured, open question Q10 (3% hard floor, 20% warning) | T4.2, feed in T1.3 and T2.8 | `agents::spawn::spawn_refuses_below_quota_floor` | keep |
| P05 | Fresh clones: fast-forward the project's default branch before work starts, and after a direct push | 405 uses, last Sun 2026-11-01 `[cap]` | T4.2 | `agents::spawn::spawn_starts_from_current_origin_default_branch` | keep |
| P06 | Worktree pool through treehouse | 402 uses, last Sun 2026-11-01 `[cap]` | T4.2 | `agents::spawn::spawn_takes_treehouse_worktree_and_returns_it_on_cleanup` | keep |
| P07 | Promote a scout to a ship in the same worktree when the developer says go | 68 uses, last Sat 2026-10-31 `[cap]`; `scout-completion` skill 14 loads `[scan]` | T4.2 (spec T1.1: card kinds and follow-ups) | `agents::spawn::promote_scout_reuses_worktree_as_ship_run` | keep (gap) |

### Delivery and landing

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P08 | `no-mistakes` delivery: a ship card is done only when the pipeline has run and a PR exists. The orchestrator checks this itself and does not take the worker's word | 677 uses, last Sun 2026-11-01 `[cap]`; `validation-supervision` skill 17 loads `[scan]`; workers reported done with no pipeline three times `[reg]` | T2.7 (column derivation), T4.1 (builder role) | `reconcile::columns::ship_card_stays_working_without_pr` | keep |
| P09 | Ask-user gate escalation: a pipeline ask-user finding goes to the developer, never answered by a worker | `ask-user-authority` skill 19 loads, last Sun 2026-11-01 `[scan]` | T3.3 (attention item), T4.1 | `reconcile::attention::nm_ask_user_finding_becomes_attention_item` | keep |
| P10 | Project registry and delivery mode: per project, PR or straight to the default branch, which checks apply, and lighter checks for no-logic changes | 123 uses, last Sun 2026-11-01 `[cap]`; `project-management` skill 12 loads `[scan]`; `[rule]` Thu 2026-10-08 and Mon 2026-10-05; forge detection at project add, 7 uses `[scan]` | T2.1 (project record), T2.4 (import of the registry) | `core::project::delivery_mode_round_trips_from_import` | keep |
| P11 | No-PR delivery: the worker runs the project's own checks, pushes to the default branch, the orchestrator syncs, then the deploy build is watched to its result | `[req]` mem-41; `[rule]` Mon 2026-10-05 for three projects; deploy-build skill 7 loads `[scan]` | T4.3 (land), T2.7 (watch the deploy) | `agents::land::direct_push_runs_project_checks_then_pushes` | keep (gap) |
| P12 | Land a PR on the developer's word: re-check live checks and head SHA, merge, record the outcome. Local-only projects merge without a forge | 627 uses, last Sun 2026-11-01 `[cap]`; `ship-landing` skill 24 loads `[scan]`; local merge 18 uses, last Mon 2026-10-19 `[scan]` | T4.3 | `agents::land::land_rechecks_head_sha_before_merge`, `agents::land::local_only_project_merges_without_forge` | keep |
| P13 | Standing merge grant: on most projects, merge green in-scope PRs without asking; a few projects always wait for the word | `[rule]` Tue 2026-10-06, narrowed Wed 2026-10-21 | T4.3 (spec T1.4: a standing grant as a bound approval scoped per project) | `agents::land::standing_grant_merges_only_green_in_scope_prs` | keep (gap) |
| P14 | PR and CI watching: PR checks, review state and merges outside the fleet wake the orchestrator | `fm-pr-check.sh` 305 uses, last Sun 2026-11-01 `[scan]`; PR events are one of the wake sources in the watch row `[cap]` | T2.7 (PRs in the reconcile loop), T2.5 (pollers) | `reconcile::pr::ci_green_event_wakes_orchestrator_once` | keep |
| P15 | Promises tied to an event: "report it ready once CI passes" is a record that the event retires | `[com]` 1 open promise on a new project's PR (Sun 2026-11-01) | T2.7 (spec T1.1: obligations as records) | `reconcile::obligations::ci_green_retires_report_promise` | keep |
| P16 | Standing duty after each merge: tag the next pre-release on one public project, until the developer says stop | `[com]` standing since Fri 2026-10-23; `[rule]` Fri 2026-10-23 | T4.1 (orchestrator follows memory rules), memory in T1.5 | `evals/orchestrator_tags_prerelease_after_merge` | keep |
| P17 | PR body scrub before posting: no quoted chat from the developer, no internal tooling text, no stale pipeline claims | `[rule]` learnings.md:19; a public PR leaked a raw ask on Sun 2026-10-25 `[reg]` | T4.3 (check before the PR is opened or merged) | `agents::land::pr_body_with_quoted_ask_is_refused` | keep (gap) |

### Supervision, wakes and liveness

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P18 | Watch workers and wake the orchestrator on status changes, PR events and timers, so the developer can walk away | 8,212 uses, last Sun 2026-11-01 `[cap]`; 1,949 supervision wake turns and 1,928 task-notification turns | T2.5 (outbox), T2.6 (observations), T2.8 (daemon) | `daemon::wake::worker_status_change_wakes_orchestrator` | keep |
| P19 | Scheduled wakes and check-ins: a worker's dated pause (`paused ... until <date>`) and a hold's re-ask date bring the card back on that date, with no alarm before | `[req]` mem-47; `[reg]` missed doorbell, Fri 2026-10-30; 14 dated holds `[cards]`, latest re-ask Tue 2026-11-17 | T2.7 (spec T1.1: `until` on runs and holds) | `reconcile::wake::dated_pause_silences_alarms_until_date` | keep |
| P20 | Worker outcome reports: a worker's done, blocked or decision reaches the orchestrator once, without flooding the dialog | 2,897 uses, last Sun 2026-11-01 `[cap]` | T2.6, T2.5 | `observe::outcome::worker_done_reaches_orchestrator_once` | keep |
| P21 | One owner per task: after a restart, exactly one supervisor steers each task | 418 uses, last Sun 2026-11-01 `[cap]` | T2.7 (spec T1.1: run ownership) | `reconcile::ownership::one_owner_per_run_after_restart` | keep |
| P22 | Read a worker's state from records, with a bounded pane tail | 3,124 uses, last Sun 2026-11-01 `[cap]` | T2.2 (`tiller` card and run reads), T3.2 (Agents view) | `cli::card::show_reports_run_state_and_freshness` | keep |
| P23 | Steer a running worker durably, even mid-turn; detect an idle worker with unread messages and ring again | 785 uses, last Sun 2026-11-01 `[cap]`; 17 of 50 replay asks were follow-ups; `[req]` mem-47; `[reg]` missed doorbell | T2.5 | `delivery::outbox::idle_pane_with_unread_message_is_rung_again` | keep |
| P24 | Agent control: interrupt, restart or relaunch a stuck worker with a progress note | 73 uses, last Sun 2026-11-01 `[cap]`; `stuck-crewmate-recovery` skill 9 loads `[scan]` | T2.2 (command), T2.7 (detection) | `agents::control::relaunch_resumes_worker_in_own_worktree_with_note` | keep (gap) |
| P25 | Reboot recovery: every worker comes back in its own worktree. No agent ever answers the external-import trust prompt for the developer | `[req]` mem-46; `[reg]` Tue 2026-10-06 and Sun 2026-11-01; `harness-adapters` skill 26 loads, last Sun 2026-11-01 `[scan]` | T2.7, T2.5 | `reconcile::recovery::reboot_resumes_workers_in_own_worktrees` | keep |
| P26 | Orchestrator never goes idle with work in flight (Firstmate's turn-end guard) | Stop hook, fires on every turn end `[cap]` | T4.4 (Stop hook in observe mode), T2.5 (daemon wakes the idle orchestrator) | `agents::intake::idle_orchestrator_is_woken_by_pending_outbox` | keep |
| P27 | The supervisor knows the developer's latest words (Firstmate's main dialog mirror) | 1,304 mirror turns `[cap]` | T4.4 (every developer message is an ask record) | `agents::intake::every_prompt_records_an_ask` | keep |
| P28 | Repair outcomes that look finished but were never recorded | 15 uses, last Fri 2026-10-23 `[cap]` | T2.7 | `reconcile::recovery::merged_pr_without_record_closes_card` | keep |

### Holds, decisions and away mode

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P29 | Captain holds: park a card on a question with a reason. Nothing closes while a question is open | 354 uses, last Sun 2026-11-01 `[cap]`; 162 held cards `[cards]`: 135 held for the developer (P30, P31) and 27 blocked-by (P32); `captain-hold-lifecycle` skill 9 loads `[scan]` | T3.3 (spec T1.1: attention and decision states) | `reconcile::holds::card_cannot_close_while_decision_open` | keep |
| P30 | Dated holds re-ask on their date | 14 of the 135 developer holds are dated `[cards]`; `[com]` 2 open (one due Sun 2026-11-01, the day of the export; one Sat 2026-11-07) | T2.7, T3.3 | `reconcile::holds::dated_hold_resurfaces_on_its_date` | keep |
| P31 | Undated holds wait for the developer's word and never nag | 121 of the 135 developer holds are undated `[cards]`; `[com]` 1 open (a release held for the developer's explicit word); open question Q9 | T3.3 | `board/attention.test.ts: undated hold shows under needs and never alarms` | keep |
| P32 | Blocked-by dependencies: a card starts when the card it waits on lands or reports | 27 blocked-by holds `[cards]`, 25 of them since done; `[com]` 2 open at export (T0.2 waits on T0.1; T0.6 waited on T0.4, which closed when T0.4 reported) | T2.7 (spec T1.1: dependencies) | `reconcile::deps::blocked_card_queues_when_dependency_lands` | keep |
| P33 | An investigation that ends with choices files its own held decision | `[req]` mem-42 | T4.1 (scout role handoff format) | `evals/scout_with_choices_files_a_decision` | keep |
| P34 | Away mode (AFK): leave with standing orders and a spend cap, get a return brief | 9 uses, last Sat 2026-10-31 `[cap]`; 5 contracts; `[com]` the last one closed when the developer returned | T1.4 (an away grant scoped to the contract), T4.2 (spend cap), T4.5 (return brief) | `agents::handoff::away_contract_caps_spend_and_writes_return_brief` | keep (gap) |
| P35 | Recap and walk through unanswered decisions on request (`/ahoy`) | typed 7 times, last Sat 2026-10-31 `[scan]` | T3.3 (attention inbox), T4.5 | `board/attention.test.ts: inbox lists every open decision with its evidence` | keep |
| P36 | Notify the developer only when something new needs them: a PR ready for their merge, or a new decision. Routine turns never notify. Opening the notification jumps to the orchestrator | `[rule]` Wed 2026-10-21; herdr notification commands 51, last Sun 2026-11-01 `[scan]` | T2.5 (herdr adapter sends it), T3.3 (what counts as new) | `delivery::notify::new_attention_item_notifies_once` | keep (gap) |
| P37 | Refused actions: when the session's permission check refuses something the developer approved, do not retry; hand them one runnable command | `[reg]` Sun 2026-11-01 | T4.1 (orchestrator role) | `evals/refused_action_hands_back_one_command` | keep |

### Status surfaces and session life

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P38 | Fleet snapshot: one structured read of every project, card, hold and lane | 75 snapshot uses, last Sat 2026-10-31 `[scan]`; feeds the T0.5 demo until cutover | T2.8 (boot snapshot), T2.2 | `daemon::api::snapshot_lists_every_open_card_and_hold` | keep |
| P39 | One-screen fleet board on request (`/bearings`) | 177 uses, last Sat 2026-10-31 `[cap]`; typed 33 times `[scan]` | T3.2 | `board/board.test.ts: all projects view shows every open card` | keep |
| P40 | Every update names its project first and groups by project. Every stage change of a build is reported, not only PR open and merge | `[rule]` Thu 2026-10-08 (both) | T4.1 (orchestrator reply shape), T2.7 (stage events) | `evals/orchestrator_reports_each_stage_by_project` | keep |
| P41 | Stow and session start: save context at breakpoints, restore the fleet picture after `/clear` or a reboot | 45 uses, last Sun 2026-11-01 `[cap]`; `/stow` typed 22 times `[scan]` | T4.5 | `agents::handoff::fresh_orchestrator_resumes_from_handoff_record` | keep |
| P42 | Lean startup memory: always-loaded memory stays under a budget (7,500 tokens today) | 91 uses, last Sun 2026-11-01 `[cap]`. Tiller needs the outcome, not Firstmate's script | T1.5, T4.1 | `agents::roles::startup_context_stays_under_budget` | keep |
| P43 | Startup health checks: missing or outdated tools, network checks, a validation daemon older than its CLI | deferred startup checks 21 uses, last Sat 2026-10-31 `[scan]`; `[reg]` daemon and CLI mismatch, Mon 2026-10-05 | T2.8 (health endpoint), T3.4 (degraded states) | `daemon::health::reports_missing_and_outdated_tools` | keep (gap) |
| P44 | Fleet activity ledger: dispatch, cleanup and outcome events for metrics | ledger on since about Tue 2026-10-20; `task.dispatched` and `task.cleaned_up` events `[cap]` | T2.1 (events written in the same transaction) | `core::events::every_mutation_writes_its_event` | keep |
| P45 | Outcome metrics: count what landed | 3 uses, last Sun 2026-11-01, all in one task `[cap]` | T3.5 (Health page), T7.4 | `board/health.test.ts: shipping counts landed cards from state` | keep |
| P46 | Published PRs and issues per project (contributions view) | 19 uses, last Fri 2026-10-30 `[cap]` | T3.2 (card detail and filters) | `board/board.test.ts: project filter lists published PRs` | keep |

### Review pages and artifacts

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P47 | Live review pages: hand the developer a live link to see and annotate a mock or report; the hosting worker owns the feedback rounds | 527 uses, last Sun 2026-11-01 `[cap]`; `[rule]` Sat 2026-10-10 and Sun 2026-10-18; `[req]` mem-42 | T4.1 (worker role), T2.5 (a poller source for page answers) | `delivery::sources::page_answer_reaches_owning_worker` | keep (gap) |
| P48 | Review listeners belong to a task and are retired at its cleanup; a respawn storm trips a rate limit, not the host | `[req]` mem-43; `[reg]` host froze Sun 2026-11-01 | T4.2 (cleanup), T2.5 | `agents::spawn::cleanup_retires_task_review_listeners` | keep |
| P49 | The review server always starts with the hostname the developer uses | `[reg]` 403 on every link, Sun 2026-10-11 | T4.2 (launch environment for workers) | `agents::spawn::worker_env_sets_review_allowed_hosts` | keep |
| P50 | Pages index: every page or mockup made for the developer stays findable later | `[rule]` Sat 2026-10-10; 122 index rows, last Sun 2026-11-01; 103 index writes `[scan]` | T3.2 (card detail lists its pages; spec T1.1: an artifact record) | `board/card.test.ts: card detail lists its published pages` | keep (gap) |

### Workers and cleanup

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P51 | Clean up a finished task: return the worktree, close the pane, archive the card; never tear down unlanded work | 595 uses, last Sun 2026-11-01 `[cap]` | T4.2 (`tiller done`, cleanup) | `agents::spawn::cleanup_refuses_with_unlanded_commits` | keep |
| P52 | Reclaim build output from worktrees no live task owns | `[reg]` disk filled three times, Wed 2026-10-21 | T6.1, T6.3 (each cutover confirms the dotfiles' daily slot sweep and Nix garbage collection, which do this on host B today, still run). T4.2's disk floor (P03) is the backstop | `live check at CP7: the daily slot sweep still runs on each host after cutover` | keep |
| P53 | Cleanup of a task with no card leaves no marker that fails later | `[req]` mem-56; `[reg]` Sun 2026-11-01 | T4.2 | `agents::spawn::cleanup_without_card_leaves_no_marker` | keep |
| P54 | Workers resolve conflicts with a merge or the pipeline's reconcile path, never a hand rebase of published commits | `[reg]` Mon 2026-10-12 and Sun 2026-11-01 | T4.1 (builder role) | `evals/builder_merges_default_branch_instead_of_rebasing` | keep |
| P55 | A pipeline fix agent cut at 30 minutes loses no partial work; the run reports the cut | `[reg]` recurring through October | T2.7 (run state shows the cut), T4.1 | `reconcile::runs::cut_fix_agent_reports_partial_work` | keep |
| P56 | Screenshot steps write to files, use a kill timeout and kill only their own browser | `[reg]` Sun 2026-11-01 | T4.1 (builder role brief rules) | `evals/screenshot_brief_finishes_inside_timeout` | keep |
| P57 | Private state directories whatever the umask | `[reg]` Tue 2026-10-06 | T2.1 (state directory creation) | `core::state::state_dir_is_private_under_umask_002` | keep |

### Guards

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P58 | The orchestrator stays in its home and never edits project code (cd guard; the first mate never does the work itself) | PreToolUse hook; 27 "blocked by" refusals, last Sun 2026-11-01 `[cap]` | T4.3 (spec T1.4) | `agents::guards::orchestrator_edit_outside_home_is_denied` | keep |
| P59 | No unsupervised subagents from the primary session | PreToolUse hook, same 27 refusals `[cap]` | T4.3 (spec T1.4) | `agents::guards::orchestrator_subagent_spawn_is_denied` | keep |

### Inbox, tools and skills

| # | Capability | Evidence | Owner | Test | Decision |
|---|---|---|---|---|---|
| P60 | Out-of-band notes: scripts and the developer drop a note for the orchestrator. A weekly dotfiles job writes here, so cutover must repoint it | 50 uses, last Sun 2026-11-01 `[cap]` | T2.2 (`tiller` ask intake from a script), T1.3 | `cli::ask::note_from_script_creates_ask` | keep |
| P61 | herdr: every worker lives in a pane the developer can watch | 601 uses, last Sun 2026-11-01 `[cap]` | T2.5 (adapter, layout) | `herdr::layout::worker_workspace_nests_under_its_orchestrator` | keep |
| P62 | Remote lanes through herdr: watch host B's panes from host A | Firstmate's remote lanes: 0 uses `[cap]` (0 saved machines on host B). The developer does reach host B's herdr through herdr's own machine support (a card fixed a notification's Open button that could not find host B's machine, Wed 2026-10-21 `[cards]`) | T3.2 ("Open in herdr" through the saved machine), T3.1 (Machines) | `board/agents.test.ts: open in herdr switches to the saved machine first` | keep (view only, per the plan's default) |
| P63 | Browser checks through chrome-devtools-axi before a UI change is called right | 134 uses, last Sun 2026-11-01 `[cap]` | T4.1 (role tool list) | `agents::roles::builder_tools_include_browser_cli` | keep |
| P64 | GitHub work through gh-axi | 632 uses, last Sun 2026-11-01 `[cap]` | T4.1, T4.3 | `agents::roles::roles_use_gh_axi_not_gh` | keep |
| P65 | Firstmate skills the first mate loads, as Tiller behavior. Each one maps to its Tiller row: `harness-adapters` 26 (P25), `ship-landing` 24 (P12), `ask-user-authority` 19 (P09), `validation-supervision` 17 (P08), `scout-completion` 14 (P07), `project-management` 12 (P10), `diagnostic-reasoning` 11 (P66), `process-event-sources` 11 (P47), `captain-hold-lifecycle` 9 (P29), `stuck-crewmate-recovery` 9 (P24), `bearings` 8 (P39), `stow` 5 (P41), `session-start-recovery` 4 (P41), `afk` 3 (P34), `firstmate-self-repo` 2 (D14), `ahoy` 1 (P35) | `[scan]` skill loads, last Sun 2026-11-01 | T4.1 (orchestrator role carries the behavior, not the skill files) | `evals/orchestrator_role_covers_firstmate_skill_triggers` | keep |
| P66 | Diagnose before fixing: scope a reported bug and reproduce it before acting on a report | `diagnostic-reasoning` skill 11 loads, last Tue 2026-10-27 `[scan]` | T4.1 (orchestrator and scout roles) | `evals/scout_reproduces_bug_before_proposing_fix` | keep |
| P67 | The developer's own skills stay callable from Tiller's roles: publishing links, deploy builds, installs through the dotfiles, PR screenshots, reply style and others | `[scan]` 6, 7, 6, 3 and 3 loads, last Sun 2026-11-01. They live in the dotfiles, not Firstmate | T4.1 (role launch keeps user skills loaded) | `agents::roles::role_startup_lists_user_skills` | keep |
| P68 | Phone channel: Remote Control in the Claude app is on for the developer's own sessions and off for workers | `[rule]` Mon 2026-10-05, Fri 2026-10-16 | T4.2 (launch form per role) | `agents::spawn::worker_launch_has_remote_control_off` | keep |

### Proposed drops (for CP1)

Each row waits for the developer's approval at CP1. A drop sends the capability nowhere: Tiller does not build it.

| # | Capability | Evidence | Reason to drop | Decision |
|---|---|---|---|---|
| D01 | Secondmates and remote homes | 0 uses since Tue 2026-10-06 `[cap]`; 1 incidental read, Wed 2026-10-28 `[scan]` | Never used on host B. Tiller's per-project orchestrators and the per-host daemon cover the same need. Recheck against host A's bundle | drop? |
| D02 | Backlog handoff between homes | 0 uses `[cap]` | Never used. One owner host per project (plan default) | drop? |
| D03 | Orca and Codex app backends | 0 uses `[cap]` | Never used. Tiller runs on herdr only | drop? |
| D04 | Other harness adapters (six non-Claude harnesses) | 0 uses `[cap]`; harness detection 14 uses, all reporting Claude `[scan]` | Tiller is Claude Code only in v1 | drop? |
| D05 | Quota-array dispatch and the usage warner | 0 uses `[cap]` | Never used. P04 keeps the quota floor and T3.5 shows the quota | drop? |
| D06 | Quiet mode | 0 uses: its script never ran and its skill never loaded `[cap]`; the developer typed `/quiet` once, Sun 2026-10-18 `[scan]` | Away mode (P34) covers the developer's real use | drop? |
| D07 | X (Twitter) follow-ups and mail | 0 uses `[cap]` | Never used | drop? |
| D08 | Public follow-up collection | 0 uses `[cap]` | Never used | drop? |
| D09 | herdr project groups and lab sessions | 0 uses `[cap]` | The plan picks nested workspaces, not herdr groups (herdr layout rules) | drop? |
| D10 | Approval failover | 0 uses `[cap]` | Never used. Approvals are bound records in state.db | drop? |
| D11 | Config push to secondmate homes | 0 uses `[cap]` | Follows D01 | drop? |
| D12 | Pipeline spend accounting | 0 uses; flag present but empty `[cap]` | Never used. T3.5 reads the quota feed instead | drop? |
| D13 | Watcher arm guard | PreToolUse hook `[cap]` | It guards a Firstmate mechanism (the orchestrator arming its own watcher) that Tiller does not have: the daemon owns watching. P58 and P59 keep the other two guards | drop? |
| D14 | Self-update of Firstmate | 3 uses, last Sat 2026-10-31 `[cap]` | Firstmate-only, like work on the Firstmate repo itself (`firstmate-self-repo`). Tiller updates through the Nix flake and the dotfiles | drop? |
| D15 | Mac deploy helper | 40 uses, last Wed 2026-10-28 `[cap]` | It belongs to the dotfiles, not the orchestrator. It stays on PATH and P67 keeps it callable; Tiller builds nothing for it | drop? |

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
