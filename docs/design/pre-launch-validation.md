# Pre-launch validation

Design note, Monday 2026-11-02 (Lisbon time). Status: decided by the owner on Tuesday 2026-11-03 (section 9), built the same day (Lisbon time). Section 10 lists where the build had to settle a detail the sections above left open.

A project in the **pre-launch** stage skips per-change validation (the `no-mistakes` pipeline) and gets one batched review a week instead. A **launched** project keeps today's behavior. Switching to launched runs one final batched review first.

Why: per-change validation was about 15-20% of model spend in one measured week, mostly fixed startup cost on small diffs, for projects that have no users yet. One giant end-of-week review is no better, because sessions past 200k context were about half of all spend. So the weekly review is cut into bounded chunks.

## 1. What exists today

| Question | Answer | Evidence |
|---|---|---|
| Can projects hold settings? | Yes, as typed columns, not key-value. `delivery`, `checks`, `capacity`, `deploy_*` and `merge_gate` already live on the row. Edits go through `project.edit`, owner only in the store and in the guard. | `docs/spec/schema.sql:66-87`; `crates/core/src/state/projects.rs:337-344` (`require_owner`); `crates/core/src/store.rs:44-46` (owner = `Owner` or `Board`); `crates/agents/src/guards/bash.rs:1987` (`tiller project` other than `list` is owner-only for every role) |
| Is "skip the pipeline" already a mode? | Yes. `delivery = direct-pr` is exactly "push the branch, open the PR, no pipeline". | `crates/core/src/domain/enums.rs:84-92`; `roles/builder.md:26-27` |
| Do recurring cards exist? | No. A card is `open` or `closed`. The only long-lived card is `fleet-1`, which never closes. | `docs/spec/schema.sql:107-117`; `docs/spec/state.md` (Project, `fleet-1`) |
| Do recurring obligations exist? | Partly. `event`, `date` and `standing` triggers exist. A `standing` obligation re-arms, but only for events. A `date` obligation fires once: the reconcile timer meets it and notes the live orchestrator. There is no repeating date. | `docs/spec/schema.sql:436-452`; `crates/core/src/domain/lifecycle.rs:272-275`; `crates/reconcile/src/recovery/mod.rs:229-241`; `crates/core/src/state/records.rs:102-139` |
| How do reviews work? | One `reviews` row per reviewed run: reviewer kind, coverage, finding counts, verdict. The `reviewer` role is read-only and records its own review. | `docs/spec/schema.sql:469-482`; `roles/reviewer.md`; `docs/spec/authority.md:87` |
| How do grants interact? | A grant may require `pipeline: passed` and named passing reviews. A PR with no pipeline run has `pipeline_state = none`, so a grant requiring `passed` never matches it. | `crates/agents/src/land/grants.rs:164-198`, `:231-235`; `docs/spec/authority.md` section 6 |
| Can a finding point back at its cause? | Yes: `cards.fixes_card_id`, and `prs.card_id` maps a merged PR to its card. | `docs/spec/schema.sql:105`, `:273-298` |

Who may do what, unchanged: only the orchestrator and owner create cards and start runs; the daemon does neither (`docs/spec/authority.md:77-79`).

## 2. Data model

One migration, `0002_project_stage`:

