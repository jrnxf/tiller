# Usability audit

The owner's view of Tiller: a busy person with a main machine, one paired peer and 14 active projects, who asks orchestrators questions and starts tasks and leaves. This file is the shared state of the audit. Read it first, every time.

Severity: **P0** blocks the job or breaks trust. **P1** is daily friction. **P2** is polish.

Status values: `open`, `fixed` (with what was verified), `needs owner`, `to verify` (seen once, or possibly an effect of the bench).

## State

| | |
|---|---|
| Phase | 1 - audit. Journeys 3 (ask), 4 (start a task) and 5 (trust) are walked on the board and the CLI. Journeys 1, 2 and 6 to 11 are not walked yet. |
| Open P0 | 0. UA-15 is `needs owner`: the silent part is fixed, the answer itself needs a spec change. |
| Open P1 | 4 (UA-03, UA-09, UA-17, UA-25). UA-20 is `needs owner`: a queued card says what it waits for and "Close" is out of the main place, and its "Start" control needs a spec change (see "Needs owner"). UA-18 is `needs owner`: the wrong status line is fixed, Cancel and the full view need a spec change. UA-19 is `needs owner`: the tile, the card and the timeline show the worker's report, and the "Needs you" rule needs a spec change. |
| Fixed | UA-01, UA-02, UA-04, UA-05, UA-06, UA-12, UA-16, UA-21, UA-22, DX-01. UA-15 part 1 (the owner is told where the answer shows). UA-18 part 1 (the ask tile shows an unread answer and opens it). UA-19 part 1 (a worker's report shows on its tile, on the card and in the timeline). UA-20 part 1 (a queued card says what it waits for, and "Close" is under "More" until work starts). |
| Next step | UA-25 (Health counts a live peer as not live; small, and it shares its cause with the `stale` peer row of UA-03). Then the CLI group (UA-17, UA-03, UA-09). Walk journeys 2, 6 and 8 after the stand-in forge of the bench gaps. UA-11 (P2) is not started: check `docs/spec/state.md` section 6 first, because the column of a card is a contract. |

## The bench

One command, inside `nix develop`:

```
scripts/usability-bench.sh up        # build, seed, start; prints two signed-in board links (about 15 s after the build)
scripts/usability-bench.sh down      # stop every process, delete the homes
```

`scripts/usability-bench.sh` with no argument prints every command. The ones a walk needs:

| Command | What it does |
|---|---|
| `main ARGS`, `peer ARGS` | `tiller ARGS` on that host, as the owner |
| `as HOST SESSION ARGS` | `tiller ARGS` as an agent session (`tiller status`, `tiller reply`, `tiller attention open`) |
| `session HOST PROJECT [CARD]` | the live session id of an orchestrator, or of a card's worker |
| `login main\|peer` | a new owner sign-in link |
| `daemon HOST stop\|start\|restart` | the daemon restart and outage cases; a restart keeps the port |
| `link freeze\|thaw` | the peer stops answering, then answers again |
| `quota HOST PERCENT` | the weekly quota the next poll reads |
| `kill-agent HOST SESSION` | a run that dies |
| `shots DIR [PATH...]` | screenshots of the main board's screens, both themes, 1440 px and 390 px (needs `chrome-devtools-axi`; `DIR` must be outside the repository). A path may be a card page: `"/cards/<ulid>?owner=peer"` |

What it builds:

- Two homes under `/tmp/tiller-bench` (`TILLER_BENCH_DIR`), `main` and `peer`, each with its own holder, its own daemon on a free port, and an empty environment. The real `~/.tiller` and port 7421 are out of reach.
- The pairing runs the real `tiller peer add` through an SSH shim, as `crates/daemon/tests/peer_e2e.rs` does.
- Agents are `fake-agent`. Stand-ins replace `claude`, `treehouse` (git worktrees under the pool root) and `quota-axi` (64% of the week left).
- 14 projects (`scripts/usability-bench-seed.sh`): Harbor, Lantern, Quill, Ember, Atlas, Juniper, Cobalt and Meadow on main; Pylon, Tundra, Saffron, Orbit, Willow and Basalt on peer. Each has an orchestrator. The mix: 10 runs (live, blocked, needs-decision, one failed, two reported scouts), 6 questions for the owner, 3 unanswered asks and one answered ask, parked and dependent cards, closed cards, an empty project, a past away period on main, and a peer whose disk floor refuses every new run.

The owner stand-in: a release `tiller` takes an owner command only from a terminal with no agent behind it. The bench builds the debug binary with the `test-owner` feature that the command tests use, and sets `TILLER_TEST_OWNER` for its own homes only. So the CLI on the bench is the owner with no terminal: an ask from the CLI may show as sent by a script.

Bench gaps (fix before the journeys that need them):

| Gap | Journey | Plan |
|---|---|---|
| No pending approval, no landing, no grant in use. An approval needs a repository and a pull request, so a stand-in for `gh`. | 6, 7 | Give two projects a repo and `direct-pr` delivery, and write a stand-in `gh` that answers the pull, checks, reviews and merge calls. |
| Every agent shows Idle. `fake-agent` does no work, so "progress" is only what the bench reports with `tiller status`. | 5, 10 | Type into two panes during the walk (`fake-agent` turns run 4 s), or add a looping driver. |
| No answer comes back to an ask by itself. | 3 | Answer as the orchestrator with `as HOST SESSION reply`. Use a real model at most once per iteration, only to judge the answer's quality. |
| The away period on main lasts under a second. | 11 | Walk away mode by hand during journey 11. |

## Journeys

| # | Journey | Walked on the board | Walked on the CLI |
|---|---|---|---|
| 1 | First run | no | partly (the bench script is a first run by script) |
| 2 | Morning check | first look only | first look only |
| 3 | Ask | yes (UA-15 to UA-18, UA-21, UA-24) | yes (UA-17, UA-23) |
| 4 | Start a task | yes (UA-20, UA-21, UA-22) | yes (UA-09, UA-23) |
| 5 | Trust | partly: progress, blocked and done reports (UA-19). Not walked: a pull request, a landing, a run that dies | partly: the same |
| 6 | Decide | first look only | no |
| 7 | Peer transparency | partly: an ask to a peer project (UA-15, UA-16) | first look only |
| 8 | Failure | no | no |
| 9 | Scale | partly: the command palette finds a card, an agent and a project's jobs (UA-21, fixed). Not walked: filter, sort, the Agents screen at 23 agents | no |
| 10 | Watch and steer | no | no |
| 11 | Away mode | no | no |

What the walked journeys cost, on the bench (Mon 16 Nov 2026, Lisbon):

| Job | Board | CLI |
|---|---|---|
| Ask a project | 4 steps, about 10 s: `A`, `@` and two letters, Enter, the text, Enter. Good. | 1 command, 36 lines of answer |
| See that the ask arrived | main project: the tile shows at once. Peer project: the tile shows in about 1 s (UA-16, fixed; it was never without a reload) | `ask show` has no delivery state (UA-17) |
| Read the answer | main project: 1 click on the ask's tile, which shows "Answered" and the first line within 2 s (UA-18), or Inbox, 2 clicks. Peer project: only on the peer's board; the Ask box, the toast and the tile now say so (UA-15) | not possible (UA-17) |
| Ask again | 1 click on "Ask again" in the reply | type the command again |
| Recover from the wrong project | not possible: the ask tile has no Cancel (UA-18, needs owner) | `ask resolve <ask> cancelled`, which the help calls an orchestrator's tool |
| Idea to a filed card | 3 steps: New card on the project's row, the title, Enter | 1 command, after one refusal for a missing `--kind` (UA-23) |
| Idea to a filed card, from anywhere | 4 steps, about 8 s: `C`, two letters of the project, Enter, the title, Enter. In a one-project view: `C`, the title, Enter (UA-22; before: scroll to the project's row for its button) | the same command |
| Find one card among 43 | `Cmd+K`, a word of its title or its id, Enter: about 4 s, on both hosts (UA-21; before: scroll the board, "No command matches") | `card list`, then read 43 records (UA-09) |
| Filed card to a running card | no board control (UA-20, needs owner). The card says who starts it ("Waits for: The Lantern orchestrator") and prints the shell command. With no orchestrator: 1 click on "Start orchestrator" in the card | `tiller spawn <card> builder`, 73 lines of answer (UA-09) |
| Read what happened on a card | the timeline shows the owner's news only (`harbor-2`: 4 lines in place of 24), with the question, the answer and a comment quoted (UA-04) | `card timeline` prints raw events (UA-09) |
| Answer a card that says "Decision" | 2 steps: open the card, click an option or press its digit. The question, the recommendation and the answer control are at the top of the card (UA-02; before: leave the card, open the Inbox, find the item, answer: 4 steps) | `attention answer <item>`, after `attention list --open` for the id |
| Find the most urgent item in the Inbox | it is the first row and the one shown, and its row says "Urgent" (UA-05; before: the last of 7 rows) | `tiller recap` orders by kind, not by the card's priority |
| Learn that a host starts no new run | a banner on every screen names the host, the numbers and the step ("peer refuses new runs. 4.6 GB of disk free, under the 878,906 GB floor. ..."); the sidebar row, Health and a queued card say it too, and all clear by themselves within 30 s of a fix (UA-01; before: nothing, until a `tiller spawn` was refused) | `tiller machines` has `refuses_runs` on this host's row, and `tiller doctor` has a `floors` line. Main's CLI does not show the peer's (UA-03) |
| Learn that a run is blocked or done | the tile shows the worker's words and "Reports blocked" or "Reports done" within 2 s, on both hosts (UA-19; before, only when the orchestrator raised an item). The card enters "Needs you" only when the orchestrator raises an item | `tiller agents`, in a CSV row |

## Findings

### UA-01 - A host that refuses every new run looks healthy (P1, fixed)

- Journey: 2, 4, 7, 8.
- Tried: the peer has a disk floor far above its free disk. I looked for any sign of it before starting work there.
- Happened: `tiller spawn` on the peer answers `host peer: disk free is 12862 MB, under the 900000000 MB floor`. Before that, nothing warns. `tiller machines` says `health: ok` and 8 free slots. The board's sidebar says "peer, 10 running, 8 free". The Health screen says Safety "Good" and "Both machines are live". `tiller doctor` does not check the floors.
- Hurts: the owner starts a task and leaves. The task never starts, and the only message is the refusal of a command the owner did not run. "8 free" is false.
- Fix: one "can this host start a run now" verdict from the floors (RAM, disk) and quota, in the host's health. Show it in `tiller machines`, the sidebar's machine row, the Health screen and `tiller doctor`, with the reason and the step ("free disk, or lower the floor in Settings > Hosts"). Free slots read 0 while the host refuses.
- Fixed on Mon 16 Nov 2026 (Lisbon), for the RAM and disk floors. `docs/spec/recovery.md` R16 already says "The Machines row shows the breached floor"; no code did it.
  - The verdict: a host's Machines row (`GET /api/v1/machines`, and `tiller machines` for this host) has a new field `refuses_runs`, one line per breached floor with the measured value and the floor. It is absent while the host can start a run. It reads the `host.degraded` events that the host check already writes, so it costs no reading of its own, and a peer's row carries it over the link. `health` and `free_slots` keep their contract values (`docs/spec/protocol.md` section 9); the board words them. `contract/openapi.json` is regenerated.
  - It follows a fix fast: a floor that the owner clears leaves the row at once, and a `host edit` runs the floor check on the next reconcile pass (30 s) in place of the next five-minute check.
  - The board, in four places, each with the numbers in GB (as Settings > Hosts sets them) and the step ("Make room on peer, or lower the floor in Settings > Hosts on its own board"):
    - a banner under the header on every screen, "peer refuses new runs. ...". The sidebar's row alone is not enough: with 16 projects in the list the Machines section is under the fold at 1440 x 900, and at 390 px it is in the drawer;
    - the sidebar's machine row: an amber dot and "10 running, refuses new runs" in place of "10 running, 8 free", with the reason in its tooltip;
    - Health: a "Machines" panel that lists each such host (it shows only then);
    - a queued card on that host: "Waits for: peer to accept new runs", with the reason, and no `tiller spawn` command (this was the open point of UA-20).
  - `tiller doctor` has a `floors` line, read live as a spawn reads it: "RAM and disk are above their floors, so a run can start", or a warning "no new run starts on this host: disk free is 4703 MB, under the 900000000 MB floor; free the resource, or lower the floor with `tiller host edit` or in Settings > Hosts". Settings > Hosts shows the doctor's lines, so the host's card counts it as a problem, for a peer too.
  - Verified on the bench: the main board showed the banner, the row, the Health panel and the wait row of the peer's `pylon-3`; `tiller machines` and `tiller doctor` on the peer named the floor. After `host edit --disk-floor-mb 1000` on the peer, the banner left and the row read "10 running, 8 free" with no reload, within one pass. Checked in light and dark at 1440 px and 390 px.
  - Tests: `crates/daemon/src/api.rs` (`the_machines_row_shows_a_breached_floor_until_it_clears`), `crates/reconcile/tests/recovery.rs` (`a_host_edit_runs_the_floor_check_on_the_next_pass`), `crates/cli/tests/commands.rs` (`machines_names_a_breached_floor_until_it_goes`), `doctor::tests::a_breached_floor_warns_with_the_numbers_and_the_step`, `board/src/lib/machines.test.ts` (2 tests), `wait.test.ts` (the refusing host), `states.test.tsx` (the banner with the sidebar row, and the Health panel).
- Not done:
  - Quota is not part of the verdict. The quota floor lives in `config.toml`, which the Machines read does not load, and a start that the quota floor refuses already opens a decision in the Inbox by contract (`docs/spec/protocol.md` section 8). To verify in journey 8 with `scripts/usability-bench.sh quota HOST PERCENT`.
  - A first breach shows up to 5 minutes late (the host check's cadence), and the numbers are those of the first breach, not live ones. `tiller doctor` reads live.
  - `tiller machines` on main does not show the peer's refusal: the CLI reads only this host's database (UA-03).
  - The banner has no dismiss. After 30 minutes the daemon also opens a recovery item on the host's health card (R16), so the Inbox then says it a second time.
  - The spec's field table (`docs/spec/protocol.md` section 9) and its list of doctor checks (6.4) do not name the new field and the new line. See "Needs owner".

### UA-02 - A card that needs you does not show its question (P1, fixed)

- Journey: 5, 6.
- Tried: on the board I opened `harbor-2`, whose status is "Decision", to answer it.
- Happened: the card panel shows Status "Decision", then Slug, Branch, the worktree's absolute path, a 24-line timeline and the runs. The question, the options, the recommendation and an answer control are not on the card. They are only in the Inbox.
- Hurts: the board says "this card needs you", the owner clicks it, and has to leave the card and find the same item in a list of others.
- Fix: show the card's open questions at the top of the card panel with the Inbox's own answer control (options, keys 1-9, the note field). One component, two places.
- Fixed on Mon 16 Nov 2026 (Lisbon), in the board only (the card read already carries the card's items):
  - A card with an open item shows a "Needs you" block under its title and above its details: the item's kind, its age, the question, what follows from it, the recommendation and who made it, and the Inbox's own answer control (`AnswerForm`: the options with keys 1-9 and the note, or the text field). "Open in the Inbox" goes to the same item for its evidence.
  - With two open items on one card, the keys 1-9 answer the first only, and the others take a click. A merge approval says that it is approved in the card's pull request section.
  - After the answer the block stays with its receipt (Recorded, Delivered, Done) and reads "Answered" until a reload, as an answered row does in the Inbox.
  - A peer's card works the same way: the answer is forwarded to the peer.
  - Verified on the bench: `harbor-2` showed "Rotate keys with a 24 h overlap, or cut over at once?" with both options at the top of the drawer and of the full page; the key `2` recorded "Cut over at once" (`attention.answered`, `via: board`) and the receipt stayed. On the peer's `pylon-2` one click recorded "HMAC-SHA256" on the peer, the timeline said "You answered", and "Needs you" went from 6 to 5. `quill-1` (a scout report with no options) showed the text field. Checked in light at 1440 px (drawer), in dark at 1440 px (full page) and in dark at 390 px.
  - Tests: `board/src/components/board/views.test.tsx` ("a card that needs the owner shows its question and takes the answer in place", "a card with nothing to answer has no question block"). The board fixtures now list a card's own items on its card read, as the daemon does.
- Not done: the card's main button is still "Close" while a question waits. UA-20 removed it only from a card that has not started.

### UA-03 - The CLI on main cannot see or reach work on the peer (P1, open)

- Journey: 3, 4, 6, 7.
- Tried: from main, `tiller board`, `tiller recap`, `tiller card show pylon-2`, `tiller ask note pylon --text ...`.
- Happened: `tiller board` lists main's cards only. `tiller recap` lists 4 decisions; the board's Inbox lists 6 (2 are on the peer). `card show pylon-2` answers `no card pylon-2` and `ask note pylon` answers `no project pylon`, with no hint that the project lives on the paired host. `tiller machines` on main shows the peer as `stale` with no slots, while the board shows it live.
- Hurts: the owner must know which machine owns a project, and open a shell on it, to use the CLI. The board hides the host; the CLI makes it the first question. `tiller recap` gives an incomplete morning list without saying so.
- Fix: owner reads and writes in the CLI go through the local daemon when it runs, as the board's do, so a peer's project, card and question resolve by name. Until then, a `not_found` on a name the peer owns says "pylon is on peer" and `tiller recap` says how many items the peer holds. Check `docs/spec` before the fix: if the CLI is host-local by contract, this moves to "Needs owner".

### UA-04 - The card timeline is plumbing (P1, fixed)

- Journey: 5.
- Tried: read `harbor-2`'s timeline to learn what happened.
- Happened: 24 lines, among them "Tiller deferred a message: not idle" four times, "Tiller delivered a message" five times, "You stamped the session", "You recorded a reservation reserved", "Hook bound a session to the run", "You placed the run on a machine" three times. The two lines that matter ("Orchestrator raised an item for you", "Orchestrator reported the run status") do not say what the item or the status was.
- Hurts: the timeline is the proof behind "done" and "blocked". The owner cannot read it. "You stamped the session" credits the owner with steps the system took.
- Fix: the default timeline shows owner events with their content (the status line, the question, the answer, the PR, the landing). A "Show system events" switch shows the rest. Events a command made in one step fold into one line.
- Done with UA-19 on Mon 16 Nov 2026 (Lisbon): a worker's report reads "Worker reported blocked" with its words quoted under it, and a worker's lines no longer say "Orchestrator".
- Fixed on Mon 16 Nov 2026 (Lisbon), in the daemon's timeline read and the board's card page:
  - Each line of `GET /api/v1/cards/{id}/timeline` has a new field `system`. It is true for the steps of the machinery: message delivery (`outbox.*`, except a failed delivery and the owner's own words), session and slot bookkeeping (`session.*`, `reservation.*`, `run.placed`, `run.cleaned`), obligations, observations and a sent notification. The read still returns every event, so the contract of `docs/spec/protocol.md` ("the card's events, newest first, paged") holds. `contract/openapi.json` is regenerated.
  - The board hides the system lines. A "System events 21" switch in the timeline's header shows them, in a quieter colour. The choice is kept in the browser, like a folded lane.
  - The lines that stay say what happened: "Orchestrator asked you for a decision" with the question quoted, "You answered" with the answer quoted, "You commented" with the comment quoted (before: "You queued a message"), "You started a builder run (opus, medium)", "ended the run as failed".
  - A system step is never credited to the owner: "Tiller stamped the session", "Tiller reserved a slot" (before: "You recorded a reservation reserved").
  - The raw "Events" list at the bottom of the card (UA-24) shows only with the switch on. With the switch off, a line of that list that reports a problem still shows, under "Problems in the event log".
  - Verified on the bench: `harbor-2` showed 4 lines in place of 24, then 6 after a comment on the board and an answer in the CLI, each within 2 s and with its words. The switch showed all 27. The peer's `pylon-1` showed 3 lines and "System events 16" through the link. Checked in light at 1440 px (switch off and on) and in dark at 390 px.
  - Tests: `crates/daemon/tests/status_timeline.rs` (`the_owners_news_is_quoted_and_the_machinery_is_marked`), `views::tests::timeline_lines_read_as_plain_sentences`, `board/src/components/board/views.test.tsx` ("a card's timeline keeps system events out of the way until the owner asks for them").
- Not done, by choice:
  - Lines are hidden, not folded into one. With the system lines out, no run of repeated lines was left on the bench.
  - `tiller card timeline` in the CLI still prints the raw events. That belongs to UA-09 (CLI answers are raw records).
  - When the newest 50 events are all system events, the timeline says so and offers "Show earlier". It does not load further pages by itself.

### UA-05 - The Inbox ignores priority (P1, fixed)

- Journey: 2, 6.
- Tried: opened the Inbox to learn what needs me first.
- Happened: 7 items, newest first. The only P0 Urgent card (`harbor-2`) is last. A reply to a question I asked sits in the middle.
- Hurts: with 14 projects the list is long. The owner answers what is on top and the urgent item waits.
- Fix: order by card priority, then by how long the item has waited (oldest first). Show the priority on the row when it is P0 or P1.
- Fixed on Mon 16 Nov 2026 (Lisbon), in the board (`board/src/components/attention/order.ts`). No spec fixes the order of the Inbox's items; only the replies read is "newest first" (`docs/spec/state.md` and the replies read of `docs/spec/protocol.md`), and replies keep that order.
  - Rows that wait on the owner (decisions, permission prompts, dialogs) come first: the most urgent card, then the longest wait. An orchestrator stopped on a dialog has no card and counts as urgent, because it holds up its whole project. A row answered from this tab moves under the rows that still wait.
  - A row of a P0 or P1 card says "Urgent" or "High" with the board's priority mark.
  - Replies follow under their own "Replies N" heading. They are news, not a job. The "Replies" filter is unchanged.
  - With nothing chosen, the detail shows the first row, so the most urgent item is what the Inbox opens on.
  - Verified on the bench: `harbor-2` (P0) is the first row with "Urgent" (before: the last of 7), and the one reply is last under "Replies 1". Checked in light at 1440 px and in dark at 390 px.
  - Tests: `order.test.ts`, `inbox.test.tsx` ("the most urgent card leads the list, then the longest wait, and an urgent row says so"), `replies.test.tsx` (the replies group).
- Not done: `tiller recap` orders by the item's kind and then the age (`crates/cli/src/cmd/recap.rs`, a contract of `docs/spec/protocol.md`: "most blocking first"). The two orders differ. The "Needs you" count still includes unread replies (UA-24).

### UA-06 - Every Inbox row says "decision" (P2, fixed)

- Journey: 2, 6.
- Happened: a credential request, a recovery question and a scout report all show "decision" in the row and "Decision" in the away brief. `tiller board` and `tiller recap` name the real kind.
- Hurts: "issue a new key" (I must do something elsewhere) and "pick A or B" (one key press) look the same.
- Fix: show the item's kind with the words the CLI uses, and a distinct icon for credential and recovery.
- Fixed on Mon 16 Nov 2026 (Lisbon): an Inbox row names its kind ("decision", "credential", "merge approval", "recovery", "scout report", "review") and has an icon for it (a fork, a key, a merge mark, a retry arrow, a page). The away brief's "Still waiting for you" names the kind too. The separate "merge" chip is gone, because the row says "merge approval". Verified on the bench in both themes; test: `inbox.test.tsx` (the kinds of the shared list).

### UA-07 - The host is on every card (P2, open)

- Journey: 7.
- Happened: every card on the board and every Inbox row carries a "main" or "peer" chip. In the sidebar, a peer's project has a second line with a "peer" chip, so its row is taller than a main project's row.
- Hurts: the owner should not think about machines. The chip is the loudest repeated element on the board, and the list has two row heights.
- Fix: show the host only when it matters: when a host is offline, stale or refusing runs, in the machine filter, and in the card's details. Recommendation only; the chip may be a deliberate choice, so confirm with the owner before removal.

### UA-08 - "host / main" and "host / peer" are listed as projects (P2, open)

- Journey: 2, 9.
- Happened: the sidebar and the board list the two host health projects among the owner's projects, in lower case. The breadcrumb counts "All projects 16". The owner has 14.
- Fix: move host health out of the project list to the Machines section, and count only the owner's projects.

### UA-09 - CLI answers are raw records (P1, open)

- Journey: 1, 3, 4.
- Happened: `tiller init` prints about 60 lines of fields (`stage_changed_at: null`, `reviewed_through_sha: null`) and one hint. It does not say to start the daemon, open the board or pair a host. `tiller spawn`, `tiller ask note` and `tiller card create` print the whole row, most of it `null`. The answer to "did it start, where, on which model" is in there, among 40 other lines.
- Hurts: the owner has to read a record to confirm an action. The next step is not shown.
- Confirmed in journeys 3 and 4: `ask note` answers with 36 lines, `spawn` with 73. The host is a 26-character id, never its label. The one hint after `card create` is "start each line about this card with [lantern-5]", which is an instruction for an orchestrator, not for the owner.
- Fix: a short human summary by default for owner commands ("Started harbor-1 build on main, sonnet, medium. Watch: tiller attach harbor-1-build"), with the next step. `--json` stays the full record.

### UA-10 - `tiller host edit` needs a rev that no command prints (P2, open)

- Journey: 1, 8.
- Happened: `tiller host edit --rev <REV>` is required. `tiller machines` does not print the rev. The only way to learn it is to send a wrong one and read the conflict.
- Fix: print `rev` in `tiller machines` for this host, or let `host edit` read the rev itself when the owner runs it at a terminal.

### UA-11 - Cards with nothing running sit in "Working" (P2, open)

- Journey: 5.
- Happened: `ember-1` (its run failed and ended) and `quill-1` (its scout reported and ended) are in the Working column. The card of a live run that the agent left idle shows the badge `idle`.
- Hurts: "Working 11" overstates what is in progress. A failed run looks like work.
- Fix: a card whose runs all ended and that waits for the owner belongs in a "Needs you" lane or carries a clear "stopped" state in the column.

### UA-12 - The Inbox row's age wraps to its own line (P2, fixed)

- Journey: 2.
- Happened: at 1440 px, rows with a longer project and card name ("Lantern / lantern-1 / decision /") push the age ("3m") to a second line, after a hanging "/".
- Fix: keep the age on the right edge of the row, outside the wrapping text.
- Fixed on Mon 16 Nov 2026 (Lisbon), another way: the row's second line is two pieces that never break inside, the place (host, project, card) and "kind / age". A narrow row wraps between the pieces, so no "/" hangs and no age stands alone. Verified at 1440 px and 390 px.

### UA-13 - The away brief counts runs outside the away scope (P2, to verify in journey 11)

- Journey: 11.
- Happened: away mode covered Quill and Ember. The brief says "Runs started 6" and lists Harbor, Lantern and Juniper too. "Still waiting for you 2" counts only the two projects in scope, while the Inbox holds 7.
- Hurts: two different scopes in one card. The owner cannot tell whether the brief is the whole picture.
- Fix: say the scope in the brief ("Quill and Ember"), keep both lists to it, and add one line for what happened outside it.

### UA-14 - An ask from the CLI is labelled "script" (P2, open)

- Journey: 3.
- Happened: an ask sent with `tiller ask note` carries a "script" chip on the board; an ask from the board's Ask box carries "board". This is not an effect of the bench: `ask note` always records the source `script` (`docs/spec/protocol.md` section 2).
- Hurts: the owner typed both. "script" suggests that something else sent it.
- Fix: show no chip for the owner's own asks, or one word for both ("you"). Keep "script" for an ask that a script's idempotency key marks. The wording "Waiting for the orchestrator to tag it" moved to UA-18.

### UA-15 - The answer to an ask sent to a peer project never reaches the owner (P0, needs owner; the silent part is fixed)

- Journey: 3, 7.
- Tried: on the main board, `A`, `@Pylon`, "What port does the gateway listen on?", Enter. The toast said "Sent to the Pylon orchestrator". Pylon's orchestrator answered with `tiller reply`.
- Happened: the answer never shows on the main board. The Inbox's "Replies" count stayed at 2 (both from main projects) for more than 45 s and after a reload. The ask's tile under Pylon shows no question text, only "Waiting for the orchestrator to tag this ask", so two asks to one peer project cannot be told apart. The answer exists only on the peer's own board.
- Cause: by design today. A peer's board rows carry an ask's source but not its text (`docs/spec/protocol.md` section 10, the `snapshot` frame: "ask text stays on the peer"), and replies are read per project from this host only (`board/src/lib/queries.ts`, "Never federated: a peer's replies stay on the peer"; `docs/design/cli-board-parity.md`, "Mark a reply read ... does not forward the call to a peer").
- Hurts: the Ask box offers every project, accepts the ask, confirms it, and then the owner waits for an answer that cannot arrive. This is the owner's first daily job, and it fails without a word for 6 of 14 projects.
- Fix, in two parts:
  1. Inside the contract (no spec change): the main board learns from the peer's `ask.replied` event that an answer exists. The Inbox then shows a row "Pylon answered your ask", and the tile says "Answered". Until part 2, the row opens the answer on the peer's board. The Ask box says so before the send when a target is a peer project.
  2. With a spec change (see "Needs owner"): the link carries the ask's first line and the unread replies of a paired host, and "Mark read" forwards. The text already crossed the link once, from this board to the peer, when the owner sent it.
- Part 1, done on Mon 16 Nov 2026 (Lisbon), smaller than first planned: the board no longer lets the owner wait in silence.
  - The Ask box shows "The answer shows on the board of peer, not in this Inbox." under the project list as soon as a target is a peer project. The toast after the send repeats it.
  - A peer's ask tile reads "Ask kept on peer" and "Text and answer are on peer". It no longer says "Waiting for the orchestrator to tag this ask". The tooltip has the full sentence.
  - Not done: the row "Pylon answered your ask" and the "Answered" mark. An `ask.replied` event of a peer does reach the board, but it is a one-time event: after a reload or a daemon restart the mark would be gone, so the tile would say "waiting" for an answered ask. A mark that survives needs a field in the peer's `asked` rows, which is the same spec change as part 2.
  - Verified on the bench: Ask box, toast and tile in both themes at 1440 px and 390 px. Tests: `board/src/lib/ask-plan.test.ts` (`answersElsewhere`), `ask-dialog.test.tsx` (the line shows before the send), `federation.test.tsx` (the tile's words).
- Still open for the owner: the answer itself. It stays `needs owner` until the decision under "Needs owner".

### UA-16 - An ask sent to a peer project does not show on the board until a reload (P1, fixed)

- Journey: 3, 7.
- Tried: sent an ask to Pylon from the Ask box, then looked for it. Later, recorded an ask on the peer and watched the open main board.
- Happened: the "Asked" count and Pylon's row did not change for 25 s and for 20 s. After a reload the tile was there. A new card on the same peer project showed in 6 s without a reload, so the link works: only ask changes of a peer are missed. `board/src/lib/sse.ts` does refetch the board on an `ask` event, so the event of a peer's ask does not reach the browser, or the cached peer board is not replaced when it comes.
- Hurts: "Sent" and then nothing. The owner sends it a second time, or reloads to check.
- Cause: in the board, not the daemon. An ask event of a peer carries no row, so the peer's next snapshot brings it (at most 2 s later), and the daemon pushes that to the browser as `peer_reset`. The board's `peer_reset` handler replaced the peer's card rows, attention items, agents and projects, but not its `asked` rows (`board/src/lib/sse.ts`, `applyPeerReset`). The refetch that the `ask` event starts runs before the snapshot, so it read the old rows.
- Fixed on Mon 16 Nov 2026 (Lisbon): `peer_reset` replaces that host's `asked` rows too.
- Verified on the bench with the board open and no reload: an ask recorded on the peer moved "Asked" from 3 to 4 within 1 s (before: no change in 25 s), and `ask resolve <ask> cancelled` on the peer moved it back to 3 within 1 s. An ask sent from the main board's Ask box to Pylon showed its tile within 3 s.
- Tests: `board/src/lib/sse.test.ts` ("peer_reset replaces that host's asks"), and `crates/daemon/tests/peer_e2e.rs` (`board_starts_a_peers_orchestrator_and_asks_it` now waits for a `peer_reset` that carries the new ask and reads it from the board host's `/api/v1/board`, with no `summary`).

### UA-17 - The CLI cannot read an answer (P1, open)

- Journey: 3.
- Tried: `tiller ask note lantern --text ...`, then `tiller ask show <ask>` and `tiller ask list` after the orchestrator replied.
- Happened: `ask show` prints the ask and `cards[0]`. It has no reply, no "answered" mark and no delivery state of the note. `ask list` shows `unresolved` for the answered ask. `tiller recap` does not list replies. No command prints the reply text.
- Hurts: a job that starts in the CLI cannot end there. The owner must open the board's Inbox to read the answer to a question typed in a shell.
- Fix: `ask show` prints the note's delivery state and every reply. `ask list` gets an `answered` column. `tiller recap` lists unread replies beside the decisions, and `tiller reply read <reply>` (or `ask show --mark-read`) marks one read, so the parity row "Mark a reply read" becomes Full on both sides.

### UA-18 - An ask tile is a dead end, and its status line can be wrong (P1, needs owner; the status line is fixed)

- Journey: 3.
- Tried: clicked my ask's tile in the "Asked" column, before and after the answer came.
- Happened: the tile is not a link or a button. It cannot be opened, cancelled or moved to another project. After the orchestrator answered, the tile still says "Waiting for the orchestrator to tag it", because the ask keeps the disposition `unresolved` until the orchestrator runs `ask resolve`. The delivery state of the note (pending, delivered, read by the agent) is not shown.
- Hurts: an ask sent to the wrong project stays on the board with no way out. The CLI has one (`ask resolve <ask> cancelled`). An answered question reads as unanswered. "Tag it" is an internal word.
- Fix: the tile opens the ask: the full text, when it was delivered, the answer when there is one, "Ask again" and "Cancel". The tile's line follows the facts: "Sent", "Delivered", "Answered" (with the first line of the answer), and never "tag".
- Part 1, done on Mon 16 Nov 2026 (Lisbon), inside the contract (board only, no new read or field):
  - An ask of this host with an unread answer shows the answer's first line and a green "Answered 2 s ago" chip. The tile is then a link that opens that answer in the Inbox, where "Mark read" and "Ask again" are. The answer comes from the unread replies that the Inbox and the sidebar already read, so the tile adds no request.
  - An ask with no answer says "Waiting for the orchestrator". No tile says "tag" any more.
  - Verified on the bench with the board open and no reload: an ask to Lantern said "Waiting for the orchestrator"; within 2 s of the orchestrator's `tiller reply` the tile showed the first line and "Answered 1 s ago"; one click opened the answer in the Inbox. Checked in light at 1440 px and in dark at 390 px.
  - Tests: `board/src/components/board/views.test.tsx` ("an answered ask says so, shows the answer's first line and opens it in the Inbox", and the two wording tests).
- Not done, and why (see "Needs owner"):
  - After "Mark read", an ask that the orchestrator answered but did not resolve says "Waiting for the orchestrator" again. The board knows only unread replies, so it cannot tell an answered ask from a new one. A mark that lasts needs `answered_at` on the `asked` row, the same field as UA-15 part 2. An orchestrator that follows its instructions resolves the ask in the same turn, so the ask then leaves "Asked"; the bench's `fake-agent` never resolves.
  - "Cancel" on the board. `docs/spec/state.md` section 8 lists the actors of `ask.resolve` as the orchestrator, the board "implicit on add" and the prompt hook, and `docs/spec/protocol.md` section 7 has no call for it.
  - The full text and the delivery state of an ask with no answer. The board row has the first line only (200 characters), and no read returns one ask.

### UA-19 - A worker's own report does not change what the card says (P1, needs owner; the tile, the card and the timeline are fixed)

- Journey: 5.
- Tried: started a builder on `lantern-5`. As the worker, reported `working: Wrote the header builder; 14 of 20 tests pass`, then `blocked: The staging mail server refuses the test login`, then `done: All 20 tests pass`.
- Happened: after each report the tile and the card's Status say "Idle". The Inbox count and "Needs you" do not move. The status line is on the card, but in the "Runs" section under a timeline of 14 to 26 lines, off the first screen at both widths. The timeline says "Orchestrator reported the run status" for the worker's report, without the text. A card shows "blocked" or "needs you" only when the orchestrator raises an item (the seeded `lantern-1` shows "Credential").
- Hurts: this is the "start a task and leave" job. The worker says it is blocked, and the board says "Idle". If the orchestrator is busy, slow or gone, the owner never learns it. "Idle" is the agent's terminal state, not the state of the work.
- Fix: the tile and the Status row show the newest report of the run: its state as the badge ("Working", "Blocked", "Reports done") and its line as the tile's second line, with its age. A `blocked` or `needs-decision` report puts the card in "Needs you" even before the orchestrator acts. The timeline names the worker and quotes the line. Check `docs/spec/state.md` first: if the column and the "needs you" rule are a contract, the badge and the line are still free to change.
- Part 1, done on Mon 16 Nov 2026 (Lisbon), inside the contract (no new read and no new field: the report is the `last_status_line` of the agents read that the board already loads, for both hosts):
  - A tile shows the words of its live worker's newest report as one line under the title. The tooltip has the role, the state and the full line.
  - A report that the work stopped or ended replaces a badge that only says what the terminal does ("Idle", "Working", "Stale", "Unknown"): "Reports blocked" and "Asks for a decision" in the "needs you" colour, "Reports failed" in red, "Reports done" in green, "Reports paused". "Reports done" is the worker's claim, said as one: the card still lands only through its checks. A report of progress keeps the activity badge and adds its words. A card that the orchestrator flagged keeps its item's badge ("Credential", "Decision") and shows the words too.
  - The card's details have a row "Builder says" (or Scout, Reviewer, Verifier) with the same chip and the full line, right under Status, on the first screen at both widths.
  - The timeline says "Worker reported blocked" and quotes the line under it. Before, it said "Orchestrator reported the run status" with no text. Every line of a worker session now says "Worker" (new actor value `worker` in `GET /api/v1/cards/{id}/timeline`; `contract/openapi.json` is regenerated). The board shows an actor it does not know as "Tiller".
  - With two live workers on one card the most urgent report shows: blocked, then needs-decision, failed, paused, done, working.
  - Verified on the bench with the board open and no reload: a new builder on `lantern-4` showed "Idle" and no line; 2 s after `status working ...` the tile showed the words and kept "Idle"; 2 s after `status blocked ...` it showed the words and "Reports blocked". The card showed "Builder says / Reports blocked / The staging mail server refuses the test login", and the timeline "Worker reported blocked" with the quote. The peer's cards show their reports the same way (`pylon-1`, `tundra-1`, `orbit-1` on the board, `pylon-1` on its card page with "Builder says" and the quoted timeline line). Checked in light at 1440 px and in dark at 390 px.
  - Tests: `board/src/components/board/report.test.ts`, `views.test.tsx` ("a tile shows what its worker last reported, and a stopped worker replaces the activity badge"), `crates/daemon/tests/status_timeline.rs`.
- Not done, and why (see "Needs owner"):
  - A `blocked` or `needs-decision` report does not put the card in "Needs you", and the Inbox count does not move. `docs/spec/state.md` section 6 defines the flag (an open attention item, or a session that waits on a prompt) and says that the Needs you tab keeps exactly the flagged cards.
  - The report has no age on the tile. The agents read carries the line but not `last_status_at` (`docs/spec/protocol.md` section 7.3). The timeline has the time.
  - The report goes away with the worker's session: the agents read lists live sessions only. The last line of an ended run is still under "Runs" on the card (UA-11).

### UA-20 - On the board a new task stops at "Filed" (P1, needs owner; the wait and the "Close" button are fixed)

- Journey: 4.
- Tried: "New card" on Lantern's row, a title, Enter. Then opened the card to start it.
- Happened: the toast says "Filed lantern-4". The card sits in "Queued" with the badge "Filed", Model "none yet", and "No runs yet. The orchestrator starts one when the card leaves the queue." Nothing says when that is, on which machine, or with which model. There is no "Start" control: `tiller spawn` exists only in the CLI (`docs/design/cli-board-parity.md`, "Run and session control ... No"). The one filled button on a card that is seconds old is "Close".
- Hurts: "from an idea to a running card" ends one step short on the board, and the owner cannot tell whether the next step is theirs. The loudest button does the opposite of what the owner came for.
- Fix: the new-card dialog gets "File and start" as its main action, which files the card and starts the run the dispatch rule picks, and the card then says what was started ("Builder, sonnet, on main, started 10 s ago"). A queued card says what it waits for ("Waits for the orchestrator's next turn", "Waits for a free slot", "Waits on harbor-1"). On an open card "Close" moves into "More", and the main action is "Start". This adds an owner mutation to the board, so check `docs/spec/authority.md` before the work; if the board may not start a run by contract, it moves to "Needs owner".
- Checked (Mon 16 Nov 2026, Lisbon): `docs/spec/authority.md` gives the owner the right ("Start a run (the execute decision): yes"), but `docs/spec/protocol.md` section 7 has no board call that starts a run, and `docs/design/cli-board-parity.md` lists run control as "No" for the board. So "File and start" and "Start" need the owner (see "Needs owner").
- Fixed inside the contract (Mon 16 Nov 2026, Lisbon), in the board only (`board/src/components/board/wait.ts`, `card-page.tsx`, `card-header.tsx`):
  - A card in "Queued" with no hold and no dependency has a "Waits for" row under Status. It names the project's orchestrator and says when a run starts ("on its next turn, when a slot is free", or "It is working now ..."). It reads the agents list the board already loads, so it adds no request and works for a peer's card.
  - A project with no orchestrator: the row warns "Meadow has no orchestrator. Nothing starts this card until the orchestrator runs", with "Start orchestrator" on the right. A silent orchestrator, a terminal holder that does not answer and a peer whose link is down each get their own warning.
  - The Runs box of such a card prints the command that starts it now (`tiller spawn lantern-4 builder`, with "on peer" for a peer's card). A held or blocked card shows no command and no row: its hold note or its "Blocked by" status already says what it waits for.
  - A card with no run and no pull request has no "Close" button. "Close card..." is in "More". The button comes back once a run exists.
  - Model reads "picked when a run starts" in place of "none yet".
  - Verified on the bench: `lantern-4` and the peer's `pylon-3` name their orchestrator; after `agent stop` of Meadow's orchestrator `meadow-1` showed the warning, one click on "Start orchestrator" started it and the row changed within 5 s; after `tiller spawn lantern-4 builder` the row left and "Close" returned. Checked in light and dark at 1440 px and 390 px.
  - Tests: `board/src/components/board/wait.test.ts` (7 cases) and 2 card page tests in `card-lifecycle.test.tsx`.
- Not done, by choice: the row does not say that the orchestrator received the card's note. The delivery state is in the timeline's system events only, and a claim built from the agents list could be wrong.
- Done with UA-01: a queued card on a host that refuses every new run says "Waits for: peer to accept new runs" with the reason and the step, and prints no spawn command.
- Still needs the owner: "File and start" and "Start".

### UA-21 - "Search or run" finds no card and runs no job (P1, fixed)

- Journey: 3, 4, 9.
- Tried: `Cmd+K`, then "digest" (a word of a card's title), "lantern-1" and "new".
- Happened: "No command matches". The palette holds navigation, theme, density, "Focus <project>", "Hide <project>" and two machine filters. It has no card, no session, no "New card", no "Ask", no "Away".
- Hurts: with 14 projects and 43 cards, the only way to one card is to scroll the board. The field's own label promises search.
- Fix: the palette searches cards (id and title), projects and agents on both hosts, and runs the owner's jobs: "Ask <project>", "New card in <project>", "Turn on away mode", "Open the Inbox item for <card>".
- Fixed (Mon 16 Nov 2026, Lisbon), in the board only, with reads the board already makes:
  - The palette searches cards of both hosts by id and by the words of the title. A card's row shows its status mark, its project and its column, or "Needs you". Enter opens the card (a peer's card too).
  - It searches agents: "Builder on pylon-1 ..." and "Orchestrator of Harbor" open that agent on the Agents screen.
  - It runs the owner's jobs. The empty palette starts with "Ask a project" (`A`), "New card" (`C`) and "Turn on away mode". A query that names a project lists "Ask <project>" and "New card in <project>". A project the owner cannot act on now (a peer whose link is down, a view-only peer) has no such row.
  - A found thing is listed only when every word of the query is in its text. Before, the match was "letters in order", which would list most of 43 cards for any short query. Commands keep the loose match.
  - A group stays in one piece under one heading. Before, a group heading could show twice in one result list.
  - The field's hint says "Search cards and agents, or type a command...", and no result says "No card, agent or command matches".
  - Verified on the bench: "digest" lists `lantern-1` and its builder, and Enter opens the card. "pylon" lists the 3 peer cards, "Ask Pylon", "New card in Pylon", the 2 builders and the orchestrator, and Enter opens `pylon-1` with the peer as its owner. "orchestrator harbor" and Enter opens that agent. "away" and Enter opens "Start away mode". Checked in light and dark at 1440 px and at 390 px.
  - Tests: `board/src/lib/commands.test.ts` (search-only rows, word match, group order) and 7 new tests in `board/src/components/shell/shell.test.tsx`.
- Not done:
  - "Open the Inbox item for <card>" is not a command: the card itself now shows its question and takes the answer (UA-02), and its row says "Needs you".
  - The palette does not search memory, contributions or the text of asks. An ask has no page to open (UA-18, needs owner).
  - "Focus Pylon (peer)" still names the host (UA-07).

### UA-22 - The view of one project has no "New card" (P2, fixed)

- Journey: 4.
- Happened: "New card" is on each project's row in "All projects". After a click on one project in the sidebar, the board shows that project alone, and the button is gone. The header has "Show all projects", the filters, "Away" and "Ask".
- Fix: keep "New card" in the header of the one-project view, and give it a key (`C`) next to `A` for Ask.
- Fixed (Mon 16 Nov 2026, Lisbon):
  - The header of a one-project view has "New card" with its key `C`, between "Away" and "Ask". It opens the dialog for that project.
  - `C` works on every screen. With one project in view it opens that project's dialog. With all projects in view it opens the palette on "New card in ", and two letters and Enter pick the project. The palette's own "New card" row does the same.
  - At 390 px the button is a plus, and "Away" beside it is its moon alone, so the screen's title keeps its room (it was cut to "B" in the first try).
  - Verified on the bench: `C`, "lan", Enter, a title, Enter filed `lantern-4` in about 8 s with no scroll. In the Lantern view, `C` and the header button both open "New card in Lantern". Checked in light and dark at 1440 px and 390 px.
  - Tests: 3 tests in `board/src/components/shell/shell.test.tsx`.

### UA-23 - Small CLI refusals with no next step (P2, open)

- Journey: 3, 4.
- Happened: `tiller card create lantern --title ...` is refused with "a new card needs --kind ship|scout|question|ops"; the help lists `--kind` as an option, and the board defaults it to ship. `tiller ask note lantrn` answers "no project lantrn" with no near match. `tiller status "text"` fails on a parse error because the state comes first.
- Fix: `--kind` defaults to `ship` as on the board. A `not_found` for a project names the nearest id ("did you mean lantern?"), and names the peer when the project lives there (UA-03).

### UA-24 - Small board defects seen in journeys 3 to 5 (P2, open)

- The toast ("Sent to the Pylon orchestrator", "Filed lantern-4") covers the "All / Active / Needs you" filter at 1440 px.
- The new-card dialog explains itself with a command ("Files a card, as `tiller card create` does") and shows the raw kind "ship".
- Fixed with UA-04: the card panel had two event lists, "Timeline" and, at the bottom, "Events" with raw kinds (`outbox.delivered BX6YJFV`, `session.stamped`). The second now shows only with the "System events" switch on.
- A new card's panel shows the worktree's absolute path and the slug before anything the owner needs.
- At 390 px the "x" that removes a project in the Ask box wraps to a line of its own.
- To verify: the Ask box said "will start orchestrator" for Lantern, whose orchestrator was running, right after a page load. The label may read "not running" while the agents list still loads (`board/src/components/shell/ask-dialog.tsx`, `statusOf`).
- The Inbox counts unread replies in "Needs you 8". A reply is news, not a job for the owner. Since UA-05 the replies have their own group under the rows that wait, but the count and the sidebar badge still include them.
- The board has two names for priority 4: "Someday" in the card's priority picker (`attention/format.ts`, `PRIORITIES`) and "Lowest" in the card's details (`board/icons.tsx`, `PRIORITY_NAMES`).
- A card with a live run has "Close" as its one filled button, but the daemon refuses a close while a run is active (`docs/spec/state.md`, `card.close`). Seen after UA-20's fix; not checked what the dialog says on the refusal.
- To verify: the `A` key that opens the Ask box may also type "a" into its field. Seen once with a scripted key press (`chrome-devtools-axi press a`); not checked with a real keyboard.
- To verify: `board/src/components/shell/ask-settings.test.tsx` (line 181, the folder field's value) failed once in a full `bun run test` while the bench ran, and passed 3 times alone and in the next full run. It has a `waitFor` deadline, so the cause is probably the machine's load.

### UA-25 - Health says "Only 1 of 2 machines are live" while the peer is live (P1, open)

- Journey: 2, 7, 8.
- Tried: opened Health on the main board during the walk of UA-01, with the peer's link live.
- Happened: the Safety card reads "Watch", "Only 1 of 2 machines are live" and "machines live 1 of 2". The sidebar shows the peer with a green dot and its counts, and the peer's cards are live on the board. The count comes from `hosts.last_seen_at` in the last two minutes (`crates/core/src/state/signals.rs`, `machines_live`), and a live peer link does not keep that column fresh on the bench. `tiller machines` on main shows the same peer as `stale` for the same reason (UA-03).
- Hurts: Safety is the card that says whether to worry. It says "Watch" every day for a state that is fine, so the owner learns to ignore it, and a real outage then looks the same.
- Fix: count a paired peer as live from its link state (as the Machines read does), or have the link's frames refresh `last_seen_at`. Check which of the two `docs/spec/protocol.md` section 9 means by "`last_seen_at`: the last frame from the peer link" before the fix; the signals read and `tiller machines` should then agree with the board.

### DX-01 - A debug build made before the board was built never served the board (fixed)

- Found while building the bench: a debug `tiller` compiled while `board/dist` was missing answered "board assets are not built" forever, even after the board was built and the crate was rebuilt. rust-embed keeps the folder's path with its `..` parts in that case and then refuses every file, and the compiler cache returns the same artifact.
- Fixed: debug builds read `board/dist` with the daemon's own reader (`assets::board_from_disk`), which does not depend on what existed at compile time. Release builds embed as before.
- Verified: the same stale binary case now serves the board; the hashed asset still answers `text/javascript` and `immutable`; a unit test covers paths that try to leave the build.

## Gates on the bench machine

State on Mon 16 Nov 2026 (Lisbon), on the macOS machine this audit runs on. `cargo fmt --all --check`, clippy with `-D warnings` and `scripts/scrub-check.sh` pass.

`cargo nextest run --workspace` does not pass there. None of the failures comes from this audit's changes: the six below fail the same way on the unchanged code.

| Test | What it reports | Status |
|---|---|---|
| `tiller-cli::startup startup_is_under_ten_milliseconds` | `tiller version` takes a CPU median of about 24 ms against the 10 ms budget | open, cause not found |
| `tiller-cli::hook hook_is_inside_the_ten_ms_budget_with_the_daemon_down` | `tiller hook` takes a CPU median of about 27 ms | open, cause not found |
| `tiller-cli::holder a_slow_subscriber_is_held_up_to_its_queue_then_lagged_and_the_app_never_waits` | "2289048 bytes of 2288895 came, or not in order" | open, cause not found |
| `tiller-cli::holder get_and_screen_read_a_pane_in_text_ansi_and_with_its_scrollback` | fails after 12 s | open, cause not found |
| `tiller-cli::holder_upgrade an_upgrade_keeps_the_pids_the_instance_and_every_pane_field_by_field` | a handover record reads `Short` | open, cause not found |
| `tiller-cli::holder_upgrade a_close_under_way_at_the_upgrade_still_kills_a_child_that_outlives_its_grace` | fails after 15 s | open, cause not found |

Two more failed in the one full run and were not run again alone: `tiller-cli::holder_agents a_lost_holder_resumes_its_worker_and_notifies_the_owner_once` and `tiller-cli::soak a_short_soak_has_no_gap`. Eighteen `tiller-cli::doctor` and `commands` tests failed in that run too and pass with `-j 2`: other builds had the machine at a load average of 22, and those tests have deadlines.

Do not run the bench and the test suite at the same time: the bench starts about 30 processes.

The machine's disk had 4 to 5 GB free during iteration 10 (the bench's seeded "disk floor" case showed the real number). A full workspace build or a Nix build may not fit; check `df -h /` before one.

Next step for these: find out whether they fail on a quiet machine and on Linux. If they fail only here, the cause is outside the repository and it goes to "Needs owner".

## Plan

Order of work, highest owner value first. Fixes that share a cause are grouped.

1. Done as far as the contract allows: UA-16, UA-15 part 1, UA-18 part 1 and UA-19 part 1. The rest of UA-15, UA-18 and UA-19 waits under "Needs owner".
2. Done: UA-04 (a timeline without plumbing lines). UA-11 (no ended run in "Working") moves to the polish step: it is P2, and the column is a contract of `docs/spec/state.md` section 6.
3. Done: UA-02, UA-05, UA-06 and UA-12 (the "needs you" path). The question is on the card, the Inbox is ordered by urgency, and the kind is named.
4. Done: UA-21 and UA-22 (find a card and start a job from anywhere on the board). Done as far as the contract allows: UA-20 part 1 (a queued card says what it waits for). Its "Start" control waits under "Needs owner".
5. Done: UA-01 (a host that refuses every new run says so on the board, in `tiller machines` and in `tiller doctor`).
6. UA-25 (a live peer counts as live on Health), then UA-17 with UA-03, UA-09, UA-10 and UA-23: the CLI as an owner surface.
7. Finish the audit in parallel with the fixes: close the bench gaps (the stand-in forge), then walk journeys 1, 2, 6, 8, 9, 10 and 11 and the rest of 5 and 7.
8. UA-07 with UA-08: the host out of the owner's way.
9. UA-11, UA-13, UA-14, UA-22, UA-24: polish.

## Needs owner

- The test failures in "Gates on the bench machine": six tests fail on this machine without any change of this audit. Recommendation: one iteration finds the cause of the four holder failures (they may be real macOS bugs of the terminal holder); the two 10 ms budgets are judged against a quiet machine.

- UA-15 part 2 (the last P0): `docs/spec/protocol.md` section 10 says that ask text stays on the peer, and replies are not federated. With that rule the owner can never read, on one board, the answer to a question that the same board sent. Today the board says so in the Ask box, the toast and the tile (part 1, done), but the owner must still open the peer's board to read the answer. Recommendation: for a paired host with an `act` key, let the link carry the first line of each ask, an `answered_at` on each `asked` row, and the unread replies, and forward "Mark read". The owner's words already cross the link when the ask is sent. The work after a yes is about one iteration: the snapshot rows, a federated replies read, the forwarded "Mark read", and the tile and Inbox on the board.

- UA-18, the rest: let the owner cancel an ask on the board, and open one. Today a wrong ask leaves the board only when the orchestrator resolves it, or when the owner runs `tiller ask resolve <ask> cancelled` in a terminal. `docs/spec/authority.md` (the matrix row "Resolve an ask, create or edit a card") already gives the owner this right, but `docs/spec/state.md` names the board as an actor of `ask.resolve` only "implicit on add", and `docs/spec/protocol.md` section 7 lists no call. Recommendation: add `POST /api/v1/asks/{id}/cancel` (owner only, forwarded to a peer like the other mutations, `ask.resolve cancelled` with the reason "cancelled by the owner on the board") and `GET /api/v1/asks/{id}` (the full text, the note's delivery state, the replies), and add `answered_at` to the `asked` row (shared with UA-15 part 2). The board work after a yes is about one iteration: a panel that the tile opens, with the text, "Delivered", the answer, "Ask again" and "Cancel".

- UA-19, the rest: let a worker's `blocked` or `needs-decision` report flag its card. Today the card enters "Needs you" only when the orchestrator opens an item, so a worker that stops while its orchestrator is busy, slow or gone shows "Reports blocked" on its tile but is in no count and no Inbox. `docs/spec/state.md` section 6 fixes the flag. Recommendation: do not change the flag. Have the daemon open a `recovery` item when a `blocked` or `needs-decision` report is older than a few minutes and no open item exists for the card (a reconcile rule, so `docs/spec/recovery.md` gets a row). The owner then gets one Inbox item with the worker's words, and the existing flag, counts and notification apply unchanged. Also add `last_status_at` to the agents rows (`docs/spec/protocol.md` section 7.3), so the tile can say how old the report is. The work after a yes is about one iteration.

- UA-20, the "Start" control: let the owner start a run on the board. Today the board files a card and the orchestrator decides when it runs; the owner's own start is `tiller spawn <card> <role>` in a terminal. `docs/spec/authority.md` already gives the owner the right. `docs/spec/protocol.md` section 7 has no call for it. Recommendation: add `POST /api/v1/cards/{id}/runs` (owner only, forwarded to a peer like the other mutations, the same `run.start` as `tiller spawn` with the role from the card's kind and the model and effort from the dispatch table, and the same refusals for capacity, the floors and quota). The board then gets "File and start" in the new-card dialog and "Start" on a queued card, and shows a refusal with its reason. The work after a yes is about one iteration.

- UA-01, the spec text: the fix added the field `refuses_runs` to the Machines row and a `floors` line to `tiller doctor`. `docs/spec/recovery.md` R16 asks for the first ("The Machines row shows the breached floor"), but the field table of `docs/spec/protocol.md` section 9 and the list of checks in its section 6.4 do not name them, and this audit may not edit a spec. Recommendation: add one row to the table (`refuses_runs`: one line per breached RAM or disk floor, absent while the host can start a run; a peer's from its own row while the link is live) and "the floors" to the list of doctor checks. No code changes with a yes.

Two more candidates, to settle when their fix starts:

- UA-03: whether the CLI may reach a peer through the local daemon. Recommendation: yes for the owner's commands.
- UA-07: whether the host chip on every card is wanted. Recommendation: show it only when a host needs attention.
