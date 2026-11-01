# Export this Firstmate home into a Tiller import bundle

You are the first mate of this Firstmate home. The captain is moving to a new system, Tiller. Tiller starts on day one with no history of its own, so everything this home knows has to reach it through one import bundle that you write now.

**Your job:** produce that bundle completely and faithfully, with provenance on every item, and change nothing in this home or its projects while you do it.

**Who does what:**
- Scripts and shell tools do the mechanics: copying, parsing, counting, checksums.
- You do the judgment: classifying memory, writing summaries, writing the handoff letter.
- When a record exists, copy it verbatim. Summarize only where a section below asks for a summary.

## Settings

Set these first, then echo them back in your opening line.

```sh
MODE=dry            # dry: a rehearsal any time. final: only after the captain freezes dispatch at cutover.
HOST_LABEL=<short label for this machine, as the captain names it>
FM_HOME=<this home's root>
STAMP=$(TZ=Europe/Lisbon date +%Y%m%d-%H%M)
EXPORT_DIR="$HOME/.tiller/import/${HOST_LABEL}-${STAMP}-${MODE}"
```

- Make `EXPORT_DIR` fresh on every run. Never write into an earlier bundle.
- Show every date in Europe/Lisbon. Write dates as `YYYY-MM-DD`, and datetimes as ISO 8601 with the offset.

## Ground rules

1. **Read-only everywhere outside `EXPORT_DIR`.**
   - Your only writes go into `EXPORT_DIR`, plus the scout worktree for Part C if you spawn one.
   - Leave the backlog, holds, captain records, learnings, config, state, panes, worktrees, branches and PRs exactly as they are.
   - Before starting, capture `git -C "$FM_HOME" status --porcelain` and a checksum list of `data/` and `config/`. Prove both are unchanged at the end.
2. **Dispatch continues as normal during a `dry` run.** Freezing dispatch and draining in-flight work are cutover steps the captain starts separately. Mention them only in `CUTOVER.md`.
3. **Secrets stay where they are.**
   - Open no secret-bearing file: `.env`, `.envrc`, key files, `*.pem`, `*.key`, vault output, token stores.
   - When a record names where a secret lives, keep the reference, set `"secret_ref": true`, and copy no value.
   - Part G includes a scan that must come back clean.
4. **Work data stays on its machine.** If this home serves an employer's work, the bundle stays on this machine and is never copied to personal infrastructure. On every host the bundle is local-only: never commit it, push it or attach it anywhere, because it holds verbatim transcripts and quotes.
5. **Provenance on everything.** Every exported item carries `source.file`, plus `source.line` or a record id, and the date the source gives.
6. **Unsure means ask.** When you cannot map something with confidence, write it to `open-questions.md` with the source and your best two readings. Never guess silently.

## Inputs

Read these. Use this home's own tools wherever they exist.

| Input | How to read it |
|---|---|
| Projects | `data/projects.md` |
| Backlog | `bin/fm-fleet-snapshot.sh --json` for parsed records (state, hold bucket, captain_actionable, repo, PR, priority, since), plus `data/backlog.md` for line numbers and raw hold payloads |
| History | `data/done-archive.md` |
| Captain records | `data/captain.md` |
| Learnings | `data/learnings.md` (tier markers like `<!--a:YYYY-MM-DD-->` carry dates) |
| Archives | `data/note-archive.md`, `data/memory-archive.md` (if present) |
| Per-task records | every `data/<task-id>/` directory: brief, launch brief, final report, PR body, review findings, media |
| Config | every file in `config/`, symlinks resolved, values copied |
| State | `state/decision-bindings`, `state/afk-contracts`, `state/when`, `state/operational-inbox`, `state/terminal-outcomes`, per-task `state/<task-id>.*` entries, and `state/fleet-ledger.jsonl` if present |
| Live | `herdr api snapshot` (local server, plus `--machine <id>` for any saved machine this home dispatches to), treehouse worktree list, `gh-axi` for every open PR this home tracks |
| Incidents | `AGENTS.md`, `VISION.md`, `docs/` (only for the regressions list in Part E) |
| Harness memory | the Claude Code auto-memory directory for this home, if one exists |
| Transcripts | the Claude Code session transcripts for this home (`~/.claude/projects/<this home's path slug>/*.jsonl`), only for Part C |

## The bundle (schema `tiller-import/v1`)

