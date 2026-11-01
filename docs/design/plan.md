# Plan: per-project agent orchestrators, a live board, and a measured learning loop

Written Sunday 2026-11-01, about 17:30 Lisbon time (WET). This is the research report's plan and recommendations in one self-contained file, so a separate model can judge it. Everything a judge needs is here. Section 15 lists the weak points worth attacking first.

---

## Revision 1 (Sunday 2026-11-01, evening Lisbon): one cutover, not a strangler

**The developer's call.** They do not want to roll off Firstmate slowly. They want the new system (working name Tiller, CLI `tiller`) built to day-one completeness. On day one, a prompt run inside Firstmate exports everything it knows into an import bundle, and Tiller starts from that bundle. This revision supersedes the strangler parts of sections 1, 13, 14 and 15. Everything else stands.

**The export prompt.** It is a separate file: `docs/firstmate-export.md`, beside this one. Later it lives in the new repo as the import contract.

- **It is read-only on the Firstmate home.** It proves that with a before and after `git status` plus checksums of `data/` and `config/`.
- **It writes one bundle** (schema `tiller-import/v1`): projects, every backlog and done record as cards, per-task records, in-flight state, classified memory, config, commitments, regressions, a handoff letter from the first mate, a cutover checklist and a historical baseline.
- **It runs in two modes:** `dry` (a rehearsal any time) and `final` (after dispatch is frozen).
- **It runs once per host.** Work data stays on the work Mac.

**The parity gate is replaced by a historical baseline.** Without a side-by-side run, nothing proves Tiller matches Firstmate before cutover. Instead:

- **A historical baseline.** One scout during the dry run mines Firstmate's done archive and its captain session transcripts (about 75 sessions) for landed cards per week, median time to done and captain corrections per card. That is the "before".
- **A post-cutover check.** Two weeks after cutover, compare the same numbers. If Tiller is clearly worse, restart Firstmate (stopped, never deleted) and fix forward from there.

**Revised phases (agent wall-clock):**

| Phase | Work | Estimate |
|---|---|---|
| 0 (gate 2) | throwaway board demo, unchanged | 60-75 min |
| dry run | run the export prompt with `MODE=dry` in Firstmate now; fix the schema from what it finds | 30-60 min, plus the baseline scout |
| day one | daemon and live board (5-7 h), board actions on Tiller's own state (3-5 h), card-protocol hooks (2-3 h), the `tiller` CLI and the importer (3-4 h), the orchestrator and worker roles with the field-incident guards from `regressions.md` (12-18 h) | about 24-36 h |
| cutover | freeze Firstmate dispatch, drain in-flight work, `MODE=final` export, import, repoint dotfiles, start Tiller; per host | 1-2 h per host |
| after | memory view (5-7 h), then the learning loop (4-6 h), measured against the historical baseline from the start | 9-13 h |

Total build: about 34-50 agent hours, with no calendar wait before cutover. The two-week baseline wait and the per-project pilot are gone.

**Choices made:**

- **Drain in-flight work rather than adopt live panes.** Adopting means Tiller inherits panes it did not spawn and briefs in Firstmate's format. Draining costs at most a day of slower dispatch. The bundle still records live state with an adopt or respawn option for anything that cannot drain.
- **Memory moves as classified text, verbatim.** Rules and notes import as is. Lines about Firstmate's own mechanics become requirements for Tiller, not memory. Stale lines are kept, flagged, with evidence.
- **Firstmate's field incidents become day-one regression tests**, not lessons learned after cutover.

**New weak points (add to section 15):**

1. **Big-bang risk.** Every project moves at once. A bug in the orchestrator role hits all projects on day one, not one pilot. Mitigations: the dry run, the regression list as tests, a fallback that keeps Firstmate restartable for two weeks, and cutting over the personal Mac (host A) first, then the remote Linux host (host B), then the work Mac (host C).
2. **No live parity check.** The historical baseline measures Firstmate under different conditions (different weeks, different work). It is a weak "before". A Tiller regression could hide inside normal week-to-week variance.
3. **Transcript mining is approximate.** Linking captain messages to cards depends on ids or titles appearing nearby. Corrections per card from history will undercount. Compare trends, not absolute values.
4. **The day-one estimate leans on the 12-18 h orchestrator.** Section 15 item 2 still applies, and without a pilot it matters more. A judge could argue for one pilot project for a week even inside a fast cutover.
5. **The export prompt runs in Firstmate's primary session**, where the Agent tool is blocked by a hook. The baseline therefore goes to a scout through Firstmate's normal path, which adds a worker and a few minutes. This was verified in an earlier session.

---

## Revision 2 (Sunday 2026-11-01, late evening Lisbon): after the second opinion

A second model reviewed this plan in `docs/design/second-opinion.md`. The point-by-point answer is in `docs/design/plan-v2.md`, beside this file. That response is now the current plan: its "revised plan (v2)", its fact-ownership table and its fact checks supersede sections 3, 4, 9, 11 and 13 wherever they conflict.

**Kept:**
- one cutover per host;
- replacement over a fork, with Firstmate's MIT specs and scripts reused.

**Added:**
- a capability probe on every host, first;
- SQLite as the authority for workflow state;
- lifecycle facts with columns derived from them;
- outbox delivery;
- approvals bound to the exact action;
- a single-owner marker;
- a recovery table plus a restore drill;
- an offline replay gate;
- nothing loaded into a fleet role until approved;
- a scorecard in place of a single metric;
- read-only remote lanes in v1.

