# Usability audit

The owner's view of Tiller: a busy person with a main machine, one paired peer and 14 active projects, who asks orchestrators questions and starts tasks and leaves. This file is the shared state of the audit. Read it first, every time.

Severity: **P0** blocks the job or breaks trust. **P1** is daily friction. **P2** is polish.

Status values: `open`, `fixed` (with what was verified), `needs owner`, `to verify` (seen once, or possibly an effect of the bench).

## State

| | |
|---|---|
| Phase | 1 - audit. Journeys 3 (ask), 4 (start a task) and 5 (trust) are walked on the board and the CLI. Journeys 1, 2 and 6 to 11 are not walked yet. |
| Open P0 | 1 (UA-15) |
| Open P1 | 12 (UA-01, UA-02, UA-03, UA-04, UA-05, UA-09, UA-16, UA-17, UA-18, UA-19, UA-20, UA-21) |
| Next step | Fix UA-15 with UA-16 (an ask to a peer project must show at once and its answer must reach the owner). Then walk journeys 2, 6 and 8, which need the stand-in forge of the bench gaps. |

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
| `shots DIR [PATH...]` | screenshots of the main board's screens, both themes, 1440 px and 390 px (needs `chrome-devtools-axi`; `DIR` must be outside the repository) |

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
| 9 | Scale | partly: the command palette (UA-21) | no |
| 10 | Watch and steer | no | no |
| 11 | Away mode | no | no |

What the walked journeys cost, on the bench (Mon 16 Nov 2026, Lisbon):

| Job | Board | CLI |
|---|---|---|
| Ask a project | 4 steps, about 10 s: `A`, `@` and two letters, Enter, the text, Enter. Good. | 1 command, 36 lines of answer |
| See that the ask arrived | main project: the tile shows at once. Peer project: nothing until a reload (UA-16) | `ask show` has no delivery state (UA-17) |
| Read the answer | main project: Inbox, 2 clicks. Peer project: never (UA-15) | not possible (UA-17) |
| Ask again | 1 click on "Ask again" in the reply | type the command again |
| Recover from the wrong project | not possible: the ask tile has no action (UA-18) | `ask resolve <ask> cancelled`, which the help calls an orchestrator's tool |
| Idea to a filed card | 3 steps: New card on the project's row, the title, Enter | 1 command, after one refusal for a missing `--kind` (UA-23) |
| Filed card to a running card | not possible: the orchestrator decides (UA-20) | `tiller spawn <card> builder`, 73 lines of answer (UA-09) |
| Learn that a run is blocked or done | only when the orchestrator raises an item; the worker's own report stays under "Runs" (UA-19) | `tiller agents`, in a CSV row |

## Findings

### UA-01 - A host that refuses every new run looks healthy (P1, open)

- Journey: 2, 4, 7, 8.
- Tried: the peer has a disk floor far above its free disk. I looked for any sign of it before starting work there.
- Happened: `tiller spawn` on the peer answers `host peer: disk free is 12862 MB, under the 900000000 MB floor`. Before that, nothing warns. `tiller machines` says `health: ok` and 8 free slots. The board's sidebar says "peer, 10 running, 8 free". The Health screen says Safety "Good" and "Both machines are live". `tiller doctor` does not check the floors.
- Hurts: the owner starts a task and leaves. The task never starts, and the only message is the refusal of a command the owner did not run. "8 free" is false.
- Fix: one "can this host start a run now" verdict from the floors (RAM, disk) and quota, in the host's health. Show it in `tiller machines`, the sidebar's machine row, the Health screen and `tiller doctor`, with the reason and the step ("free disk, or lower the floor in Settings > Hosts"). Free slots read 0 while the host refuses.

### UA-02 - A card that needs you does not show its question (P1, open)