```
EXPORT_DIR/
  manifest.json          # written last
  README.md              # one-page human summary of the bundle
  projects.json
  cards.jsonl
  inflight/<card-id>.json
  inflight/<card-id>.brief.md
  tasks/<card-id>/...    # copied per-task records
  memory.jsonl
  memory/rules.md
  memory/notes.md
  memory/requirements.md # Firstmate mechanics the new system must reproduce
  capabilities.md        # every Firstmate capability the captain actually uses, with evidence
  config.json
  commitments.jsonl
  regressions.md
  handoff-letter.md
  baseline/              # Part C, optional
  open-questions.md
  CUTOVER.md
```

---

### Part A: records (mostly mechanical)

**`projects.json`**: an array with one object per project:

```json
{"id":"alpha","repo":"owner/name","remote":"<git remote URL>","fm_checkout":"<path, recorded only>",
 "default_branch":"main","delivery":"no-mistakes|no-mistakes-prod-only|direct-pr|local-only",
 "capacity":4,"owner_host":"<HOST_LABEL>","added":"2026-10-25","description":"<verbatim from projects.md>",
 "release_flow":"<from learnings, verbatim quote or null>","source":{"file":"data/projects.md","line":5}}
```

**`cards.jsonl`**: one line per backlog record, from the current backlog and the done archive alike.

```json
{"id":"<task id>","project":"<project id, or \"fleet\" for home-level work>","title":"<verbatim title>",
 "column":"asked|queued|working|needs|landing|done","kind":"ship|scout|captain|other","priority":0,
 "since":"YYYY-MM-DD",
 "hold":{"kind":"captain|date|blocked|credential|other","until":"YYYY-MM-DD or null","reason":"<decoded hold text, verbatim>"},
 "pr":"<url or null>","report":"tasks/<id>/<file> or null",
 "runs":[{"started":"<datetime or null>","ended":"<datetime or null>","model":"...","outcome":"landed|reported|failed|abandoned|respawned|unknown","pr":"<url or null>"}],
 "depends_on":[],"category":null,"size":null,
 "done":{"outcome":"merged|reported|answered|dropped|cancelled|failed","at":"YYYY-MM-DD"},
 "source":{"file":"data/backlog.md","line":15}}
```

- Set `hold` and `done` to `null` when they do not apply.
- List every attempt in `runs`, including failed, abandoned and respawned ones, from the task's records and the fleet ledger. A card with one clean run has one entry.
- Leave `category` and `size` as `null` unless a record states them. Do not estimate them.
- Decode `fm-hold-v1:<base64>` payloads and keep the decoded text verbatim.
- Map columns like this:

| Firstmate state | Column |
|---|---|
| In flight with a live worker | `working` |
| In flight with an open PR whose pipeline or CI is running | `landing` |
| Waiting on the captain (a decision, a credential, a merge with authority off) | `needs` |
| Dated or blocked hold | `queued` (keep the hold) |
| Done or archived | `done` |

**`tasks/<card-id>/`**: copy each `data/<task-id>/` directory, with three exceptions:

- Skip scratch fake homes (any `home/` subdirectory) and empty files.
- Skip any file a ground rule excludes.
- Copy media too. If the bundle would pass 500 MB, copy text only and list the media with its size and original path in `README.md`.

**`config.json`**:

```json
{"dispatch":<crew-dispatch.json verbatim>,"lane_capacity":<parsed>,"project_capacity":<parsed>,
 "startup_memory_budget":<number>,"quota_floor":{"provider":"claude","percent":3,"source":{...}},
 "notify":{"script":"<resolved path>","managed_by":"dotfiles|local"},"herdr":{"<config key>":"<value>"}}
```

**`commitments.jsonl`**: everything this home has promised to do later. Sources: scheduled wakes in `state/when`, open AFK contracts, dated holds, "I'll report back when..." promises in recent threads, and `operational-inbox` items still open. Fields:

```json
{"what":"...","due":"YYYY-MM-DD or null","card":"<id or null>","source":{...}}
```

### Part B: live work

For each card whose column is `working`, `landing` or `needs` and that has a live worker or an open PR, write `inflight/<card-id>.json`:

```json
{"id":"...","kind":"...","project":"...","model":"...","effort":"...",
 "herdr":{"machine":"local|<saved machine id>","workspace":"<label>","tab":"<label>","pane_id":"...","agent_status":"<from snapshot>"},
 "worktree":"<path>","branch":"...","base":"...","pr":"<url or null>","pipeline":"<no-mistakes run state or null>",
 "last_status":"<the worker's latest status line, verbatim>","started":"<datetime>",
 "recommendation":"drain|adopt|respawn","why":"<one sentence>"}
```