**The estimate is now:** day one 50-70 agent hours, 65-95 in total.

## Revision 3 (Sunday 2026-11-01, evening Lisbon): stack and herdr layout decided

- **Language (the developer's call):** Rust for the core: the `tiller` CLI, the daemon, the hooks, the herdr adapter and SQLite. The board is TypeScript: Vite, React, TanStack Router and Query, built as a single-page app embedded in the binary. It has no SSR, so no JavaScript server. Perfect loads come from a boot snapshot inlined into `index.html`, plus a live SSE stream into the query cache. The details are in `docs/plan/build-plan.md`, under "Stack" and M3. This supersedes the TypeScript-on-Bun lean in section 14 and in review point 41.
- **herdr layout (the developer's call):** Firstmate-style nested workspaces, not tabs. Each worker gets its own workspace, labelled `├ <card> <role>` and kept under its project's orchestrator workspace. The details are in `docs/design/herdr-layout.md`.
- **The build plan** (`docs/plan/build-plan.md`) is now the executable form of v2. Its estimate is day one 86-114 summed agent-hours, or 40-56 h on the critical path with 4 lanes.

---

## 0. Context the judge needs

**Who and what.** One developer runs a fleet of coding agents on two Macs:

- **Personal Mac.** It also reaches a Linux container (host B, the remote Linux host) through herdr's saved SSH machine. That container runs its own fleet.
- **Work Mac.** It is locked down: MDM, managed hooks, bypass mode disabled, auto mode only, and a separate Claude account. Work code must never touch personal infrastructure.

**Today's system: Firstmate** (github.com/kunchenguid/firstmate, MIT). The developer (the "captain") talks to one orchestrator agent, the "first mate". It spawns worker agents ("crewmates") in herdr tabs, each in its own git worktree (made by the treehouse tool), and tracks work in a markdown backlog through the tasks-axi CLI.

Firstmate also has:

- secondmates (persistent sub-orchestrators with their own home);
- `/bearings` (a status digest plus an on-demand Lavish HTML board);
- `/stow` (memory curation);
- captain holds (decisions waiting on the human);
- a validation pipeline called no-mistakes.

**Terminal multiplexer: herdr** (herdr.dev). It shows a sidebar of workspaces ("spaces"), tabs and panes, and detects agent state.

**The four problems the developer stated:**

1. Replies come back interleaved in one chat. Nothing ties a reply to the ask it answers, so the developer keeps asking for status.
2. One orchestrator runs every project. They want one orchestrator per project, in that project's herdr space.
3. Nothing shows the whole picture live.
4. Learning happens by accident. They want a defined, measured improvement loop. They called this the most important part.

**What they asked for:**

- One orchestrator per project.
- Every ask becomes a card the moment it is sent. The orchestrator acks with the card id, and every later reply names it.
- An always-on kanban app on a monitor: one swimlane per project, live push updates, zero agent tokens spent on rendering. Actions: add card, answer decision, approve/reject, reprioritize, comment, jump to the agent's pane.
- The board must never become a second tracker.
- A memory view in the same app: every memory, global and per project, with provenance (card, session, when, why). It supports edit, delete and approve in place, writes back to the real files with history and rollback, and has a queue of proposed memories.
- Written per-project policies: context growth and compaction, memory creation and approval, roles.
- Best-in-class role, subagent and skill definitions.
- An improvement loop with a metric. Proposals come from evidence, nothing applies without approval, and the system improves through the harness (prompts, memory, skills, evals), not model weights.

**Hard constraints:**

- Both Macs run the same way.
- One mechanism per job. herdr's saved machine stays the only way into the remote fleet, with no wrappers.
- Scripts do mechanics, agents do judgment, and a live board costs zero agent tokens.
- Durable state lives on disk; any session can die without losing a card.
- Quality and maintainability beat build cost. Take the simplest direct path first.
- Install skills and agents only through the developer's Nix dotfiles, and respect licenses.
- Time estimates are agent wall-clock. Dates are Lisbon time.
- Firstmate's self-repo boundary: push, open or comment nothing upstream.

**Added mid-task by the developer:**

- "The more I work with [Firstmate] the more I wish I had full ownership and control of it. I'm up for creating my own system as long as it's feasible to create something comparable and my output will not be diminished. Output and quality - both very important."
- "If herdr is holding me up and tmux would be better, that's an acceptable suggestion. I do like the sidebar of herdr a lot."
- "Copy the look and feel of Linear heavily ... use their UI as our basis for our design system."

**Gates:**

1. A research report, then stop.
2. A throwaway board demo, fed by the real backlog and live herdr state, with the estimate given up front. Then stop.
3. The real build in shippable phases.

This file covers gate 1.

---

## 1. Recommendation (one)

**Build an owned system native to Claude Code and herdr, and migrate onto it one job at a time while Firstmate keeps running.** This is a strangler migration.

1. Build the live board first, reading Firstmate's existing state.
2. Then the card protocol, the memory view and the learning loop, all still running on Firstmate.
3. Then replace Firstmate's orchestration one project at a time.
4. Cut a project over only when the new path matches Firstmate's baseline on a scorecard.

Name: Tiller, CLI `tiller` (chosen Sunday 2026-11-01). A tiller is one trunk of a clonal colony: each project orchestrator is one, sharing one root.

### Alternatives considered and rejected

| Option | Verdict | Reason |
|---|---|---|
| Configure Firstmate (one secondmate per project) | Rejected | Mechanically possible (section 6). It runs against Firstmate's "one captain, one interface" design: a secondmate's own chat is documented as one "nobody reads". The developer still owns nothing; 226 upstream commits from 5+ authors landed between 2026-10-01 and 2026-10-31. |
| Fork Firstmate | Rejected | Legal ownership (MIT), but it inherits 242 scripts (146,796 lines of mostly bash) and 337 test files (272,370 lines). Those cover 12 session backends and about 9 harnesses; the developer uses one of each. Pruning interlinked bash looks riskier than writing the subset that is used. |
| Build own, all at once | Rejected | Risks the "output must not drop" requirement. No baseline to compare against. |
| **Build own, strangler with a parity gate** | **Chosen** | Ownership, with output protected by measurement and fallback. |

### Why it is feasible

Claude Code (v2.1.310 on this Mac, docs verified) now ships natively much of what Firstmate built in bash:

- lifecycle hooks (UserPromptSubmit, Stop, Notification, PreToolUse, PostToolUse);
- agent files with model, effort, tools, permissionMode, `isolation: worktree` and persistent `memory:`;
- background subagents that can be resumed;
- skills that can fork into a subagent (`context: fork` plus `agent:`);
- `claude plugin eval` for behavior and trigger tests;
- `/doctor prompt-audit`;
- `autoMemoryDirectory`.

herdr exposes state, ids and focus over its CLI and socket. The standalone tools the developer already trusts stay: treehouse, no-mistakes, tasks-axi, gh-axi, quota-axi, lavish-axi.

### What must be rebuilt to match Firstmate (the "must not lose" list)

| Capability | Firstmate today | Owned version |
|---|---|---|
| Dispatch contract | ship/scout briefs, delivery modes, PR shape rules | role files plus `tiller spawn` writing the brief; no-mistakes unchanged |
| Supervision and wakes | bash watcher, per-backend busy classifiers | Claude Code hooks for turn edges, herdr for liveness, the daemon wakes the orchestrator |
| Restart reconcile | recovery scripts | `tiller reconcile`: cards vs live panes vs worktrees vs PRs |
| Authority guards | never write to a project from the orchestrator, never merge without the captain's word, never tear down unlanded work, push guard | PreToolUse hooks per role; `tiller land` refuses without a recorded answer; `tiller done` refuses unlanded commits |
| Model and quota policy | `crew-dispatch.json` (haiku/low trivial, sonnet/medium default, opus/medium ambiguous, opus/high risky), 3% weekly quota floor, lane and project capacity | same rules file, checked by `tiller spawn` via quota-axi |
| Decisions and merges | captain holds, keyed answers, merge-click safeguards (re-verify the PR is open and green, record the answer before merging, merge only via the merge script) | same semantics as the board's action contract |
| Memory curation | `/stow`, tiers, 30-day decay, 7,500-token startup budget | git-backed memory with provenance, an approval queue, the same budget idea |
| Not rebuilt | relay, voice, 11 other backends, other harnesses | - |

**Parity gate before any project cuts over:**

- Metrics: captain touches per landed card, review findings per PR, median time to done, landed cards per week.
- Each must be no worse than the Firstmate baseline over at least 10 landed cards.
- If worse, the project returns to Firstmate and the gap becomes cards.
- Firstmate's dated field incidents (in its docs and tests) become regression evals before cutover.

---

## 2. Vocabulary (question 1)

**Narrow "software factory" (StrongDM).** Shares the aims of non-interactive work and outcome measurement. But its core rule is code that "must not be reviewed by humans", with hidden holdout scenarios and digital twins of third-party services. The developer's design puts a human at every gate and has no holdouts or twins.

**Broad "software factory"** (Factory.com, Warp: agents across the lifecycle with a control plane and a CI backbone). Matches on orchestration and shared context, but assumes a team SDLC. This is one person, per project, on local machines.

**"Compound engineering"** (Every, Kieran Klaassen, 2025-08-18: "each unit of engineering work should make subsequent units easier"). Matches the step that writes learnings for the next run. But it is a plan, work, review, compound ritual inside one session, with no board, supervisor, attention queue or metric.

**Measurement is the gap in all three.** StrongDM's "satisfaction" score is the nearest model.

**Best name:** *a per-project agent control plane with a compounding loop*.

- control plane = per-project orchestrators plus the board;
- compounding loop = measured memory and harness changes;
- attention management = what the board is for.

Avoid "factory": it implies no human review.

---

## 3. Architecture

### State model: one home per host

Each host (host A the personal Mac, host B the remote Linux host, host C the work Mac) has its own home, a private git repo at `~/.tiller/`:

```
projects/<p>/cards.md          # the one tracker, tasks-axi markdown
projects/<p>/events.jsonl      # append-only, gitignored; hooks and scripts write it
projects/<p>/threads/<card>.jsonl   # per-card conversation, de-interleaved
projects/<p>/policy/{context,memory,roles}.md
projects/<p>/memory/  proposals/
```

The code (daemon, `tiller`, hooks, role files, skills, evals) lives in one repo, installed through dotfiles, identical on every machine. No work data reaches a personal home.

### Who writes what

| State | Writer | Readers |
|---|---|---|
| Cards | the orchestrator, `tiller` scripts, board actions (through `tiller`) | everyone |
| Events | hooks and scripts only, never agent prose | daemon, retro, scorecard |
| Threads | UserPromptSubmit and Stop hooks | board, retro |
| Memory, policy, proposals | agents (notes tier); `tiller mem apply` (rules tier, after approval) | sessions at start, board |
| Project AGENTS.md, dotfiles CLAUDE.md, skills, roles | a worker PR or a dotfiles commit, after approval | every session |
| Live agent state | herdr server plus Claude Code hooks | daemon only |

### How events reach the board with zero agent tokens

One daemon per host.

1. It watches each home with FSEvents or inotify and re-reads changed files (about 150 ms debounce).
2. It polls `herdr api snapshot` every 2 s for the local server and `herdr --machine <id> api snapshot` for saved machines, diffing on `state_change_seq`. The remote read rides the existing saved machine; I measured about 0.4 s. Socket `events.subscribe` is deferred, because it needs an explicit pane_id per subscription (no wildcard) and is not forwarded over `--machine`.
3. It derives each card's column and pushes changed cards to the browser over Server-Sent Events.
4. **Agent state comes from harness hooks first:** UserPromptSubmit = working, Stop = turn ended, Notification = blocked on a permission or question. herdr confirms liveness and owns pane ids. The reason: Firstmate measured live Claude on herdr 0.8.0 reading `idle` through a whole turn (`bin/backends/herdr.sh:3353`).
5. Board actions run the same CLIs agents use (tasks-axi, the hold-answer command, herdr), so the board cannot drift into a second tracker.

During the Firstmate phase the daemon reads `bin/fm-fleet-snapshot.sh --json`, measured at 2.2 s and 38 KB, so updates land in about 2.5 s. It also reads Firstmate's opt-in fleet ledger (`state/fleet-ledger.jsonl`), which Firstmate's docs call "the stable, documented hook for firstmate status".

**Cross-project layer:** scripts only (quota floor and capacity checks in `tiller spawn`). No global orchestrator agent.

**Remote lanes (open decision).** Host B's daemon can be reached from the Mac's browser over a LAN HTTPS name, with a per-browser token. That is a second control path into the remote fleet, which strains the "one way in" rule. The alternative is read-only plus jump through the saved machine.

---

## 4. The board and the card protocol

### Columns

All derived from state; none stored.

| Column | A card is here when |
|---|---|
| Asked | an ask event has no disposition yet (should last seconds) |
| Queued | filed, waiting on capacity, a dependency or a date; dated holds show as Parked with their reason |
| Working | a worker is attached and hooks or herdr say working |
| Needs you | a decision or credential hold, a PR ready with merge authority off, or a pane blocked on a prompt; sorted by age |
| Landing | PR open; pipeline, review or CI running. This replaces the developer's "Review", which was ambiguous (human review is Needs you, machine review is Landing) |
| Done | merged, reported or answered in the last 7 days, with a one-line retro |

### Card protocol (fixes interleaved replies)

1. The developer sends a message in a project's orchestrator pane. The UserPromptSubmit hook appends an `ask` event with an id. The board shows it in Asked within about 200 ms, before the agent reads it.
2. The hook adds one line of context: "this is ask a-7f3; start your reply with its card tag".
3. The orchestrator acks in one line, for example `[beta-42] Filed: live render corner lags on fast pans. Queued behind beta-40.` It may instead tag an existing card for a follow-up, or use `[chat]` for small talk. That choice is judgment, so the agent makes it.
4. The Stop hook checks mechanically that every tag names a real card and every ask in the turn has a disposition. If not, it blocks the stop once with the reason. It then copies the reply into each tagged card's thread.
5. Chat stays interleaved; the board de-interleaves by card.

### Board actions

| Action | What it runs |
|---|---|
| add card | `tiller card add` plus one line typed into the orchestrator's pane |
| answer | keyed hold answer |
| approve or reject merge | records the answer; the orchestrator re-verifies the PR is open and green, then merges via the merge script |
| reprioritize | tasks-axi `--priority 0-4` |
| comment | thread entry plus a pane line |
| jump | `herdr agent focus <pane-id>` |

"Add card" and "comment" start an agent turn and so cost tokens. That is intake work, not rendering.

### Design basis

Per the developer's request, the app copies Linear's visual language:

- tokens read from linear.app's production CSS on 2026-11-01: dark surfaces #08090a/#0f1011/#141516/#191a1b, text #f7f8f8/#d0d6e0/#8a8f98/#62666d, borders #23252a/#34343a, accent #7170ff (#5e6ad2 in light), signal colors yellow #f0bf00, orange #fc7840, green #27a644, indigo #5e6ad2;
- Inter with cv01 and ss03, weights 400/510/590/680;
- radii 4/6/8/12;
- Linear-style status icons, peek panel, command menu, list grouping and inbox.

Not copied: Linear's name, logo or product copy.

Linear concepts map onto the app like this:

| Linear | Tiller |
|---|---|
| Issue | Card |
| Identifier | Card id |
| Workflow states | the derived columns |
| Board rows | project swimlanes |
| Triage | Asked plus proposed memory |
| Inbox | Needs-you items only |
| Assignee avatar | worker avatar with a live status dot |
| Activity | card thread |

---

## 5. Terms and handoff rules (question 2)

| Term | Verdict on the developer's hypothesis | Correct definition |
|---|---|---|
| Skill | Correct by default, but incomplete | Instructions loaded into the current agent when the description matches or on `/name`. It can set `model` and `effort` for the turn, remove tools (`disallowed-tools`), register hooks, auto-activate on file `paths`, and with `context: fork` plus `agent:` run in an isolated subagent. `allowed-tools` pre-approves; it does not restrict. |
| Subagent | Mostly correct | In-process child of one session, with its own context, tools, model, effort and permission mode. It need not end (resumable via SendMessage), runs in the background by default in interactive sessions, nests up to 3 levels, supports `isolation: worktree` and persistent `memory:`. Dies with the parent session. |
| Teammate (agent teams) | Missing tier | Sessions coordinated by one session through shared task tools. Skipped here: it overlaps workers, and herdr cannot see it. |
| Worker | Correct | A separate long-lived harness process in its own pane and worktree. Survives its orchestrator, can be watched or typed to, durable records. Firstmate's crewmate. |
| Role | Correct | A definition (prompt, tools, model, effort, permissions, review duties, status contract) any form can carry. |
| Orchestrator | Correct | A worker whose role is planning and supervising, which never does project work itself. Firstmate's secondmate is an orchestrator worker with its own home. |

**Handoff rules for the orchestrator:**

- **Skill** when the procedure needs this conversation's state, the output is a reply, card change or decision here, and it fits in context.
- **Subagent** when the work is read-only and bounded (find, summarize, review a diff, retro a card), would flood context, suits a cheaper model or narrower tools, and finishes inside the session. Never for project changes. At most 3 at once without asking.
- **Worker** when it changes a project (always), runs over about 10 minutes, needs its own worktree or permissions, or must survive an orchestrator restart.
- Tie-breakers: parallel writes take workers, parallel reads take subagents; a narrower tool set takes a subagent with an allowlist; lifetime beyond the session takes a worker.

**One role file, three forms.** Write each role once, as a Claude Code agent file (`roles/<name>.md`).

- Subagent: dotfiles link it into `~/.claude/agents/`.
- Worker or orchestrator: `claude --agent <name>` with the brief as the first prompt.
- Skill: only when a slash trigger is wanted, as a five-line SKILL.md with `context: fork` and `agent: <name>`.

Firstmate crew briefs remain Firstmate artifacts until a project migrates. The setup needs main-session agents (orchestrator), worker agents (builder, scout, verifier), subagents (reviewer, retro curator) and user-invoked skills (`/retro`, `/card`, a stow replacement).

---

## 6. Firstmate questions (question 3)

**Can one secondmate per project plus herdr spaces work?** Yes, mechanically.

- A `--secondmate` launch stands up its own herdr workspace (`docs/herdr-backend.md:136`).
- Keep the default one-task spaces (herdr 0.8+, `:173`), so each crewmate is its own workspace placed under its secondmate. Do not set `config/herdr-presentation-spaces=off`, which turns crew into tabs; the developer prefers nested workspaces (see `docs/design/herdr-layout.md`).
- Each home keeps its own backlog, state and lock.

**What it changes in "one captain, one interface":**

- It breaks the model. VISION.md:14 says "The captain talks to the first mate and to nobody else".
- `docs/secondmate-parent-channel.md:9` says a secondmate's chat is one "nobody reads"; outcomes reach the captain only through the parent channel. Direct talk is allowed (`docs/architecture.md:387`), but chat replies from a secondmate would need a charter change.
- The primary becomes a silent router that still has to be kept alive.

**Can the bearings board become the live board?** No.

- It is a Lavish page rebuilt per invocation from a payload the agent composes, so every refresh spends tokens and nothing pushes on change.
- Borrow from it: its buckets, the keyed answer intake, the merge-click safeguards, dropping cards whose subject already landed, and the reconcile choice.

**Self-repo boundary:** respected. Nothing edits a tracked Firstmate file or touches upstream. The card-protocol hooks go in `.claude/settings.local.json` through dotfiles, because a dotfiles script rewrites that file on every rebuild (recorded in the developer's learnings).

---

## 7. herdr (question 5): keep it, do not switch to tmux

**herdr gives today** (docs v0.9.5; the Mac runs 0.9.3, protocol 22):

- states idle, working, blocked, done, unknown, rolled up per space in the sidebar;
- `api snapshot` (0.7.2+) with workspaces, tabs, panes, agents and focused ids;
- `--machine <id>` API reads over SSH (0.9.1+, measured about 0.4 s);
- `herdr agent focus <pane-id>` focuses a local agent pane by id;
- a socket with `events.subscribe` (`pane.agent_status_changed`) and server-side plugin event hooks (0.7.0+).

**Gaps and bridges:**

| Gap | Bridge |
|---|---|
| Remote events are not forwarded (discussion #5404) | poll snapshots |
| No subscribe-to-all; subscribers raise CPU (issue #5194, fix PR #5522 open) | poll first |
| A remote focus cannot switch the local window to that machine (discussion #4396, no maintainer reply) | focus remotely and show "switch to host B" on the card |
| No OSC status protocol shipped (draft PR #5172 proposes OSC 7501) | hooks give exact state now |
| idle is unreliable mid-turn | hooks lead |

**Developer's discussion #5582** (opened Monday 2026-10-26, 20:08): open, Ideas category, no maintainer reply, nothing planned. One +1 on Sunday 2026-11-01 at 16:25 noted that `herdr agent focus <pane-id>` already works for agent panes.

**tmux would cost:**

- the sidebar and agent state (state would come only from hooks, which lead anyway);
- remote work becomes ssh plus tmux attach, a second way in.

Keep every herdr call in one small module so a later swap touches one file.

---

## 8. Prior art (question 4)

None was adopted whole, because each keeps its own store (a second tracker).

| Tool | Borrow | Why not whole |
|---|---|---|
| Vibe Kanban (Apache-2.0, 28.5k stars) | card = branch plus terminal plus dev server; export-only exit | Company shutdown verified (vibekanban.com/blog/shutdown, 2026-04-10); repo community-maintained (commits to 2026-10-28); own DB |
| langwatch/kanban-code (Apache-2.0) | column signals from Claude Code hooks; orphan-worktree reconciler | tmux only; second store |
| dev-3.0 (Apache-2.0) | agent-raised `attention` flag; read-only peek at a peer pane | tmux; brew cask on an MDM Mac |
| kanbots (MIT, quiet since 2026-05-25) | MCP server over the board; pre-push guard | SQLite tracker; headless `claude -p` |
| iii kanban worker (Apache-2.0) | change events with an `as_of` cursor for replay; actor id on every write | needs the iii engine |
| Conductor | webhook routines | cloud API; work leaves the machine |
| GitHub Projects v2 | Status single-select field as columns | state on GitHub, not local markdown; work and personal mix |

StrongDM's attractor is a spec repo (Apache-2.0). Factory and Devin are cloud-first.

---

## 9. Memory view and write-back

**Sources shown:**

| Source | Current history | Write-back |
|---|---|---|
| dotfiles CLAUDE.md and AGENTS.md | git | dotfiles commit |
| harness auto-memory | none | move into the home repo via `autoMemoryDirectory` |
| Firstmate `data/captain.md` and `data/learnings.md` | none, gitignored | a private git repo over just those files |
| project AGENTS.md, CLAUDE.md, `.claude/rules/` | project git | approval files a card; a worker opens a PR |
| dotfiles skills and roles | git | dotfiles commit; plugins are read-only |
| subagent memory | depends on scope | same as auto-memory |

**Provenance:**

- Every write is a git commit with trailers `Card:`, `Session:`, `Actor:` and `Why:`.
- A PostToolUse hook on Write and Edit to memory paths commits agent writes; `tiller mem` commits board edits.
- "Why" is the captain message that preceded the write, copied from the thread, and is editable.
- No marker bytes go into memory files, so provenance costs no startup tokens.
- Rollback is a one-click `git revert`.

**Proposals:** `proposals/` entries carry target, exact diff, evidence links and expected effect. Lifecycle: proposed, approved, applied, reverted.

---

## 10. Written policies per project

Three files in `policy/`, inheriting global defaults, editable on the board through the same git write-back.

**context.md:**

- global CLAUDE.md under 200 lines;
- project AGENTS.md mostly navigation pointers;
- orchestrator startup memory under 7,500 tokens;
- orchestrators stow and clear at about 150k tokens instead of auto-compacting;
- one card per worker session;
- reads over about 10 files go to a subagent.

**memory.md:**

- Notes tier (auto-memory, learnings): agents write directly, the human reviews after; 30-day decay unless reinforced.
- Rules tier (captain prefs, CLAUDE.md, skills, roles, hooks): proposal, approval, script applies.
- Project rules: approval plus a PR.
- Mechanical rules become hooks or lint, not prose.

**roles.md:**

- roles used in this project;
- model and effort per role, from crew-dispatch rules;
- review duties, including offering an independent review on sizeable or sensitive PRs;
- delivery path.

---

## 11. Learning loop and metric (question 6, the priority)

### The loop

1. **Capture** with zero-token events.
2. **Retro** on each Done card: a Sonnet subagent writes counts, quotes and at most one learning, gated by a counterfactual test borrowed from Every's ce-compound ("would a future agent still repeat the mistake?").
3. **Propose** with evidence: 2+ cards or one severe incident, an exact diff, mechanical rules routed to hooks or lint (from mattpocock's retro skill).
4. **Approve** on the board.
5. **Apply:** a script commits, adds a regression eval case, and opens a 10-card measurement window.
6. **Measure:** a revert is offered if the guardrails worsen.
7. **Prune monthly:** `/doctor prompt-audit` for conflicts, stale references and no-ops, plus promotion of notes reinforced 3+ times into proposed rules.

Model upgrades re-run the evals, because they can turn rules into no-ops.

### Primary metric

**Captain touches per landed card** = (corrections + unplanned escalations + rework) / cards landed. Lower is better.

- **Correction:** a captain message on a card that changes direction or points out a mistake. The retro judges it and quotes it.
- **Unplanned escalation:** a Needs-you entry the brief did not plan for.
- **Rework:** a reopened card, or a new card with `fixes: <id>`, within 7 days.

**This metric is a design choice.** The research found no primary source measuring corrections per task as an outcome.

### Guardrails against gaming

| Guardrail | Measure |
|---|---|
| Output | landed cards per week |
| Quality | review findings per PR, and escaped defects (later bug cards) |
| Speed | median time from Asked to Done |
| Cost | tokens per card, reported only |

### Evidence from the survey

- Addy Osmani (2026-01-31): fresh process per task, validate, commit, log learnings to AGENTS.md, morning PR review. The AGENTS.md writes have no review gate.
- CODESKILL (arXiv 2605.25430): +11.03 points average pass rate, but via a trained 4B manager model. Only the skill-bank design transfers: task-level plus event-driven skills, skip on weak evidence, add/merge/drop.
- arXiv 2602.11988: context files like AGENTS.md do not generally improve task success and raise cost by over 20%. This is why every rule must earn its tokens and pruning is a first-class step.
- claude-reflect (MIT): corrections queue and apply only after approval. It is the closest to this loop.
- ECC continuous-learning-v2: confidence-scored "instincts" with no human gate. Borrow only the cross-project promotion rule.

---

## 12. Role and skill standard, starter catalog (question 7)

**Collections** (stars on 2026-11-01):

| Collection | Stars | License | Assessment |
|---|---|---|---|
| obra/superpowers | 310k | MIT | best process: "Use when" descriptions, a verification iron law, a 20 KB size test |
| mattpocock/skills | 309k | MIT | best writing: one job each, small files; no evals |
| affaan-m/ECC | 295k | MIT | breadth over quality |
| anthropics/skills | 186k | mixed, much Apache-2.0 | skill-creator has the description optimizer |
| addyosmani/agent-skills | 112k | MIT | best evals: one description clause moved TDD triggering from 6/24 to 19/24 with near-misses at 0/24; long skills, and its reviewer has write tools |
| wshobson/agents | 42k | MIT | breadth over quality |
| github/awesome-copilot | 41k | MIT | breadth over quality |
| VoltAgent | 26k | MIT | breadth over quality |
| vercel-labs/skills | 38k | MIT | an installer, not definitions |

The four "breadth over quality" collections share the same weaknesses: persona prose, 2,000-word agents, and reviewers with write tools.

**The standard:**

1. One job per definition. User-invoked skills set `disable-model-invocation: true`.
2. The description is the trigger only: "Use when...", key case first, under 1,536 characters, never a workflow summary.
3. Progressive disclosure: SKILL.md under 500 lines and 20 KB.
4. Every step ends on a checkable criterion. Workers return DONE, DONE_WITH_CONCERNS, BLOCKED or NEEDS_CONTEXT. Reviewers get read-only tools and fresh context. Completion claims need fresh command output.
5. Positive phrasing. Prohibitions only for hard guardrails. Prune no-ops.
6. Ship evals: 3+ behavior cases against a no-skill baseline, plus a fires/stays-quiet pair. No edit lands without a failing case first.

**Starter catalog** (all sources MIT; keep copyright notices; install via dotfiles):

| Role | Form | Grounded in | Mark |
|---|---|---|---|
| orchestrator | main-session agent per project | Firstmate AGENTS.md rules; superpowers subagent-driven-development; mattpocock chief-of-staff | write fresh |
| scout | worker, light subagent variant | mattpocock research plus Firstmate's scout report contract | adapt |
| builder | worker | superpowers implementer-prompt; mattpocock tdd; E2E-repro-first rule | adapt |
| reviewer | read-only subagent | superpowers code-reviewer.md | adopt + adapt |
| verifier | worker | superpowers verification-before-completion | adapt |
| curator | subagent plus `/retro` | Every ce-compound; mattpocock retro; VoltAgent memory-curator | adapt |
| authoring | skill | mattpocock writing-for-agents; superpowers writing-skills | adopt |

**Trigger testing:**

- `claude plugin eval` cases with a `tool_used` grader (`tool: Skill`, `input_match`) prove a skill fires. The same grader with `min: 0, max: 0` on a sibling's prompt proves it stays quiet.
- It runs a baseline arm. Use 3+ runs, a pinned model and CLI version, and `--no-publish` (reports otherwise publish to claude.ai).
- skill-creator's loop tunes descriptions: 20 queries, a 60/40 split, 3 runs each.

---

## 13. Migration path (agent wall-clock)

| Phase | Work | Estimate |
|---|---|---|
| 0 (gate 2) | throwaway board demo: a local server polls the Firstmate snapshot plus herdr (local and saved machine) and pushes over SSE into one Linear-styled HTML kanban; read-only, with jump for local panes | 60-75 min (was 45-60 before the Linear design basis) |
| 1 | live read-only daemon, installed via dotfiles as a login service; turn on Firstmate's fleet ledger | 5-7 h |
| 2 | board actions via Firstmate CLIs | 3-5 h |
| 3 | card-protocol hooks via dotfiles | 2-3 h |
| 4 | memory view, git provenance, proposal queue, policy pages | 5-7 h |
| 5 | learning loop, scorecard, eval gate, then 2 weeks of baseline on Firstmate | 4-6 h + 2 weeks |
| 6 | owned orchestrator on one pilot project (alpha recommended for volume); shadow run for at least 10 landed cards, then the parity check | 12-18 h + 1-2 weeks |
| 7 | remaining projects one at a time, then the remote Linux host (host B), then the work Mac (host C); retire Firstmate per host after 2 weeks at parity | 1-2 h per project |

Total: about 32-48 agent hours of build, spread over about 4-6 calendar weeks because the baseline and the pilot need time to measure.

---

## 14. Open decisions for the developer

1. **Direction:** own via strangler (recommended), configure secondmates, or fork.
2. **Remote lanes:** full actions over LAN HTTPS with a token (recommended for one board, at the cost of a new control path), or read-only plus jump.
3. **Pilot project:** alpha (recommended: most cards), beta, or gamma.
4. **Daemon language:** Rust (recommended: one static binary, matches another of the developer's projects), TypeScript on Bun, or Go.
5. **Columns:** Asked, Queued, Working, Needs you, Landing, Done (recommended), or the original set.
6. **Design:** Linear basis as built; follow system theme; comfortable density; all-cards board with project rows; short minted card ids like `alpha-42` (recommended over slugs, for cheap chat tags).

---

## 15. Known weak points: where a judge should push

1. **Is the strangler worth it versus staying on Firstmate plus a read-only board?** The board (phases 0-2) solves problems 1 and 3 without replacing anything. The case for phases 6-7 rests on the developer's ownership wish and on Firstmate's upstream churn, not on a measured output gap. A judge could argue: build phases 0-5, and decide on 6-7 only after the baseline data exists.
2. **The 32-48 hour estimate for a Firstmate-comparable orchestrator may be optimistic.** Firstmate's 272k lines of tests encode field incidents: stale panes, reboot recovery, lost outcomes, merge races, permission prompts the orchestrator cannot answer. Phase 6's 12-18 h could double. The parity gate limits the damage, but not the cost.
3. **The primary metric is unvalidated.** "Captain touches per landed card" depends on an LLM retro classifying corrections. Classifier drift or bias could fake improvement. Mitigations: guardrails, quoted evidence, human approval. There is no ground truth.
4. **Small samples.** At current volume (about 5-10 landed cards per day across projects, uneven), a 10-card window per change is noisy. Several approved changes in one window confound each other.
5. **Hooks as the primary state source** assume Claude Code hook events are reliable and identical on the MDM work Mac. Managed hooks there may conflict or be overridden. Not verified on that Mac.
6. **The remote-lane HTTPS path** adds a control surface and contradicts "one way in", unless restricted to read-only.
7. **Card protocol enforcement via a Stop-hook block** could annoy (an extra turn) or loop if the agent cannot satisfy it. It needs a one-block cap, which the design has, and a measured false-block rate.
8. **Linear design copying:** fine for a private tool. If the app is ever published, the look is close enough to raise trade-dress questions. Names and logos are already excluded.
9. **arXiv 2602.11988 cuts both ways:** the plan adds policies, roles and memory, which is more context. The pruning step and the startup budget are the only counterweights.
10. **Unverified items:**
    - OpenAI's "harness engineering" page (returned 403);
    - exact tasks-axi semantics of priority 0-4 versus Linear's scale;
    - whether `herdr agent focus <pane-id>` works across workspaces on 0.9.3 (docs and a commenter say yes; not run locally);
    - work Mac behavior generally.

---

## 16. Primary sources

- Firstmate, local clone at head bb3e6906 (2026-10-31): VISION.md, AGENTS.md, README.md, docs/architecture.md, docs/herdr-backend.md, docs/fleet-ledger.md, docs/secondmate-parent-channel.md, skills bearings, secondmate-provisioning and stow, bin/fm-bearings-board.sh, bin/fm-fleet-snapshot.sh, bin/backends/herdr.sh, bin/backends/herdr-eventwait.py.
- Claude Code docs: code.claude.com/docs/en/skills, /sub-agents, /memory, /plugin-evals; `claude plugin eval --help` on v2.1.310.
- herdr: herdr.dev/docs (v0.9.5); github.com/herdrdev/herdr discussions 5582, 4396, 5404, 2577; issue 5194; PRs 5172, 5522; release v0.9.1.
- Vocabulary: factory.strongdm.ai (and /techniques/dtu); simonwillison.net/2026/Feb/7/software-factory/; factory.com/news/software-factory; warp.dev/articles/best-software-factory-platforms-2026; every.to/guides/compound-engineering; openhands.dev/blog/agent-control-plane.
- Prior art: vibekanban.com/blog/shutdown; github.com/BloopAI/vibe-kanban; github.com/langwatch/kanban-code; github.com/h0x91b/dev-3.0; github.com/leodavinci1/kanbots; iii.dev/registry/workers/kanban; conductor.build/docs/api; GitHub Projects docs; github.com/strongdm/attractor.
- Learning loop: addyosmani.com/blog/self-improving-agents/; arxiv.org/abs/2605.25430; arxiv.org/abs/2602.11988; EveryInc/compound-engineering-plugin skills/ce-compound; BayramAnnakov/claude-reflect; affaan-m/ECC skills/continuous-learning-v2; mattpocock/skills engineering/retro.
- Collections: obra/superpowers; mattpocock/skills; addyosmani/agent-skills (and evals/README.md); anthropics/skills skill-creator; affaan-m/ECC; wshobson/agents; VoltAgent/awesome-claude-code-subagents; github/awesome-copilot; vercel-labs/skills; platform.claude.com agent-skills best practices.
- Design: linear.app production stylesheets (static.linear.app/web/_next/static/css/*), read 2026-11-01.