```sql
ALTER TABLE projects ADD COLUMN stage TEXT NOT NULL DEFAULT 'launched'
  CHECK (stage IN ('pre-launch', 'launched'));
ALTER TABLE projects ADD COLUMN reviewed_through_sha TEXT;   -- last commit a batched review covered
ALTER TABLE projects ADD COLUMN stage_changed_at TEXT;

CREATE TABLE review_passes (
  id             TEXT PRIMARY KEY,
  project_id     TEXT NOT NULL REFERENCES projects(id),
  card_id        TEXT NOT NULL REFERENCES cards(id),   -- the pass card (kind scout)
  kind           TEXT NOT NULL CHECK (kind IN ('weekly', 'final')),
  base_sha       TEXT NOT NULL,                        -- = reviewed_through_sha at start
  head_sha       TEXT,                                 -- default-branch head, fixed at start
  chunks         TEXT NOT NULL DEFAULT '[]',           -- JSON plan, section 4
  state          TEXT NOT NULL DEFAULT 'pending' CHECK (state IN
                   ('pending', 'running', 'finished', 'abandoned')),
  report_artifact_id TEXT REFERENCES artifacts(id),
  fix_card_ids   TEXT NOT NULL DEFAULT '[]',           -- JSON
  obligation_id  TEXT REFERENCES obligations(id),      -- the weekly due date that started it
  created_at     TEXT NOT NULL,
  started_at     TEXT,
  finished_at    TEXT,
  rev            INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX review_passes_live ON review_passes(project_id)
  WHERE state IN ('pending', 'running');
```

- **Existing projects** read `stage = 'launched'` from the column default. Nothing changes for them. Rigor is never lowered by a migration.
- **Why a separate `stage` and not `delivery = direct-pr`.** Switching back must restore the exact prior mode (`no-mistakes` or `no-mistakes-prod-only`), and the stage also drives the weekly review. One derived value, `effective_delivery`, is what builders and the land loop read: `pre-launch` with `no-mistakes` or `no-mistakes-prod-only` gives `direct-pr`; every other pair gives `delivery` unchanged. `tiller card show` and the spawn brief print it.
- **Why a new table.** `reviews` is per run and has no commit range. A pass needs its range, its chunk plan and its outcome in one place, and "last reviewed commit" needs a compare-and-set target. Chunk runs still write ordinary `reviews` rows, and the report is an ordinary `artifacts` row.
- Backup, export and the API (`GET /api/v1/projects`) carry the new columns. The Firstmate import sets nothing, so imported projects are `launched`.

## 3. Owner-only stage change

New mutation `project.stage` (`tiller project stage <id> pre-launch|launched --rev <n> [--from <sha>]`, and a board action with a confirm dialog). It is owner only twice over, like every `project.*` command: `require_owner` in the store and the `tiller project` owner-only rule in the guard. No agent path can change the stage, and the daemon never does.

To **pre-launch**, in one transaction: set `stage`; set `reviewed_through_sha` to the default-branch head the CLI read just before (or `--from <sha>`); open the first weekly `date` obligation (`card_id` null, `what = "pre-launch review"`, `due_at` = next slot, section 5); write `project.staged`. The confirmation says that grants' `pipeline` condition is skipped while pre-launch and every other condition still applies (D4).

To **launched**: see section 7.

The board shows the stage as a chip on the project in the sidebar, plus last reviewed commit and date, next review due, and "final review running" while a final pass is open.

## 4. Chunking

`tiller review plan <project>` computes the plan in Rust, deterministically, from git and the `prs` table:

1. Range: `base_sha..head_sha` on the default branch, first-parent only. `head_sha` is the fetched head at pass start. Dates are never used.
2. Units: each first-parent commit maps to its PR (`prs.merge_sha`, else the forge for a PR merged outside Tiller), else a "direct commit" unit.
3. Size: changed lines (added plus deleted), not counting lockfiles, generated files (the repo's `linguist-generated` attributes, plus `bun.nix` and `*.gen.ts` style names) and binaries. Excluded paths are listed in the report.
4. Group by code area first: PRs that touch the same files land in the same group (connected components over changed files), so changes that can interact are read together. Then pack groups, in merge order inside each, into chunks of at most **1,500 changed lines** (hard cap 2,000) and 15 PRs. A group over the cap is split by directory into slices of at most 1,500 lines, each carrying the PR titles and bodies. A single file over the cap is its own chunk, flagged in the report.
5. Bound: about 1,500 diff lines is roughly 25-40k tokens. With a ~50k start and room to read callers, a chunk session stays well under 150k. The chunk brief also says: past ~150k, stop, record what was covered, and the uncovered files become a new chunk. Coverage is never dropped silently.

What a chunk does and does not limit. The chunk limits which diff a reviewer must judge, not what it may read. Every chunk reviewer has the whole repository at `head_sha`, so it reads callers, tests and the final state of every file, including code other chunks changed. Its brief also carries a one-line index of every PR in the pass (number, title, files touched), so it can see that another PR touched what it is reading and open that diff when it matters. What remains is a bug that only shows when two diffs in different chunks are read side by side; grouping by shared files keeps most of those in one chunk, and the summary run checks the rest (below).

Each chunk is one `reviewer` run on the pass card, at the pass head in a read-only worktree, with the chunk's diff and PR list as the brief (D3). It writes the usual `review.md` (`roles/reviewer.md`), records a `reviews` row, and adds it as a `report` artifact. The brief narrows scope to correctness bugs, security issues and regressions: no style, no refactors. Every finding names `file:line` and the PR that introduced it.

Then one **summary** run (sonnet, medium) reads only the chunk reports. It re-anchors each finding at `head_sha` (`git blame`; a line already fixed later in the range is dropped as "fixed in range"), merges duplicates, ranks by severity, and writes the report page. It also takes the list of places where two chunks touched the same function or interface (computed from the plan) and reads those spots at `head_sha` for cross-chunk breakage. Its input is findings and a short list of seams, not whole diffs, so it stays small.

## 5. Weekly schedule

- **Due date.** An ordinary `date` obligation per pre-launch project, with no card. The reconcile timer already meets it when due and notes the project's live orchestrator (`recovery/mod.rs:229-241`, `records.rs:126-138`). The slot is Saturday 06:00 Lisbon time, computed DST-aware (D5).
- **Start.** On the note, the orchestrator runs `tiller review start <project>`. That creates the pass card (kind `scout`) and the `review_passes` row, fixes `head_sha`, stores the plan, and starts chunk runs one after another (`runs_active_role` already allows one active reviewer run per card). Spawn's quota floor and capacity checks apply as usual: below the floor, the pass waits.
- **No orchestrator at due time.** The obligation is still met. `tiller review due` lists met weekly obligations with no pass after them; the orchestrator checks it at session start, and the board shows "review overdue" on the project after 24 hours.
- **Next week.** `review_pass.finish` (orchestrator) checks that every chunk has a `reviews` row and the report artifact exists. In one transaction it advances `reviewed_through_sha` from `base_sha` to `head_sha` (compare-and-set: a mismatch is refused), closes the pass card `reported`, and, when the pass is `weekly` and the project is still pre-launch, opens the next `date` obligation. Scheduling never waits for the owner to read the report.
- **Abandon.** `review_pass.abandon` leaves `reviewed_through_sha` alone, so the next pass covers the same commits again, and opens the next obligation.

## 6. Findings to fix cards

- A finding becomes a fix card when it is `[confirmed]`, `high` or `medium`, and still present at `head_sha`. `plausible` and `low` findings stay in the report only. The owner can file any of them from the report.
- The orchestrator files each with `tiller card create <project> --kind ship`: priority 1 for high, 2 for medium; category `pre-launch-review`; title is the claim; body has `file:line` at the head commit, the introducing PR URL, the failure scenario, the smallest fix and a link to the report. `fixes_card_id` is the introducing PR's card (`prs.card_id`) when Tiller knows it. The idempotency key is `review:<pass id>:<sha256 of path, claim and introducing commit>`, so a retried pass files nothing twice.
- Ranges are contiguous and never overlap, so a later pass never sees the same introducing commit again. Fix PRs are themselves reviewed next week.
- The report page is recorded as an `artifacts` row of kind `page` on the pass card. One `review` attention item on the pass card points the owner at it, with counts. Answering it is optional reading; it gates nothing.

## 7. Launch transition

`project.stage <id> launched` (owner), in one transaction (D2, recommended):

1. `stage = launched`. From this moment `effective_delivery` is the project's real `delivery`, so every push after the switch goes through the pipeline.
2. Cancel the open weekly obligation. A weekly pass already running continues and its result counts.
3. Create the final pass card and a `review_passes` row of kind `final`, `pending`, from `reviewed_through_sha`. A note wakes the orchestrator, which runs `tiller review start` as for a weekly pass. `head_sha` is fixed at start, so it also covers PRs that merged between the switch and the start.
4. The reply lists open PRs with `pipeline_state = none`. The orchestrator sends each back through the gate before it is merged, so nothing unvalidated lands after the switch.

Finishing a final pass advances `reviewed_through_sha` and schedules nothing. The board shows "launched, final review running" until it finishes.

Switching back to pre-launch is allowed, owner only, and starts from the current head.

## 8. Test plan

Stage switch:

- `core::migrate::existing_projects_read_launched_after_0002` on the v1 fixture (`crates/core/src/migrate/fixture_v1.sql`).
- `core::state::project_stage_is_owner_only`: a session actor gets `Forbidden`; owner and board succeed. Guard: `tiller project stage ...` denied with `owner-only` for every role.
- `core::domain::effective_delivery_table`: all 10 `(stage, delivery)` pairs.
- `core::state::stage_to_prelaunch_opens_one_obligation`: one `date` obligation at the next slot, `reviewed_through_sha` set, replay writes nothing, stale `rev` conflicts.
- Board: the stage chip and project panel in both themes at 1440 and 390 px, plus a `views.test.tsx` case.

Recurring schedule:

- `reconcile::weekly_review_due_notes_orchestrator` with a fake clock.
- `core::state::review_finish_advances_and_reschedules`: compare-and-set on `reviewed_through_sha`; the next obligation at the next slot, including across the last Sunday of March and October (Lisbon DST); no obligation after a switch to launched; refused while a chunk has no `reviews` row.
- `agents::review::plan` proptest: every counted changed line lands in exactly one chunk; no chunk over the cap except a lone oversized file; a PR under the cap is never split; PRs sharing a file share a chunk whenever their group fits the cap; the same input gives the same plan.
- `review due` lists a met obligation with no pass after it.

Launch transition:

- `agents::land::grants::pipeline_condition_skipped_only_while_prelaunch`: a `pipeline: passed` grant matches a pre-launch PR with `pipeline_state = none`, and the same grant refuses that PR after the switch to launched.
- `core::state::launch_cancels_weekly_and_opens_final_pass`: one transaction, `effective_delivery` back to `no-mistakes`, final pass `pending` from `reviewed_through_sha`.
- `core::state::final_pass_finish_schedules_nothing`.
- `cli::tests::prelaunch_cycle_end_to_end`: a temp git repo with fake forge, pre-launch, two merged PRs, the due timer, a pass with two chunks, a fix card, the next obligation, then launch and the final pass.

## 9. Owner decisions

The owner picked one option for each on Tuesday 2026-11-03 (Lisbon time). The picks match the recommendations below, and the sections above already describe the picked option.

- **D1:** (a) keep the builder's own `/code-review`, drop only `no-mistakes`.
- **D2:** (a) validation back on at the switch; the final review runs alongside.
- **D3:** (a) one `reviewer` run per chunk, one after another, plus a summary run.
- **D4:** (b) the grant matcher skips the `pipeline` condition while pre-launch and enforces it again at launch.
- **D5:** (a) fixed weekly slot, Saturday 06:00 Lisbon time.

The options as they were presented:

| ID | Choice | Options | Recommendation |
|---|---|---|---|
| D1 | Per-change review in pre-launch | (a) Keep the builder's own `/code-review` before the PR, drop only `no-mistakes`. (b) Drop both. | (a): one in-session pass, cheap next to a 6-7 session pipeline, and grants requiring `code-review` keep matching. |
| D2 | When validation turns back on at launch | (a) At the switch; the final review runs alongside. (b) Only after the final review finishes. | (a): rigor only goes up, the stage stays a pure owner fact, and there is no gap between "final review head" and "validation on". |
| D3 | How a chunk runs | (a) One `reviewer` run per chunk, one after another, plus a summary run. (b) One run that farms chunks to read-only subagents. | (a): each chunk is a recorded run with its own review row, liveness and cut handling; a crash loses one chunk, not the week. Context per chunk is the same either way (section 4). |
| D4 | Grants that require `pipeline: passed` | (a) Literal: they stop matching in pre-launch; the owner adds a second grant without `pipeline` and must revoke it at launch. (b) The matcher skips the `pipeline` condition while the project is pre-launch, and enforces it again from the moment of launch. | (b): under (a), a grant with no `pipeline` condition still matches a PR whose pipeline **failed** (`grants.rs:164-170`; `pipeline_done` only blocks `running`, `land/mod.rs:696`), so a pre-launch grant forgotten at launch would auto-merge failed PRs. (b) ties the condition to the stage the owner set, with nothing to remember. Checks, reviews and path exclusions still apply. |
| D5 | Weekly slot | (a) Fixed slot, Saturday 06:00 Lisbon time. (b) 7 days after the previous pass finished. | (a): the report is ready the same morning each week; a late pass (quota floor, crash) does not push every later one. |

## 10. Settled while building

Details the sections above left open, each settled in the direction that never lowers rigor:

- **The forge read** goes through `gh api`, like the land sequence's forge seam, because the commit-to-PR lookup needs machine-readable replies. `TILLER_GH` names another client for tests.
- **The pass card closes when the owner answers.** `card.close` refuses while an attention item is open, so finishing opens the `review` item and answering it closes the pass card `reported`. `reviewed_through_sha` and the next due date are still written at finish and never wait for the answer (section 5).
- **"Every chunk has a `reviews` row"** is checked per chunk: chunk `n` counts once a live `report` artifact titled `chunk <n> of <total>` is on the pass card, from a run that also recorded a review there, both since the pass started. A re-run of a cut chunk covers that chunk again and the summary run covers none, so neither can stand in for a chunk that never ran.
- **D4 waives only a missing pipeline.** The grant matcher skips the `pipeline` condition while pre-launch only for a PR with no pipeline run (`pipeline_state = none`). A pipeline that ran and failed still blocks, so switching to pre-launch never merges a PR the gate rejected.
- **A weekly review met before a launch** is covered by the final pass: `tiller review due` lists only obligations met since the project last became pre-launch.
- **The stage switch fetches only when it applies.** `tiller project stage` and the board read the default-branch head only for a switch to pre-launch at the current `rev` with a new key; a replay, a stale `rev` or the current stage answers without touching the remote. `tiller review start` refuses before reading the range when nothing is due.
- **A live pass at launch.** At most one pass per project is pending or running. A pending weekly pass is superseded (its card closes `superseded`) and the final pass opens at once; a running weekly pass continues, and its finish or abandon opens the final pass (section 7, steps 2 and 3).
- **Abandoning a final pass** opens a new pending final pass, so the project is never left launched without one.
- **Back to pre-launch** is refused while a pass is pending or running, so the compare-and-set at finish always has the base it started from.
- **Chunk worktrees.** A spawned run's worktree is cut at the default-branch head at spawn time, which is at or after the pass head. The chunk brief anchors every line at the pass head and tells the reviewer to read a moved file with `git show <head>:<path>`.
- **A cut chunk.** A chunk report's "Not covered" files get one more reviewer run, briefed with `tiller review brief <pass> --chunk <n> --only <path>`; the stored plan does not change.
- **Seams** are files changed in more than one chunk; the summary brief lists them.