Copy the task's brief verbatim to `inflight/<card-id>.brief.md`, and append a short "State at export" section: what is done, what remains, and the next step.

- **Recommend `drain`** when the task can finish in Firstmate before cutover.
- **Recommend `adopt`** when Tiller should take over the live pane and worktree as they are.
- **Recommend `respawn`** when a fresh worker in the same worktree with the brief is cleaner.

### Part C: historical baseline (optional, one scout)

This part gives Tiller a "before" to compare against, because it will not run side by side with Firstmate. Run it only when `MODE=dry`, or when no dry-run baseline exists yet. Hand it to one scout through this home's normal scout path, using the model and effort the dispatch rules pick for an investigation. The scout writes only into `EXPORT_DIR/baseline/`.

The scout produces:

1. **`baseline/output.json`.** Weekly landed cards per project and kind (merged ships, reported scouts), and median days from `since` to done. It comes from `cards.jsonl`. Plain arithmetic, done with a script.
2. **`baseline/touches.jsonl`.** For each landed card in the last 21 days, the captain messages about it in the session transcripts. Each line carries a classification (`correction`, `unplanned_escalation_answer`, `routine`) and a verbatim quote.
   - A correction is a captain message that changes direction or points out a mistake.
   - Attribute a message to a card only when the transcript ties it to one: the card id, or an unambiguous title or PR reference nearby. Leave the rest unattributed.
3. **`baseline/summary.md`.** It reports captain touches per landed card (corrections, unplanned escalation answers and routine touches counted separately), landed cards per week, and median time to done. Give each figure its method, sample size and caveats. State the caveats plainly: transcript attribution is approximate, and date-only fields make time to done coarse.
4. **`baseline/replay.jsonl`.** This is the replay set the new system's orchestrator is tested against before cutover. Pick 30-50 captain asks from the transcripts, as varied as possible: ships, scouts, decisions, follow-ups on existing cards, plain chat, and asks that were cancelled or folded into another card. Each line holds:
   - the ask text, verbatim;
   - its timestamp;
   - the cards it created or touched;
   - what this home did with it (new card, follow-up, chat, or cancelled);
   - the kind, model and effort dispatched;
   - any hold placed;
   - the final outcome.

   Prefer asks whose handling the records show clearly. Skip any ask that quotes a secret value.

### Part D: memory (your judgment)

Read every non-empty bullet or paragraph in `data/captain.md`, `data/learnings.md`, both archives and the harness auto-memory. Write one line per entry to `memory.jsonl`:

```json
{"id":"mem-<n>","text":"<verbatim>","scope":"global|project:<id>","section":"<heading it sat under>",
 "class":"rule|note|requirement|stale","tier_marker":"<the tier marker or date, if any>",
 "secret_ref":false,"provenance":"legacy-import","rationale":"<only when the source states why, verbatim; else null>",
 "generalized":"<only for requirement: one sentence a different system can implement>",
 "reason":"<only for stale or requirement: why>","source":{"file":"data/learnings.md","line":12}}
```

How to classify:

| Class | Meaning | Examples |
|---|---|---|
| `rule` | a captain preference that holds for any system | stay on Claude for workers; the PR shape; off-screen testing; the quota floor |
| `note` | a fact about a project, tool or machine that stays true outside Firstmate | gamma has no PR CI; parallel Rust builds overload the Mac |
| `requirement` | a fact about Firstmate's own scripts or flow that names a behavior the new system must reproduce | merge before teardown, so landed-work metadata survives; workers report done before the pipeline starts, so verify a run exists. Fill `generalized` |
| `stale` | no longer true. Give the evidence in `reason`. When unsure, keep the entry as a `note` and add an open question | - |

Never invent a rationale, an origin card or an author. When the source does not say, leave the field `null`.

Then render three files from `memory.jsonl`: `memory/rules.md`, `memory/notes.md` (grouped by scope) and `memory/requirements.md`. Use the verbatim text with a source suffix, for example `(captain.md:7, 2026-10-26)`.

**Completion check:** the count of entries in `memory.jsonl` equals the count of non-empty entries across every source. Print both numbers.

### Part E: regressions

`regressions.md` lists every incident the new system must survive. Sources:

- dated lines in learnings (reboot recovery, stale panes, permission prompts the orchestrator cannot answer, load storms, merge-order mistakes, classifier refusals, screens covered by tests);
- incidents recorded in this home's `docs/` and `AGENTS.md`.

Each entry has five fields:

```
### <short name>
- When: YYYY-MM-DD (source)
- What happened: <two sentences, factual>
- Required behavior: <what the new system must do>
- Check: <eval case, test or hook that would catch it>
- Source: <file:line>
```

**`capabilities.md`**: every Firstmate capability the captain actually uses. It is the input to Tiller's parity matrix. The captain's bar is that the new system must be "comparable" and that their "output will not be diminished", so a missing row here is how output quietly drops. List what you actually do for the captain, not what Firstmate could do. Typical areas:

- holds, dated holds and AFK mode;
- scheduled wakes and check-ins;
- secondmates and remote lanes;
- dispatch, capacity and quota floors;
- delivery through `no-mistakes`, plus PR and CI watching;
- supervision and liveness;
- the fleet snapshot and herdr presentation;
- every skill or axi tool you call on the captain's behalf.

Use one entry per capability:

```
### <capability>
- What it does for the captain: <one sentence>
- Evidence of use: <count over the last 4 weeks, last used YYYY-MM-DD, sources>
- How it works today: <script, doc or skill, with file:line>
- Load-bearing: yes|no, with one sentence on why
```

Leave out anything with no evidence of use in the last 8 weeks, but list those names under a closing **Unused** heading, so nothing is hidden.

End `regressions.md` with a **Spec sources** section. It lists this home's contract documents that define behavior the new system must match. Typical ones cover the hold lifecycle, pre-tool guards, agent control, the subagent guard, the cd guard, the supervision host and the fleet ledger. For each document, give one line naming the contract it defines, with the path relative to the home. Do not copy the documents.

### Part F: the letters

**`handoff-letter.md`.** In your own voice, at most 1,500 words, write what a successor orchestrator needs that the files above do not already say:

- how the captain likes to work with you, and which corrections you have absorbed;
- each project's current state and quirks;
- open threads and promises;
- upcoming dates;
- what you would do first on day one.

Give each claim a source pointer when one exists. Mark opinions as opinions.

**`CUTOVER.md`.** The concrete checklist for this host, in order:

1. Freeze dispatch (the exact command).
2. Drain or hand off each in-flight card, with its estimate.
3. Holds that need the captain's answer first.
4. Run this prompt with `MODE=final`.
5. Stop this home's watchers and services (name each one and how it starts: launchd, login items, herdr config).
6. Repoint everything that references this home: dotfiles symlinks such as `captain-notify.sh` and `crew-dispatch.json`, the settings files their hooks rewrite, herdr project groups and presentation settings. Give the file paths found by a read-only `grep` of the dotfiles.
7. How to restart this home if the captain falls back within the first two weeks. Nothing here is deleted.

### Part G: verify, then manifest

Run every check below and show the output in your final message.

- [ ] Every backlog and done-archive record appears exactly once in `cards.jsonl`. Print the counts, plus any ids missing or duplicated.
- [ ] Every in-flight card has both `inflight/<id>.json` and `.brief.md`.
- [ ] Every `report` path in `cards.jsonl` exists in the bundle.
- [ ] The memory count check from Part D passes.
- [ ] `capabilities.md` exists, and every entry has evidence of use and a `file:line` for how it works today.
- [ ] `jq empty` passes on every `.json` file, and on every line of every `.jsonl` file.
- [ ] The secret scan of `EXPORT_DIR` finds nothing:
  ```sh
  grep -rIEn 'sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|github_pat_|AKIA[0-9A-Z]{16}|xox[baprs]-|-----BEGIN [A-Z ]*PRIVATE KEY|(api[_-]?key|token|password|secret)[[:space:]]*[:=][[:space:]]*[^[:space:]]{8,}' "$EXPORT_DIR"
  ```
- [ ] `git status --porcelain` and the `data/` and `config/` checksums match the snapshot from the start.
- [ ] `manifest.json` is written last:
  ```json
  {"schema":"tiller-import/v1","mode":"dry|final","host_label":"...","generated_at":"<Lisbon ISO datetime>",
   "firstmate_head":"<git rev-parse HEAD>","counts":{"projects":0,"cards":0,"inflight":0,"memory":0,"regressions":0,"capabilities":0,"open_questions":0},
   "files":{"<relative path>":"<sha256>"}}
  ```

## Done means

All Part G checks pass. Then give the captain one short receipt containing:

- the bundle path;
- the counts;
- the open questions that need them, as a numbered list with your recommendation for each;
- the in-flight cards and your drain, adopt or respawn call for each;
- and whether Part C ran.

Stop there. The import into Tiller is a separate step that Tiller runs.