- Journey: 5, 6.
- Tried: on the board I opened `harbor-2`, whose status is "Decision", to answer it.
- Happened: the card panel shows Status "Decision", then Slug, Branch, the worktree's absolute path, a 24-line timeline and the runs. The question, the options, the recommendation and an answer control are not on the card. They are only in the Inbox.
- Hurts: the board says "this card needs you", the owner clicks it, and has to leave the card and find the same item in a list of others.
- Fix: show the card's open questions at the top of the card panel with the Inbox's own answer control (options, keys 1-9, the note field). One component, two places.

### UA-03 - The CLI on main cannot see or reach work on the peer (P1, open)

- Journey: 3, 4, 6, 7.
- Tried: from main, `tiller board`, `tiller recap`, `tiller card show pylon-2`, `tiller ask note pylon --text ...`.
- Happened: `tiller board` lists main's cards only. `tiller recap` lists 4 decisions; the board's Inbox lists 6 (2 are on the peer). `card show pylon-2` answers `no card pylon-2` and `ask note pylon` answers `no project pylon`, with no hint that the project lives on the paired host. `tiller machines` on main shows the peer as `stale` with no slots, while the board shows it live.
- Hurts: the owner must know which machine owns a project, and open a shell on it, to use the CLI. The board hides the host; the CLI makes it the first question. `tiller recap` gives an incomplete morning list without saying so.
- Fix: owner reads and writes in the CLI go through the local daemon when it runs, as the board's do, so a peer's project, card and question resolve by name. Until then, a `not_found` on a name the peer owns says "pylon is on peer" and `tiller recap` says how many items the peer holds. Check `docs/spec` before the fix: if the CLI is host-local by contract, this moves to "Needs owner".

### UA-04 - The card timeline is plumbing (P1, open)

- Journey: 5.
- Tried: read `harbor-2`'s timeline to learn what happened.
- Happened: 24 lines, among them "Tiller deferred a message: not idle" four times, "Tiller delivered a message" five times, "You stamped the session", "You recorded a reservation reserved", "Hook bound a session to the run", "You placed the run on a machine" three times. The two lines that matter ("Orchestrator raised an item for you", "Orchestrator reported the run status") do not say what the item or the status was.
- Hurts: the timeline is the proof behind "done" and "blocked". The owner cannot read it. "You stamped the session" credits the owner with steps the system took.
- Fix: the default timeline shows owner events with their content (the status line, the question, the answer, the PR, the landing). A "Show system events" switch shows the rest. Events a command made in one step fold into one line.

### UA-05 - The Inbox ignores priority (P1, open)

- Journey: 2, 6.
- Tried: opened the Inbox to learn what needs me first.
- Happened: 7 items, newest first. The only P0 Urgent card (`harbor-2`) is last. A reply to a question I asked sits in the middle.
- Hurts: with 14 projects the list is long. The owner answers what is on top and the urgent item waits.
- Fix: order by card priority, then by how long the item has waited (oldest first). Show the priority on the row when it is P0 or P1.

### UA-06 - Every Inbox row says "decision" (P2, open)

- Journey: 2, 6.
- Happened: a credential request, a recovery question and a scout report all show "decision" in the row and "Decision" in the away brief. `tiller board` and `tiller recap` name the real kind.
- Hurts: "issue a new key" (I must do something elsewhere) and "pick A or B" (one key press) look the same.
- Fix: show the item's kind with the words the CLI uses, and a distinct icon for credential and recovery.

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

### UA-12 - The Inbox row's age wraps to its own line (P2, open)

- Journey: 2.
- Happened: at 1440 px, rows with a longer project and card name ("Lantern / lantern-1 / decision /") push the age ("3m") to a second line, after a hanging "/".
- Fix: keep the age on the right edge of the row, outside the wrapping text.

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

### UA-15 - The answer to an ask sent to a peer project never reaches the owner (P0, open)

- Journey: 3, 7.
- Tried: on the main board, `A`, `@Pylon`, "What port does the gateway listen on?", Enter. The toast said "Sent to the Pylon orchestrator". Pylon's orchestrator answered with `tiller reply`.
- Happened: the answer never shows on the main board. The Inbox's "Replies" count stayed at 2 (both from main projects) for more than 45 s and after a reload. The ask's tile under Pylon shows no question text, only "Waiting for the orchestrator to tag this ask", so two asks to one peer project cannot be told apart. The answer exists only on the peer's own board.
- Cause: by design today. A peer's board rows carry an ask's source but not its text (`docs/spec/protocol.md` section 10, the `snapshot` frame: "ask text stays on the peer"), and replies are read per project from this host only (`board/src/lib/queries.ts`, "Never federated: a peer's replies stay on the peer"; `docs/design/cli-board-parity.md`, "Mark a reply read ... does not forward the call to a peer").
- Hurts: the Ask box offers every project, accepts the ask, confirms it, and then the owner waits for an answer that cannot arrive. This is the owner's first daily job, and it fails without a word for 6 of 14 projects.
- Fix, in two parts:
  1. Inside the contract (no spec change): the main board learns from the peer's `ask.replied` event that an answer exists. The Inbox then shows a row "Pylon answered your ask", and the tile says "Answered". Until part 2, the row opens the answer on the peer's board. The Ask box says so before the send when a target is a peer project.
  2. With a spec change (see "Needs owner"): the link carries the ask's first line and the unread replies of a paired host, and "Mark read" forwards. The text already crossed the link once, from this board to the peer, when the owner sent it.

### UA-16 - An ask sent to a peer project does not show on the board until a reload (P1, open)

- Journey: 3, 7.
- Tried: sent an ask to Pylon from the Ask box, then looked for it. Later, recorded an ask on the peer and watched the open main board.
- Happened: the "Asked" count and Pylon's row did not change for 25 s and for 20 s. After a reload the tile was there. A new card on the same peer project showed in 6 s without a reload, so the link works: only ask changes of a peer are missed. `board/src/lib/sse.ts` does refetch the board on an `ask` event, so the event of a peer's ask does not reach the browser, or the cached peer board is not replaced when it comes.
- Hurts: "Sent" and then nothing. The owner sends it a second time, or reloads to check.
- Fix: find where the peer's `ask.recorded` and `ask.resolved` events stop (the peer link's event relay, or the snapshot cache in the daemon), and add a test in `crates/daemon/tests/peer_e2e.rs` that a peer's new ask reaches the board host's `/api/v1/board`.

### UA-17 - The CLI cannot read an answer (P1, open)

- Journey: 3.
- Tried: `tiller ask note lantern --text ...`, then `tiller ask show <ask>` and `tiller ask list` after the orchestrator replied.
- Happened: `ask show` prints the ask and `cards[0]`. It has no reply, no "answered" mark and no delivery state of the note. `ask list` shows `unresolved` for the answered ask. `tiller recap` does not list replies. No command prints the reply text.
- Hurts: a job that starts in the CLI cannot end there. The owner must open the board's Inbox to read the answer to a question typed in a shell.
- Fix: `ask show` prints the note's delivery state and every reply. `ask list` gets an `answered` column. `tiller recap` lists unread replies beside the decisions, and `tiller reply read <reply>` (or `ask show --mark-read`) marks one read, so the parity row "Mark a reply read" becomes Full on both sides.

### UA-18 - An ask tile is a dead end, and its status line can be wrong (P1, open)

- Journey: 3.
- Tried: clicked my ask's tile in the "Asked" column, before and after the answer came.
- Happened: the tile is not a link or a button. It cannot be opened, cancelled or moved to another project. After the orchestrator answered, the tile still says "Waiting for the orchestrator to tag it", because the ask keeps the disposition `unresolved` until the orchestrator runs `ask resolve`. The delivery state of the note (pending, delivered, read by the agent) is not shown.
- Hurts: an ask sent to the wrong project stays on the board with no way out. The CLI has one (`ask resolve <ask> cancelled`). An answered question reads as unanswered. "Tag it" is an internal word.
- Fix: the tile opens the ask: the full text, when it was delivered, the answer when there is one, "Ask again" and "Cancel". The tile's line follows the facts: "Sent", "Delivered", "Answered" (with the first line of the answer), and never "tag".

### UA-19 - A worker's own report does not change what the card says (P1, open)

- Journey: 5.
- Tried: started a builder on `lantern-5`. As the worker, reported `working: Wrote the header builder; 14 of 20 tests pass`, then `blocked: The staging mail server refuses the test login`, then `done: All 20 tests pass`.
- Happened: after each report the tile and the card's Status say "Idle". The Inbox count and "Needs you" do not move. The status line is on the card, but in the "Runs" section under a timeline of 14 to 26 lines, off the first screen at both widths. The timeline says "Orchestrator reported the run status" for the worker's report, without the text. A card shows "blocked" or "needs you" only when the orchestrator raises an item (the seeded `lantern-1` shows "Credential").
- Hurts: this is the "start a task and leave" job. The worker says it is blocked, and the board says "Idle". If the orchestrator is busy, slow or gone, the owner never learns it. "Idle" is the agent's terminal state, not the state of the work.
- Fix: the tile and the Status row show the newest report of the run: its state as the badge ("Working", "Blocked", "Reports done") and its line as the tile's second line, with its age. A `blocked` or `needs-decision` report puts the card in "Needs you" even before the orchestrator acts. The timeline names the worker and quotes the line. Check `docs/spec/state.md` first: if the column and the "needs you" rule are a contract, the badge and the line are still free to change.

### UA-20 - On the board a new task stops at "Filed" (P1, open)

- Journey: 4.
- Tried: "New card" on Lantern's row, a title, Enter. Then opened the card to start it.
- Happened: the toast says "Filed lantern-4". The card sits in "Queued" with the badge "Filed", Model "none yet", and "No runs yet. The orchestrator starts one when the card leaves the queue." Nothing says when that is, on which machine, or with which model. There is no "Start" control: `tiller spawn` exists only in the CLI (`docs/design/cli-board-parity.md`, "Run and session control ... No"). The one filled button on a card that is seconds old is "Close".
- Hurts: "from an idea to a running card" ends one step short on the board, and the owner cannot tell whether the next step is theirs. The loudest button does the opposite of what the owner came for.
- Fix: the new-card dialog gets "File and start" as its main action, which files the card and starts the run the dispatch rule picks, and the card then says what was started ("Builder, sonnet, on main, started 10 s ago"). A queued card says what it waits for ("Waits for the orchestrator's next turn", "Waits for a free slot", "Waits on harbor-1"). On an open card "Close" moves into "More", and the main action is "Start". This adds an owner mutation to the board, so check `docs/spec/authority.md` before the work; if the board may not start a run by contract, it moves to "Needs owner".

### UA-21 - "Search or run" finds no card and runs no job (P1, open)

- Journey: 3, 4, 9.
- Tried: `Cmd+K`, then "digest" (a word of a card's title), "lantern-1" and "new".
- Happened: "No command matches". The palette holds navigation, theme, density, "Focus <project>", "Hide <project>" and two machine filters. It has no card, no session, no "New card", no "Ask", no "Away".
- Hurts: with 14 projects and 43 cards, the only way to one card is to scroll the board. The field's own label promises search.
- Fix: the palette searches cards (id and title), projects and agents on both hosts, and runs the owner's jobs: "Ask <project>", "New card in <project>", "Turn on away mode", "Open the Inbox item for <card>".

### UA-22 - The view of one project has no "New card" (P2, open)

- Journey: 4.
- Happened: "New card" is on each project's row in "All projects". After a click on one project in the sidebar, the board shows that project alone, and the button is gone. The header has "Show all projects", the filters, "Away" and "Ask".
- Fix: keep "New card" in the header of the one-project view, and give it a key (`C`) next to `A` for Ask.

### UA-23 - Small CLI refusals with no next step (P2, open)

- Journey: 3, 4.
- Happened: `tiller card create lantern --title ...` is refused with "a new card needs --kind ship|scout|question|ops"; the help lists `--kind` as an option, and the board defaults it to ship. `tiller ask note lantrn` answers "no project lantrn" with no near match. `tiller status "text"` fails on a parse error because the state comes first.
- Fix: `--kind` defaults to `ship` as on the board. A `not_found` for a project names the nearest id ("did you mean lantern?"), and names the peer when the project lives there (UA-03).

### UA-24 - Small board defects seen in journeys 3 to 5 (P2, open)

- The toast ("Sent to the Pylon orchestrator", "Filed lantern-4") covers the "All / Active / Needs you" filter at 1440 px.
- The new-card dialog explains itself with a command ("Files a card, as `tiller card create` does") and shows the raw kind "ship".
- The card panel has two event lists: "Timeline" and, at the bottom, "Events" with raw kinds (`outbox.delivered BX6YJFV`, `session.stamped`). The second belongs behind the "Show system events" switch of UA-04.
- A new card's panel shows the worktree's absolute path and the slug before anything the owner needs.
- At 390 px the "x" that removes a project in the Ask box wraps to a line of its own.
- To verify: the Ask box said "will start orchestrator" for Lantern, whose orchestrator was running, right after a page load. The label may read "not running" while the agents list still loads (`board/src/components/shell/ask-dialog.tsx`, `statusOf`).
- The Inbox counts unread replies in "Needs you 8". A reply is news, not a job for the owner.

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

Next step for these: find out whether they fail on a quiet machine and on Linux. If they fail only here, the cause is outside the repository and it goes to "Needs owner".

## Plan

Order of work, highest owner value first. Fixes that share a cause are grouped.

1. UA-15 part 1 with UA-16 and UA-18: an ask is honest from end to end. A peer ask shows at once, the tile opens, an answer is announced, and the wrong ask can be cancelled.
2. UA-19 with UA-04 and UA-11: what a card says about its own progress. The worker's report on the tile, a readable timeline, no ended run in "Working".
3. UA-02 with UA-05 and UA-06: the "needs you" path. The question is on the card, the Inbox is ordered by urgency, and the kind is named.
4. UA-20 with UA-21 and UA-22: start a task from anywhere on the board.
5. UA-01: an honest host verdict in every place that shows a host.
6. UA-17 with UA-03, UA-09, UA-10 and UA-23: the CLI as an owner surface.
7. Finish the audit in parallel with the fixes: close the bench gaps (the stand-in forge), then walk journeys 1, 2, 6, 8, 9, 10 and 11 and the rest of 5 and 7.
8. UA-07 with UA-08: the host out of the owner's way.
9. UA-12, UA-13, UA-14, UA-22, UA-24: polish.

## Needs owner

- The test failures in "Gates on the bench machine": six tests fail on this machine without any change of this audit. Recommendation: one iteration finds the cause of the four holder failures (they may be real macOS bugs of the terminal holder); the two 10 ms budgets are judged against a quiet machine.

- UA-15 part 2: `docs/spec/protocol.md` section 10 says that ask text stays on the peer, and replies are not federated. With that rule the owner can never read, on one board, the answer to a question that the same board sent. Recommendation: for a paired host with an `act` key, let the link carry the first line of each ask and the unread replies, and forward "Mark read". The owner's words already cross the link when the ask is sent. Part 1 of UA-15 needs no decision and comes first.

Two more candidates, to settle when their fix starts:

- UA-03: whether the CLI may reach a peer through the local daemon. Recommendation: yes for the owner's commands.
- UA-07: whether the host chip on every card is wanted. Recommendation: show it only when a host needs attention.
