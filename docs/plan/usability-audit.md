# Usability audit

The owner's view of Tiller: a busy person with a main machine, one paired peer and 14 active projects, who asks orchestrators questions and starts tasks and leaves. This file is the shared state of the audit. Read it first, every time.

Severity: **P0** blocks the job or breaks trust. **P1** is daily friction. **P2** is polish.

Status values: `open`, `fixed` (with what was verified), `needs owner`, `to verify` (seen once, or possibly an effect of the bench).

## State

| | |
|---|---|
| Phase | 3 - the full walk. Every journey is walked once on the board and the CLI in this phase (Mon 16 Nov 2026, Lisbon). A second pass has begun, with one board page left open and no page load: journeys 3, 5, 6, 7 and parts of 1, 8 and 11 are walked that way (see "The open-page pass" under Journeys). It found three new P1: UA-42 and UA-43 are fixed as far as the contract allows, and UA-44 is fixed (iteration 23, walked again on both hosts with the page left open). Iteration 24 walked the rest of that pass (journeys 2, 4, 9 and 10, and for journey 8 the peer offline and quota out): every change reached the open page, and the pass found one more P1, UA-46, fixed as far as the contract allows. The open-page pass is complete. Iteration 25 walked all 11 journeys once more, on the board and the CLI (the third pass). It found two new P1, UA-48 and UA-49, both fixed on the board, so one more full pass is still to do. Iteration 26 began the fourth pass with the paths that no pass had taken (the card's "More" menu, a scout report, "Reject" on a peer's pull request, the `local-only` landing, four screens at 390 px). It found one new P0, UA-53, and two new P1, UA-51 and UA-52. All three are fixed as far as the contract allows, so one more full pass is still to do. Iteration 29 began a fifth pass (journeys 2 to 5, and the `local-only` landing again). It found one new P0, UA-57, which needs owner, and two new P1, UA-55 and UA-56, both fixed. |
| Open P0 | 1, `needs owner`: UA-57 (on a `local-only` project, once Tiller lands work in the checkout, every new run of the project is refused: the spawn starts from `origin`, and Tiller never pushes there). The fix changes the `base_sha` contract of `docs/spec/state.md`, so it waits under "Needs owner". UA-53 (a `local-only` project that names no repo could never land work: the request for the landing was refused with "project has no repo to bind") was found in the fourth pass and is fixed in the store. UA-15 is `needs owner`: the silent part is fixed, the answer itself needs a spec change. |
| Open P1 | 0. UA-55 (the first request of a direct landing was bound to an old head, so the owner approved twice) and UA-56 (after a second request, the card stayed in "Needs you" with the older question when the newer approval landed, and never closed) were found in the fifth pass and are fixed. UA-51 ("Hold" on the board promised "starts no new run", but no rule refuses a start on a held card and the orchestrator was not told: its start went through) and UA-52 (on the page of a peer's card, Release, Hold, Close, Cancel, Edit, the priority and "Depends on" went to this host and failed with "no card <id>") were found in the fourth pass. UA-52 is fixed on the board. UA-51 is fixed as far as the contract allows: the board now tells the orchestrator, and the refusal of the start is `needs owner`. UA-48 (in the Ask box `@la` and Enter took Atlas, not Lantern: a name that only holds the letters came before a name that starts with them, and an ask to the wrong project cannot be cancelled on the board) and UA-49 ("Close", then "Done", on a ship card always ended in a raw refusal, "landed needs a succeeded merge or land approval...", with no next step, and a dim "Close card" button said nothing) were found in the third pass and are fixed on the board. UA-46 (the owner answered "Start now anyway" on a quota question, and nothing told the orchestrator which command starts the run: its plain start was refused and the owner got the same question again) was found in the last part of the open-page pass and is fixed in the answer's note, the refusal and the orchestrator's role text; what a plain start does with a standing answer is `needs owner`. UA-44 (a merge that fails said nothing, and a grant tried it again every 15 s without end) is fixed in the daemon, on the board and in the CLI. It was found in the open-page pass, with UA-42 (the Ask box said "will start orchestrator" for every live project of this host on a paired board, and the toast said one was started; fixed on the board) and UA-43 (a peer's project has no grants control on main's board and no word about where grants are; the panel now says so, and grants for a peer from one board are `needs owner`). UA-40 (with the Inbox open, a permission prompt never came to the list, and an answered one never left it, until a new page load; for a peer's agent the same) was found in the walk of the last journey 8 case and is fixed on the board. UA-38 (failed checks after an approval left an item whose Approve and Reject could only fail, with a receipt and a banner that said the head moved) was found in the walk of the rest of journey 6 and is fixed on the board and the CLI; how that note leaves the list is `needs owner`. UA-36 (an approval asked the owner to merge a commit hash, with no pull request, no link and no checks, and the card pointed to a section far down the page) was found in the walk of journey 6 and is fixed on the board and the CLI. UA-33 (first run: the start that `project add` names was refused once per missing folder, and the board ended in a toast) was found in the walk of journey 1 and is fixed on the board and the CLI. UA-26 (a run whose agent died read "Unknown" and "running") and UA-30 (away mode covers this host only and did not say so) were found in the phase 3 walk. UA-26 is fixed on the board. UA-30 is `needs owner`: the board and the CLI now say what away mode leaves out, and away mode for a paired host from one board needs a spec change. UA-03 is `needs owner`: the CLI on main now says that it reads main only and prints the command for the peer, and reads and writes of a peer's work through the local daemon need a spec change. UA-09 is `needs owner`: seven owner commands end with a sentence and the next step, and a short answer in place of the record needs a spec change. UA-17 is fixed but for "Mark read" in the CLI, which is `needs owner`. UA-20 is `needs owner`: a queued card says what it waits for and "Close" is out of the main place, and its "Start" control needs a spec change (see "Needs owner"). UA-18 is `needs owner`: the wrong status line is fixed, Cancel and the full view need a spec change. UA-19 is `needs owner`: the tile, the card and the timeline show the worker's report, and the "Needs you" rule needs a spec change. |
| Fixed | UA-01, UA-02, UA-04, UA-05, UA-06, UA-12, UA-16, UA-21, UA-22, UA-25, UA-26 (on the board), UA-33, UA-36, UA-38 (the note's exit needs owner), UA-40, UA-42, UA-44, UA-46 (the second question needs owner), UA-48, UA-49, UA-52, UA-53, UA-55, UA-56, DX-01. UA-51 part 1 (a hold or a release made on the board is told to the orchestrator; the CLI and the role text say that a start on a held card is not refused). UA-43 part 1 (a peer's project panel says that its grants and its stage switch are on the peer's own board). UA-30 part 1 (the away dialog, the away popover, the return brief and `tiller away` name the paired host that away mode does not cover). UA-17 (read an answer in the CLI; "Mark read" in the CLI needs a spec change). UA-15 part 1 (the owner is told where the answer shows). UA-18 part 1 (the ask tile shows an unread answer and opens it). UA-19 part 1 (a worker's report shows on its tile, on the card and in the timeline). UA-20 part 1 (a queued card says what it waits for, and "Close" is under "More" until work starts). UA-09 part 1 (a `summary` sentence and the owner's next step at the end of `init`, `project add`, `orchestrator start`, `card create`, `spawn`, `ask note` and `attention answer`). UA-03 part 1 (a `not_found` and every host-wide list in main's CLI name the paired host and print the `ssh -t` command for it). |
| Next step | One P0 is open and waits on the owner (UA-57). The fifth pass (iteration 29) is not complete and found one new P0 and two new P1, so the stop condition is not met. Finish the fifth pass on a new bench: journeys 1, 6, 7, 8, 9, 10 and 11, on the board and the CLI. Do not land work twice in the same `local-only` project on the bench, because UA-57 refuses its next run. The pass must find no new P0 or P1. Earlier text of this step follows. The fourth pass (iteration 26) is not complete and found one new P0 and two new P1, all fixed as far as the contract allows. Do one full pass of all 11 journeys on a new bench, on the board and the CLI, that must find no new P0 or P1; only then is the stop condition met. Iteration 26 walked only the paths in "The fourth pass" below, so the next pass must walk every journey from its start. Take every button to its end, follow each owner answer to the agent that receives it (`as HOST SESSION inbox`; read it once, since the read acknowledges the message), and do each owner action on a peer's card too, from the card's own page (`/cards/<id>?owner=peer`): UA-52 hid there for 25 iterations. Check first the one thing that no pass could settle (UA-54): the board's reads that stayed pending for 20 to 40 s, twice. The fifth pass saw no pending read. The other item, the first approval bound to an old head, is UA-55 and is fixed. Paths not taken yet: a scout card to "Promote" with a real report (the bench's scout has no worktree left), "Edit" and "Cancel card" on a peer's card, a grant on a `local-only` project, the dialogs of the "More" menu at 390 px. P2 work after that: UA-27 to UA-29, UA-31, UA-32, UA-34, UA-35, UA-37, UA-39, UA-41, UA-45, UA-47, UA-50, UA-54, then UA-07 with UA-08, UA-10, UA-11, UA-13, UA-14, UA-23, UA-24. |

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
| `link freeze\|thaw` | the peer stops answering, then answers again. A freeze lasts about 2 minutes: the link then starts a new `peer serve` |
| `quota HOST PERCENT` | the weekly quota the next poll reads |
| `kill-agent HOST SESSION` | a run that dies |
| `prompt HOST SESSION [COMMAND [--no-hook]]` | that session's agent stops at Claude Code's permission dialog for the shell command `COMMAND`, and the two hooks that Claude Code fires with it are sent. `1`, `2` or `3` in its pane answers it; the board's answer buttons type them. With `--no-hook` no hook reports it: a dialog that only the screen shows |
| `checks HOST PROJECT NUMBER success\|failure\|pending` | the checks of a pull request on the stand-in forge; the daemon reads them within a minute |
| `shots DIR [PATH...]` | screenshots of the main board's screens, both themes, 1440 px and 390 px (needs `chrome-devtools-axi`; `DIR` must be outside the repository). A path may be a card page: `"/cards/<ulid>?owner=peer"` |

What it builds:

- Two homes under `/tmp/tiller-bench` (`TILLER_BENCH_DIR`), `main` and `peer`, each with its own holder, its own daemon on a free port, and an empty environment. The real `~/.tiller` and port 7421 are out of reach.
- The pairing runs the real `tiller peer add` through an SSH shim, as `crates/daemon/tests/peer_e2e.rs` does.
- Agents are `fake-agent`. Stand-ins replace `claude`, `treehouse` (git worktrees under the pool root), `quota-axi` (64% of the week left) and `gh`.
- The stand-in forge (`scripts/usability-bench-gh.sh`, run as `TILLER_GH`) answers the `gh api` calls of the daemon with no network: open, read and edit a pull request, its files, checks and reviews, the merge, and the reads after it. A repository `example/<id>` is the project's bare repository under `origins/`, a pull request is a JSON file under `forge/<id>/`, and a merge is a fast-forward of the bare repository's `main`. Harbor and Juniper on main and Pylon on peer use it with `direct-pr` delivery; the other projects are `local-only`.
- 14 projects (`scripts/usability-bench-seed.sh`): Harbor, Lantern, Quill, Ember, Atlas, Juniper, Cobalt and Meadow on main; Pylon, Tundra, Saffron, Orbit, Willow and Basalt on peer. Each has an orchestrator. The mix: 11 runs (live, blocked, needs-decision, one failed, two reported scouts), three pull requests (`harbor-1` and the peer's `pylon-1` wait for the owner's approval, `juniper-1` waits for its checks), 6 more questions for the owner, 3 unanswered asks and one answered ask, parked and dependent cards, closed cards, an empty project, a past away period on main, and a peer whose disk floor refuses every new run.

The owner stand-in: a release `tiller` takes an owner command only from a terminal with no agent behind it. The bench builds the debug binary with the `test-owner` feature that the command tests use, and sets `TILLER_TEST_OWNER` for its own homes only. So the CLI on the bench is the owner with no terminal: an ask from the CLI may show as sent by a script.

Bench gaps (fix before the journeys that need them):

| Gap | Journey | Plan |
|---|---|---|
| No `land_direct`. (Closed in iterations 19 and 20: approvals, landing, a grant, a rejection, a moved head and failed checks are walked by hand on the stand-in forge.) | 6, 7 | The steps of the walk: `checks HOST PROJECT 1 failure` for failed checks; for a moved head, commit in the run's worktree (`.data.runs[-1].run.worktree_path` of `card show`) and `git push origin HEAD`; for a grant, "Grants and settings" on Juniper's sidebar row, then `checks main juniper 1 success`. |
| Every agent shows Idle. `fake-agent` does no work, so "progress" is only what the bench reports with `tiller status`. | 5, 10 | Type into two panes during the walk (`fake-agent` turns run 4 s), or add a looping driver. |
| No answer comes back to an ask by itself. | 3 | Answer as the orchestrator with `as HOST SESSION reply`. Use a real model at most once per iteration, only to judge the answer's quality. |
| The away period on main lasts under a second. | 11 | Walk away mode by hand during journey 11. |
| (Closed in iteration 21.) `fake-agent` paints Claude Code's permission dialog while the file named by `FAKE_AGENT_DIALOG` exists, and `prompt HOST SESSION` writes that file and sends the hooks. It sends no hook after the answer: the holder's read of the screen ends the wait, within about 2 s. | 8 | The steps of the walk: `prompt main $(session main harbor harbor-3)`, then the Inbox row "Bash: waiting for a keypress" and an answer button; the same with a peer's session; `--no-hook` for a dialog the board cannot read. |
| `quota HOST PERCENT` takes up to 5 minutes: the daemon polls quota every 300 s. | 8 | Run `daemon HOST restart` after it; the daemon polls at its start. |

## Journeys

| # | Journey | Walked on the board | Walked on the CLI |
|---|---|---|---|
| 1 | First run | yes, phase 3, on a third and a fourth empty home beside the bench: the first board, "Add project", "Start orchestrator" (UA-33, UA-34). Pairing: a look at Settings > Hosts > "Add a host" only; the bench pairs by script | yes, phase 3: `tiller init`, `project add`, `orchestrator start` up to the launch (UA-33, UA-34). `peer add` is the bench script's own step |
| 2 | Morning check | yes, phase 3 (UA-28; the Inbox does the job in one click), and in the open-page pass | yes, phase 3 (UA-27) |
| 3 | Ask | yes (UA-15 to UA-18, UA-21, UA-24), and in the open-page pass (UA-42) | yes (UA-17, UA-23) |
| 4 | Start a task | yes (UA-20, UA-21, UA-22), and in the open-page pass | yes (UA-09, UA-23) |
| 5 | Trust | yes: progress, blocked and done reports (UA-19), a run that dies (UA-26, in journey 8), and in phase 3 a pull request from "Reports done" to "Landed" (UA-36, UA-37) | partly: the same reports; `tiller land` prints the result (UA-36) |
| 6 | Decide | yes, phase 3: answer items (UA-02, UA-05), approve a pull request in the Inbox and on the card, watch it land (UA-36, UA-37), reject one, approve after a push moved the head, failed checks before and after an approval (UA-38), a grant made in "Grants and settings" that landed `juniper-1` with no question (UA-39) | yes, phase 3: `recap`, `approve`, `land` (UA-36, UA-37), `approve` and `land` after failed checks (UA-38), `grant create` and `grant list` (UA-39) |
| 7 | Peer transparency | partly: an ask to a peer project (UA-15, UA-16); in phase 3 a peer's approval, decided on main's board and landed by the peer: the same views and the same two clicks as for main (UA-36); a push to the peer's pull request asked again at the new head within 10 s, and the approval in main's Inbox merged on the peer in 13 s. A grant on the peer: main's board has no control for it (UA-43), and the grant made in the peer's CLI met a pull request that could not merge (UA-44; fixed and walked again: the peer's failed merge came to main's Inbox in 3 s, and the fixed head landed under the grant in 24 s) | first look only (UA-03: main's CLI reaches the peer through `ssh -t`) |
| 8 | Failure | yes, phase 3: a daemon restart, the peer offline, a run that dies (UA-26), quota out (UA-29), and an agent that waits on a prompt, on main and on the peer, with answer buttons and as a dialog the board cannot read (UA-40, UA-41); in the open-page pass the peer offline (UA-47) and quota out, with the answer followed to the orchestrator (UA-46) | yes, phase 3: the same cases (UA-26, UA-29); a prompt shows in `card list --needs-you` and `tiller agents`, not in `tiller recap` (UA-41) |
| 9 | Scale | yes, phase 3: one card, one project and one agent among 16 lanes, 39 cards and 23 agents, by the palette, the Focus view, the filters and the keys, at 1440 px and 390 px (UA-35, P2 only), and in the open-page pass | yes, phase 3: `tiller board`, `card list --project`, `--column`, `--needs-you`, `tiller agents` (UA-35, P2 only) |
| 10 | Watch and steer | yes, phase 3: a main agent and a peer agent, watch, type, leave (UA-32, P2 only), and in the open-page pass, with a run that started after the page opened (UA-47) | yes, phase 3: `tiller send --kind steer`, and `tiller attach` in a pseudo terminal with a typed line and a detach (UA-32, P2 only) |
| 11 | Away mode | yes, phase 3: start, the status, edit, end, the brief (UA-30, UA-13, UA-31) | yes, phase 3: `away start`, the spend cap, `away brief`, `away end` (UA-30, UA-31) |

### The open-page pass

The second pass of phase 3 (iteration 22) keeps one page of main's board open and never loads it again. Each change is made in the CLI, or on the board itself, and the open page is read with `eval`. What came to the open page, and how fast:

| Change | Main's work | The peer's work |
|---|---|---|
| A card filed in the CLI | at once | at once |
| A run started; a worker's `working` and `blocked` reports | 1 to 2 s | 2 s |
| A decision opened by the orchestrator: the tile, the sidebar count, "Needs you", the Inbox row | 1 s | 2 s |
| The same item answered in the CLI: the row leaves, the counts drop | at once | 1 s |
| An agent killed: "Agent stopped" on the tile | 21 s | 19 s |
| An ask sent from the Ask box, and the orchestrator's reply on its tile and in the Inbox count | at once, then 1 s | not walked (UA-15) |
| Failed checks before an approval: Approve turns off, and on again after a pass | 25 s each way | not walked |
| An approval on the board: "Recorded", then "Merged"; the card reads "Landed" | 9 s | - |
| An approval in the peer's CLI: the row leaves main's Inbox, the card reads "Landed" | - | at once |
| A second pull request opened by a worker: "PR 2" on the tile | - | 1 s |
| Away mode started and ended in the CLI: the header | 1 s each way | - |
| A project added in the CLI: the sidebar row | at once | not walked |
| A merge that the forge refused, after an approval on the board: the banner "Nothing merged", the question with the reason, the timeline line (UA-44) | 10 s | the row in main's Inbox: 3 s |
| A fixed head pushed after a failed merge, under a grant: the card lands and the question leaves the Inbox | not walked on a fixed binary | 24 s |
| A daemon restart: the banner comes, leaves, and events arrive again | banner gone within 1 s of the start; a new card of each host showed 1 s after | |

| An orchestrator's reply to an ask: the Inbox count and the "Replies" row, with the Inbox open (journey 2) | at once | not walked (UA-15) |
| A card filed on the board with `C`, then its run started in the CLI and the worker's report (journey 4) | the tile at once, "Filed"; the run 1 s; the report 1 s; the open card's status, timeline and run row follow | not walked: the peer's disk floor refuses every new run |
| A card filed in the CLI: found in the palette by a word of its title (journey 9) | under 2 s | under 2 s |
| A run started after the page opened: found in the palette, its screen opened, a line typed to it (journey 10) | the agent shows under "Agents" in the palette at once; the typed line shows on its screen in under 2 s | a seeded agent of the peer: the same |
| The peer stops answering (journey 8) | - | 25 s: every lane of the peer reads "as of Mon 10:42 ... peer is not answering since Mon 10:42", and the open screen of a peer's agent reads "Unknown, host not answering" with the same line above it |
| The peer answers again | - | 4 s: the lanes and the agent's screen are live again |
| Quota at 2% (after the daemon's next poll): the sidebar's "2% week left" | 3 s | not walked |
| A start that the quota floor refuses: the decision in the open Inbox | under 3 s | not walked |
| "Start now anyway" answered on the board, then the run started by the orchestrator: the receipt's "Done" line | "in progress..." at once; "The orchestrator read your answer" within 3 s of the orchestrator's read | not walked |

No row of this table needed a page load. The findings of the pass are UA-42 to UA-47.

The open-page pass is complete (iteration 24).

### The third pass

Iteration 25 walked all 11 journeys again on a new bench (Mon 16 Nov 2026, about 10:55 to 11:15, Lisbon), on main's board at 1440 px and in the CLI, with the peer's work through main's board.

| # | What was walked | Result |
|---|---|---|
| 1 | A third empty home: `tiller init`, `project add`, `orchestrator start`; its first board, "Start orchestrator" | as recorded (UA-33 holds; UA-34) |
| 2 | The Inbox in one click (8 items of both hosts, the urgent one first, 1 reply, the away brief); `tiller recap` | as recorded (UA-27) |
| 3 | `A`, `@la`, Enter, the text, Enter; the orchestrator's reply on the tile within 2 s and in the Inbox; "Ask again"; `ask show`; `ask resolve ... cancelled` for the wrong ask; an ask to a peer's project | new: UA-48 (P1, fixed) |
| 4 | `C`, "me", Enter, a title, Enter: the tile at once; the orchestrator's start 2 s later on the open card | as recorded (UA-20) |
| 5 | The worker's `working`, `blocked` and `done` on the tile and the card within 2 s; the orchestrator's question at the top of the card; the `1` key; the receipt to "The orchestrator read your answer"; "Close" on the card after "Reports done" | new: UA-49 (P1, fixed) |
| 6 | Approve `harbor-1` in the Inbox ("Merged" within 14 s); a grant for Juniper in 4 clicks, `juniper-1` landed within 30 s of its checks; `attention answer` in the CLI, with the note read as the orchestrator | as recorded |
| 7 | Approve the peer's `pylon-1` in main's Inbox ("Merged" within 14 s); answer the peer's decision with one key, with the note read as the peer's orchestrator | as recorded (UA-15, UA-43) |
| 8 | A daemon restart (the banner within 8 s, gone within 6 s of the start); a killed agent ("Agent stopped" after 35 to 60 s, "Unknown" before that); a permission prompt answered on the board; quota at 2% with "Start now anyway" followed to the start; the peer frozen (answers off with the reason within 30 s, on again within 6 s) | as recorded (UA-46 holds) |
| 9 | The palette: a card by a word of its title, "focus wi" for one project, an agent by its card id | as recorded |
| 10 | The agent's screen from the palette, a typed line, "Done"; `tiller send --kind steer` | as recorded (UA-32) |
| 11 | "Away" with the defaults, a run started under it, the header's status, "End", the brief | new: the status kept "Runs 0 of 6" after a run started (P2, fixed, in UA-50) |

The changed close dialog was also checked at 390 px in the light theme.

### The fourth pass (not complete)

Iteration 26 began a fourth pass on a new bench (Mon 16 Nov 2026, about 11:15 to 11:37, Lisbon). It took the paths that no earlier pass had taken, on main's board at 1440 px and at 390 px, with each answer followed to the agent that receives it. It did not walk journeys 1, 2, 3, 4, 9, 10 and 11 again, so it does not count as a full pass.

| Journey | What was walked | Result |
|---|---|---|
| 5, 6 | A scout card from its report: the question at the top of the card, an answer in the owner's words, the note as the orchestrator reads it | good. The report itself is not on the card (UA-54) |
| 4, 5 | "Edit" in the card's "More" menu: a new title and a body | saved at once. The orchestrator gets no note (UA-54) |
| 5 | "Hold" on a queued card, then a start by the orchestrator | new: UA-51 (P1). The start went through |
| 5 | "Release" on a held card | good on main |
| 7 | "Release" on a peer's held card, from the card's page on main's board | new: UA-52 (P1). "no card <id>" |
| 5 | "Cancel card" on a card with a live run, then "Reopen" | the card closes and opens again. The agent of the cancelled run stays live until the orchestrator stops it (UA-54) |
| 7 | "Reject" on the peer's pull request in main's Inbox | one click, the receipt reads "Rejected", and the peer's orchestrator reads "answer: reject". No reason is asked (UA-37) |
| 5, 6 | The `local-only` landing: a builder's commit, the orchestrator's request, "Approve" on the card, the merge in the checkout | new: UA-53 (P0). The request was refused for a project with no repo. After the fix: landed, in two approvals (UA-54) |
| 2, 8 | Settings, Memory, Contributions and Health at 390 px, light theme | no sideways scroll. Two banners fill 40% of the first screen (UA-54) |

What the walked journeys cost, on the bench (Mon 16 Nov 2026, Lisbon):

| Job | Board | CLI |
|---|---|---|
| Ask a project | 4 steps, about 10 s: `A`, `@` and two letters, Enter, the text, Enter. The first row of the list is the project whose name starts with the letters (UA-48; before: `@la` and Enter took Atlas). The row under the text reads "orchestrator running" for a live orchestrator on either host (UA-42; before, on a paired board: "will start orchestrator" in the warning color for every project of this host, and a toast "Its orchestrator was not running, so one was started.") | 1 command. The last two lines are a sentence ("Sent to the Lantern orchestrator: '...'") and the next step, `tiller ask show <ask>` (UA-09, UA-17; before: 36 lines of record and no sentence). The record still prints above them |
| See that the ask arrived | main project: the tile shows at once. Peer project: the tile shows in about 1 s (UA-16, fixed; it was never without a reload) | `tiller ask show <ask>`: `delivery.state` reads `delivered` within 6 s (UA-17; before: no delivery state) |
| Read the answer | main project: 1 click on the ask's tile, which shows "Answered" and the first line within 2 s (UA-18), or Inbox, 2 clicks. Peer project: only on the peer's board; the Ask box, the toast and the tile now say so (UA-15) | main project: the same `tiller ask show <ask>` prints the reply's words; `tiller recap` lists every unread answer (UA-17; before: not possible). Peer project: only in a shell on the peer; main's CLI prints the `ssh -t` command for it (UA-03) |
| Ask again | 1 click on "Ask again" in the reply | type the command again |
| Recover from the wrong project | not possible: the ask tile has no Cancel (UA-18, needs owner) | `ask resolve <ask> cancelled`, which the help calls an orchestrator's tool |
| Idea to a filed card | 3 steps: New card on the project's row, the title, Enter | 1 command, after one refusal for a missing `--kind` (UA-23) |
| Idea to a filed card, from anywhere | 4 steps, about 8 s: `C`, two letters of the project, Enter, the title, Enter. In a one-project view: `C`, the title, Enter (UA-22; before: scroll to the project's row for its button) | the same command |
| Find one card among 43 | `Cmd+K`, a word of its title or its id, Enter: about 4 s, on both hosts (UA-21; before: scroll the board, "No command matches") | `card list`, then read 43 records (UA-09) |
| Filed card to a running card | no board control (UA-20, needs owner). The card says who starts it ("Waits for: The Lantern orchestrator") and prints the shell command. With no orchestrator: 1 click on "Start orchestrator" in the card | `tiller spawn <card> builder`. The last two lines are "Started the builder of lantern-5 on main (sonnet, medium)." and `tiller attach lantern-5-build` (UA-09; before: 73 lines of record, the host as a 26-character id, no next step). The record still prints above them |
| Read what happened on a card | the timeline shows the owner's news only (`harbor-2`: 4 lines in place of 24), with the question, the answer and a comment quoted (UA-04) | `card timeline` prints raw events (UA-09) |
| Answer a card that says "Decision" | 2 steps: open the card, click an option or press its digit. The question, the recommendation and the answer control are at the top of the card (UA-02; before: leave the card, open the Inbox, find the item, answer: 4 steps) | `attention answer <item>`, after `attention list --open` for the id |
| Approve a pull request and see it land | 2 clicks, about 15 s to "Merged": the Inbox item (or the card), "Approve <head>". The view shows the pull request's title, a link that opens it, its checks and its base branch above the buttons, and a receipt then goes from "Recorded" to "Merged" by itself; the card reads "Merging", then "Landed" and "Closed: done". The same for a peer's pull request (UA-36; before: the Inbox showed "Merge harbor-1 at 3cca7be00337 (squash)?", two raw ids as "Evidence" and no link, and the card said "Approve it in the pull request section of this card", which was under the timeline and the runs) | 2 commands: `tiller approve harbor-1`, which is refused and prints the head, the pull request's address and the full command, then `tiller approve harbor-1 --sha <head>`. It ends with "Approved PR #1 of harbor-1 ('Add rate limits to the public API') at 3cca7be00337. The daemon lands it now." and `tiller land harbor-1`, which ends with "Landed PR #1 of harbor-1 (...): merged as 3cca7be00337." (UA-36; before: the approval's 26-character id had to be read out of a JSON string in `tiller recap`, `tiller approve harbor-1` found nothing, and the hint for a missing `--sha` was "the board shows the head") |
| Learn that an approved pull request did not land | within about a minute of the failed checks: the Inbox item "PR #1: the checks failed at 713bdb25a76b after the approval. Nothing merged.", with a note in place of buttons (what ended, who asks again, how the note leaves), and the same on the card with a banner "Nothing merged." Before the approval, failed checks turn Approve off (UA-38; before: a receipt and a banner that said the head moved, Approve and Reject that did nothing, and a 40-character hash in the headline) | `tiller land harbor-1` ends with "harbor-1 did not land: the approval of PR #1 at 713bdb25a76b ended before the merge (checks failed). Nothing merged. The orchestrator asks for a new approval when the pull request is ready" (UA-38; before: "harbor-1 has no pending merge approval") |
| Let a project land its work with no question | 4 clicks, about 15 s: the project's row, "Grants and settings", "New grant", "Create grant". The form has safe defaults (a builder of the project, every check passes, base `main`). `juniper-1` landed 11 s after its checks passed, and the card reads "Landed". Good. The timeline now says "Tiller approved the landing under a standing grant" (before: "Tiller decided an approval") | `tiller grant create juniper --kind merge` is refused: its own default `--conditions {}` lacks `"v": 1`, and the help sends the owner to a spec for the JSON (UA-39) |
| Let a peer's project land its work with no question | not possible on main's board. The project's panel now says "Grants and the stage switch of this project are on peer's own board." (UA-43; before: an "Edit" button with no grants in it, and no word) | `ssh -t <peer> tiller grant create ...` with the JSON of UA-39 |
| Learn that a merge failed | within 10 s of the approval: a banner on the card ("Nothing merged. The merge of f3ef0e9 failed after the approval. The pull request is still open. Merging needs a fix and a new approval."), a "Needs you" item on the card and in the Inbox with the forge's reason ("PR #1 did not merge at f3ef0e99aa72: Pull Request is not mergeable (HTTP 405). Nothing merged. Have the orchestrator fix it and ask again, or stop?") and two answers, 1 click or one digit; the timeline reads "Tiller could not land the approved work: ... Nothing landed". The answer reaches the orchestrator with the question. The same for a peer's pull request, in main's Inbox within 3 s. Under a grant: one try and one item, then the grant waits for a new head; a fixed head landed in 24 to 30 s and the item left by itself (UA-44; before: the card kept "Saved an approval bound to 52fdb21", the timeline read "Tiller finished the approved action", no item opened, and a grant tried again every 15 s) | `tiller land harbor-1`: "harbor-1 did not land: the merge of PR #1 at f3ef0e99aa72 failed: Pull Request is not mergeable (HTTP 405). Nothing merged. The item about it is in tiller attention list --open: answer it, and the orchestrator fixes the pull request and asks again". `tiller recap` lists the item with its question (UA-44; before: "has no pending merge approval: the approval of PR #2 at 52fdb2182d53 is failed", with no reason) |
| Find the most urgent item in the Inbox | it is the first row and the one shown, and its row says "Urgent" (UA-05; before: the last of 7 rows) | `tiller recap` orders by kind, not by the card's priority. On main it ends with a warning that the peer's items are not counted, and the command that lists them (UA-03) |
| Learn that a host starts no new run | a banner on every screen names the host, the numbers and the step ("peer refuses new runs. 4.6 GB of disk free, under the 878,906 GB floor. ..."); the sidebar row, Health and a queued card say it too, and all clear by themselves within 30 s of a fix (UA-01; before: nothing, until a `tiller spawn` was refused) | `tiller machines` has `refuses_runs` on this host's row, and `tiller doctor` has a `floors` line. Main's CLI does not show the peer's floors (UA-03) |
| Learn that the peer is out | Health's Safety card reads "Watch" and "Only 1 of 2 machines are live" within 30 s, and "Good" again when the link is back (UA-25; before: "Watch" all day with the peer live) | `tiller machines` on main reads `stale` with `peer_link_down` within 30 s, and `ok` on the next read after the link is back (UA-25; before: `stale` from 2 minutes after every start) |
| Learn what needs you in the morning | 1 click, about 10 s: the Inbox lists every open item of both hosts, the most urgent first, and the unread answers. The sidebar count shows before the click | `tiller recap`: 4 decisions in 50 lines, then every event as a raw kind (UA-27). The peer's items are not counted, and the last line says so (UA-03) |
| Learn that the daemon restarted | a banner within 8 s: "Daemon reconnecting. Showing the last known state; answers are queued until it is back." It leaves by itself within 11 s of the start. Good | the CLI reads `state.db` and works with no daemon |
| Learn that the peer went offline | within 30 s every lane of the peer reads "as of Mon 07:46, just now. peer is not answering since Mon 07:46.", its cards dim, and its answer buttons are off with the same reason. Good | `tiller machines` reads `stale` (UA-25) |
| Learn that a run died | within 30 s the tile reads "Agent stopped" in red, and the card says whose move is next, when the owner hears of it, and the command that starts the agent again (UA-26; before: "Unknown" on the tile, "running" on the run, no step) | `tiller card show` reads `badge: unknown`, and `tiller agents` no longer lists the worker (UA-26, not done) |
| Learn that an agent waits on a permission prompt, and answer it | the Inbox row "Bash: waiting for a keypress" comes within 3 s with the Inbox open, for a peer's agent too (UA-40; before: only after a new page load). 2 clicks, about 5 s: the row, then "1 Yes", "2 Yes, and don't ask again" or "3 No" under the command the tool wants to run ("Bash wants to run `cargo publish`"). The key reaches the agent in under a second, the receipt reads "Sent 1 · Yes.", and within 4 s the detail reads "Answered. Sent 1 · Yes. The agent went on, so it left the inbox." (UA-40; before: the row stayed and the detail said "is stopped on a permission dialog. Press an answer" with no answers, for good). The same for a peer's agent. A dialog with no hook: "The board cannot read this dialog", the dialog's rows, "View live", a click on the screen and the key: 4 steps, about 10 s | `tiller card list --needs-you` has the card with `permission_prompt`; `tiller attach <card>-<role>` and the key answers it. `tiller recap` does not name it (UA-41) |
| Learn that quota ran out | the sidebar reads "2% week left" within 3 s of the daemon's poll. A start that the floor refuses opens a decision in the Inbox with "Start now anyway" and "Wait for the reset": 2 clicks, or the row and one digit. The orchestrator's note about the answer now ends with the command that starts the run (UA-46; before: "applied: start the builder run past the quota floor" and no command, so a plain start was refused and the owner got the same question a second time). The machine row still says "6 free" (UA-29) | `tiller spawn` is refused with the reason; the time is UTC and no next command is printed (UA-29) |
| Learn that a run is blocked or done | the tile shows the worker's words and "Reports blocked" or "Reports done" within 2 s, on both hosts (UA-19; before, only when the orchestrator raised an item). The card enters "Needs you" only when the orchestrator raises an item | `tiller agents`, in a CSV row |
| Close a card by hand | 4 steps: "Close", an outcome, a reason, "Close card". A line under the buttons says what is missing while "Close card" is off. On a ship card, "Done" says at once that it needs a landing and that "Won't do" ends the card without one; a refused close says the same in a sentence (UA-49; before: a dim button with no word, then "card meadow-1: landed needs a succeeded merge or land approval, or a recovery item answered "Mark landed"") | `tiller card close`; the same refusal, in the store's words |
| Hold a card until a date | 4 steps: "More", "Hold...", the reason, "Hold card" (the default is tomorrow 09:00). The card reads "Held until Tue 17 Nov, 09:00" and "Release" is its main button. The orchestrator gets "Hold: start no new run on this card before Tue 17 Nov, 09:00 (Lisbon). <reason>" within a second, and "The hold is released" after "Release"; the same for a peer's card (UA-51, UA-52; before: no word to the orchestrator, whose start went through, and on a peer's card "no card <id>") | `tiller card hold <card> --until <date> --reason <t>`. It now ends with a sentence that says the orchestrator is not told and that a start is not refused, and the `tiller card comment` line that tells it (UA-51) |
| Land work of a project with no forge | 2 clicks on the card or in the Inbox after the orchestrator's request: "Approve <head>", about 25 s to "Landed" and "Closed: done"; the daemon moves `main` in the checkout. One approval is enough: the request reads the worktree first (UA-55; before, the first approval was bound to an old head and the owner approved twice). After a second request, the landing removes the older question, and the card closes (UA-56). Before: not possible for a project with no repo name (UA-53) | `tiller approve <card>` finds only merge approvals; the approval's id from `tiller recap` is needed (UA-54) |
| Turn away mode on | 3 steps, about 15 s: "Away", the form (every project of this host is ticked, 6 runs, a 20% floor, existing grants), "Start away mode". The header then reads "Away until Tue 09:00". With a paired host, the form says "Away mode covers main only, not peer's 6 projects", what that means and where to start it for them (UA-30; before: the peer's projects were absent from the form with no word) | 1 command. The last lines are a sentence ("Away mode is on for Harbor and Lantern on main until Tue 2026-11-17 09:00 (Lisbon). At most 3 new runs start on main. Nothing merges."), the next steps, and with a paired host a warning with the `ssh -t` command that covers it too (UA-30; before: 30 lines of an approval record) |
| Return and catch up | 2 clicks: the status, "End". The brief opens by itself in the Inbox: the projects, the orders, the spend, what landed, the runs that started, what still waits, "Open waiting items". Good. It now names the projects it covers and the paired host it does not count (UA-30, UA-13) | `tiller away end` prints the brief as a record; it ends with the same warning (UA-30) |
| Watch an agent and steer it | 3 steps, about 6 s: Agents, the agent's row (or `J`/`K`), click the screen and type. The line under the screen reads "Typing goes to the agent. Board shortcuts are paused." and "Done" leaves. The same for an agent on the peer. Good | `tiller attach <card>-<role>`, type, `Ctrl-\ d`. Good. `tiller send <run> --kind steer --text ...` ends with a record in state `pending` and no sentence (UA-32) |
| First run: a project and its orchestrator | 4 steps, about 40 s: "Add project" (the id and the checkout folder), "Start orchestrator", the two folders the host lacks in the dialog that opens, "Start orchestrator" again. The dialog lists every missing folder with a field and a Save, and shows the next refusal (no terminal holder) in place (UA-33; before: a toast that named the first missing setting as a `config.toml` key, and no way to set it there) | 5 commands: `project add`, the three `tiller config set` lines that it prints, and `tiller orchestrator start`, which it prints last. A start that still lacks a folder lists all of them at once with the command for each (UA-33; before: `project add` printed only the start, which was refused three times in a row, each time for one more key and with the hint `tiller config set <key> <value>`) |
| Find one project among 14 | 1 click on its sidebar row, or `Cmd+K`, "focus" and two letters, Enter: about 4 s. The Focus view scopes the Board, the Inbox count and the Agents list, and "Show all projects" is in the header. Good | `tiller card list --project <id>`: one row per card |
| Find one agent among 23 | `Cmd+K`, the card id or "orchestrator" and the project, Enter: about 4 s. Or Agents, then `J`/`K`; the list is grouped by project. Good | `tiller agents`: a record of 15 lines per agent (UA-09) |

### The fifth pass (not complete)

Iteration 29 began a fifth pass on a new bench (Mon 16 Nov 2026, about 12:11 to 12:30, Lisbon), on main's board at 1440 px and the CLI. It walked journeys 2, 3, 4 and part of 5, and the `local-only` landing of journeys 5 and 6 again. It did not walk journeys 1, 6 (pull requests), 7, 8, 9, 10 and 11, so it does not count as a full pass.

| Journey | What was walked | Result |
|---|---|---|
| 2 | The morning check: the Inbox, `tiller recap`, Health | good. The Inbox title of a pull request still reads "Merge harbor-1 at cbe1f2000b27 (squash)?" (UA-58) |
| 3, 4 | An ask to Lantern, its answer, a new card from the board | good. A card made on the board adds the line "You commented: added from the board: <title>" to its timeline (UA-58) |
| 5, 6 | The `local-only` landing of `lantern-4`: a builder's commit, the request, "Approve" | new: UA-55 (P1): the first request was bound to the commit before the work. After a second commit and a second request: new: UA-56 (P1): the landing worked, but the card stayed in "Needs you" with the older question |
| 5 | A new run on Lantern after the landing (`lantern-5`) | new: UA-57 (P0): `spawn` was refused, "the new worktree's HEAD ... is not on origin/main". The card reads "Working, between runs", and the owner hears nothing |
| 5, 6 | After the fixes, on Quill (`quill-3`): a commit, a request, a second commit, a second request, one "Approve" | both requests bound the head of their moment. One approval landed the work in about 10 s. The older request went stale, its question left, and the card read "Landed" and "Closed: done" |
| 11 | The away brief | "Runs started 7" counts projects outside the scope "Quill, Ember" (UA-13, still open) |

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

### UA-03 - The CLI on main cannot see or reach work on the peer (P1, needs owner; the silent part is fixed)

- Journey: 3, 4, 6, 7.
- Tried: from main, `tiller board`, `tiller recap`, `tiller card show pylon-2`, `tiller ask note pylon --text ...`.
- Happened: `tiller board` lists main's cards only. `tiller recap` lists 4 decisions; the board's Inbox lists 6 (2 are on the peer). `card show pylon-2` answers `no card pylon-2` and `ask note pylon` answers `no project pylon`, with no hint that the project lives on the paired host. `tiller machines` on main shows the peer as `stale` with no slots, while the board shows it live.
- Hurts: the owner must know which machine owns a project, and open a shell on it, to use the CLI. The board hides the host; the CLI makes it the first question. `tiller recap` gives an incomplete morning list without saying so.
- Fix: owner reads and writes in the CLI go through the local daemon when it runs, as the board's do, so a peer's project, card and question resolve by name. Until then, a `not_found` on a name the peer owns says "pylon is on peer" and `tiller recap` says how many items the peer holds. Check `docs/spec` before the fix: if the CLI is host-local by contract, this moves to "Needs owner".
- What the spec says (checked on Mon 16 Nov 2026, Lisbon): the full fix is a contract change. `docs/spec/protocol.md` section 10.2 keeps a peer's rows in the daemon's memory only, so the CLI, which reads `state.db`, has no copy of them. Section 7.1 gives every daemon read to the owner session of a browser only (a local caller is no credential). ADR 0003 keeps the owner's CLI-only powers (grants, away mode, landing, `tiller spawn`) local. So the CLI cannot learn which project a peer owns, and cannot count a peer's items, without a new route. See "Needs owner".
- Part 1, done on Mon 16 Nov 2026 (Lisbon), inside the contract: the CLI no longer answers as if the peer did not exist.
  - A `not_found` of a project, card, ask, attention item, run, session or approval on a board host with a paired peer now reads "no card pylon-2 on main. This command reads main only, and peer may have it: the board shows every host". Its `hint` is the same command for the peer, ready to paste: `ssh -t <alias> tiller card show pylon-2` (the alias of config `peers.<label>.ssh`, the arguments quoted for a shell, `--db` left out). With two or more peers, or no alias, the message names the hosts and there is no `hint`.
  - Every list that covers the whole host (`recap`, `board`, `agents`, `ask list`, `card list`, `attention list`, `project list`) ends with a `warning`: "This lists main only. peer is paired and is not counted here: the board shows every host. For peer, run: ssh -t <alias> tiller recap". A list of one project (`--project`) is whole and has no warning.
  - Only a person gets these (the owner's shell or a script). An agent session's answers do not change. On the peer itself nothing changes: it is paired to no host of its own.
  - Verified on the bench with a rebuilt binary: `main card show pylon-2` and `main ask note pylon --text ...` printed the sentence and the `ssh -t` hint (before: `no card pylon-2`, `no project pylon`); the two printed commands, run through the bench's SSH stand-in, showed the card and sent the ask on the peer; `main recap` ended with the warning (before: 4 decisions, with no word that 2 more wait on the peer).
  - Tests: 6 unit tests in `crates/cli/src/cmd/elsewhere.rs` and `a_paired_host_is_named_when_this_host_lacks_the_answer` in `crates/cli/tests/commands.rs` (no peer: no change; a peer: the message, the hint, the warning on 7 lists, none on a one-project list, none for an orchestrator).
- Not done, and why: the CLI still cannot say that the peer does own `pylon` (it says "may have it"), still cannot count the peer's decisions in `recap`, and still cannot act on a peer's card without the `ssh -t` step. All three need the route under "Needs owner". The owner must still know one thing: the name is not on main. The CLI now says where to go.

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

### UA-09 - CLI answers are raw records (P1, needs owner; the summary line and the next step are fixed)

- Journey: 1, 3, 4.
- Happened: `tiller init` prints about 60 lines of fields (`stage_changed_at: null`, `reviewed_through_sha: null`) and one hint. It does not say to start the daemon, open the board or pair a host. `tiller spawn`, `tiller ask note` and `tiller card create` print the whole row, most of it `null`. The answer to "did it start, where, on which model" is in there, among 40 other lines.
- Hurts: the owner has to read a record to confirm an action. The next step is not shown.
- Confirmed in journeys 3 and 4: `ask note` answers with 36 lines, `spawn` with 73. The host is a 26-character id, never its label. The one hint after `card create` is "start each line about this card with [lantern-5]", which is an instruction for an orchestrator, not for the owner.
- Fix: a short human summary by default for owner commands ("Started harbor-1 build on main, sonnet, medium. Watch: tiller attach harbor-1-build"), with the next step. `--json` stays the full record.
- Spec check: `docs/spec/protocol.md` section 6.1 fixes the output. The default is TOON, every response has `ok` and `data`, both forms come from one struct, and a response may end with `help` lines. So the record cannot leave the default answer without a spec change. A new field beside `key` and `warning` is additive.
- Done (iteration 13): a response of a command that a person runs (the owner's shell or a script, not an agent session) has a `summary` field: one sentence with the project's name and the host's label in place of ids. TOON prints `summary` and `help` last, so they are the lines a terminal leaves in view. `help` now holds the owner's next step:
  - `tiller init`: "Tiller is set up on main, with room for 12 runs at a time." Next: `tiller daemon` (with the board's address), `tiller project add`, `tiller peer add`.
  - `tiller project add`: "Added the project Lantern (lantern). It has no orchestrator yet." Next: `tiller orchestrator start lantern`.
  - `tiller orchestrator start`: "Started the Meadow orchestrator on main." (or "already runs"). Next: `tiller ask note meadow --text ...`, `tiller attach meadow`.
  - `tiller card create`: "Filed lantern-5 'Add a monthly digest toggle' in Lantern (ship, P2). No run has started." Next: `tiller spawn lantern-5 builder` (with "without it the Lantern orchestrator decides when it runs"), `tiller card show lantern-5`. The orchestrator's hint ("start each line about this card with [lantern-5]") now goes only to an agent session.
  - `tiller spawn`: "Started the builder of lantern-5 on main (sonnet, medium)." Next: `tiller attach lantern-5-build`, `tiller card show lantern-5`.
  - `tiller ask note`: "Sent to the Lantern orchestrator: 'Is the digest behind a flag?'." With no orchestrator: "Recorded for Meadow: '...'. No orchestrator runs there, so nobody reads it yet.", and the next step is `tiller orchestrator start meadow`.
  - `tiller attention answer`: "Answered '<question>' with '<answer>'.", plus whether the agent is told and whether a card closed.
- Found on the way and fixed: `tiller attach lantern-5-build` matched no agent. Attach compared the target with the pane's label (`lantern-5 build`, with a space), so the run name of `protocol.md` 6.1 did not work and the owner had to quote the label. Attach now takes the run name too.
- Proof: on the bench, `card create`, `spawn`, `ask note` (with and without an orchestrator), `orchestrator start` (first and second time) and `attention answer` printed the sentence quoted here as the second-last line and the next step as the last line. `tiller attach lantern-5-build` in a pseudo-terminal showed the builder's screen (before: "no agent matches"). `init` and `project add` are checked by their snapshots, not on the bench. Tests: `cmd::summary` (4 unit tests), `attach::a_run_name_names_the_pane_of_its_label`, `the_owner_reads_one_sentence_and_the_next_step` (crates/cli/tests/commands.rs: the owner gets `summary`, an agent session does not), and the snapshots of `init` and the other changed commands (additive only).
- Not done:
  - The record still prints in full above the sentence (`spawn`: 73 lines, then 2). A short answer by default needs the spec change under "Needs owner".
  - `tiller approve`, `tiller land`, `tiller away`, `tiller card close` and the other card edits, `tiller peer add` and the reads (`card list`, `card timeline`, `board`) have no sentence yet. The pattern is `Outcome::with_summary` and `crates/cli/src/cmd/summary.rs`.
  - Two or more `help` lines print on one line, comma separated and quoted (the TOON form of a list of strings). It reads badly for a person (P2, added to UA-23).

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

### UA-13 - The away brief counts runs outside the away scope (P2, open; verified in journey 11)

- Journey: 11.
- Happened: away mode covered Quill and Ember. The brief says "Runs started 6" and lists Harbor, Lantern and Juniper too. "Still waiting for you 2" counts only the two projects in scope, while the Inbox holds 7.
- Hurts: two different scopes in one card. The owner cannot tell whether the brief is the whole picture.
- Fix: say the scope in the brief ("Quill and Ember"), keep both lists to it, and add one line for what happened outside it.
- Verified in the phase 3 walk (Mon 16 Nov 2026, Lisbon). The two scopes are the contract: `docs/spec/authority.md` section 6 makes the spend cap count every run of the host, and the list of projects decides only the temporary grants and the waiting items. So "Runs started" is host-wide by design. With the fix of UA-30 the brief has a "Projects" row with the names. Still open: no word says that "Runs started" counts the whole host, and that "Still waiting for you" counts the named projects only.

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

### UA-17 - The CLI cannot read an answer (P1, fixed; "Mark read" in the CLI needs owner)

- Journey: 3.
- Tried: `tiller ask note lantern --text ...`, then `tiller ask show <ask>` and `tiller ask list` after the orchestrator replied.
- Happened: `ask show` prints the ask and `cards[0]`. It has no reply, no "answered" mark and no delivery state of the note. `ask list` shows `unresolved` for the answered ask. `tiller recap` does not list replies. No command prints the reply text.
- Hurts: a job that starts in the CLI cannot end there. The owner must open the board's Inbox to read the answer to a question typed in a shell.
- Fix: `ask show` prints the note's delivery state and every reply. `ask list` gets an `answered` column. `tiller recap` lists unread replies beside the decisions, and `tiller reply read <reply>` (or `ask show --mark-read`) marks one read, so the parity row "Mark a reply read" becomes Full on both sides.
- Fixed (Mon 16 Nov 2026, Lisbon), in the CLI and the core reads:
  - `tiller ask show <ask>` prints `delivery` (the state of the note the ask went as: `pending`, `delivered`, the times, a failure reason) and `replies` (every reply, oldest first, with its words, its time, `read_at` and its card). With no reply its help line says "no answer yet; the note to the orchestrator is delivered", or that no note went to an orchestrator.
  - Every row of `tiller ask list` and `tiller ask list --unresolved` has `answered_at`, `replies` and `unread_replies`.
  - `tiller recap` has `replies`: every unread reply on this host (or of `--project`), newest first, with the project, the question and the words.
  - After `tiller ask note` the owner's help line is `tiller ask show <short>: whether it arrived, and the answer`. Before, it was the orchestrator's `ask resolve` line. An orchestrator session still gets that one.
- Verified on the bench, in the CLI on main: `ask note lantern --text "How many mails bounced last week?"`, then the printed `ask show 4WNBNYE` 6 s later read `state: delivered` and "no answer yet". After the orchestrator's `tiller reply`, the same command printed the reply's words, `answered_at` and `unread_replies: 1`. `ask list --project lantern` showed the answer's time, and `tiller recap` listed this answer and the seeded Quill answer. The job is 2 commands, and the second one is printed by the first.
- Test: `the_owner_reads_the_answer_in_the_cli` (`crates/cli/tests/reply.rs`), and the snapshots of `ask note`, `ask show` and `ask list`.
- Not done:
  - "Mark read" in the CLI. `docs/spec/authority.md` and `docs/spec/state.md` name the board as the place where the owner marks a reply read. So an answer read in the CLI stays unread in the Inbox and in `tiller recap` until the owner marks it on the board. See "Needs owner".
  - An ask sent to a peer's project: main's CLI refuses it and cannot show it (UA-03, and UA-15 for the board).
  - The answers are fields of a record, not a sentence (UA-09).

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
- `tiller machines` shows this host's `last_seen_at` as the time the daemon started. `docs/spec/protocol.md` section 9 says "now, while the daemon runs", and the daemon's own Machines row does that. Seen with UA-25.
- Two or more `help` lines print on one line, comma separated and quoted. Seen with UA-09.
- A second `tiller spawn lantern-5 builder` is refused with "builder run 01M7... is already active": a 26-character id and no next step (`tiller attach lantern-5-build`). Seen with UA-09.
- The help of `tiller attach` says a card names an agent, but no pane carries a `card` field, so `tiller attach lantern-5` matches nothing. Seen with UA-09.

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
- Health's Safety card says "Only 1 of 2 machines are live" and does not name the machine that is out. Seen with UA-25.

### UA-25 - Health says "Only 1 of 2 machines are live" while the peer is live (P1, fixed)

- Journey: 2, 7, 8.
- Tried: opened Health on the main board during the walk of UA-01, with the peer's link live.
- Happened: the Safety card reads "Watch", "Only 1 of 2 machines are live" and "machines live 1 of 2". The sidebar shows the peer with a green dot and its counts, and the peer's cards are live on the board. The count comes from `hosts.last_seen_at` in the last two minutes (`crates/core/src/state/signals.rs`, `machines_live`), and a live peer link does not keep that column fresh on the bench. `tiller machines` on main shows the same peer as `stale` for the same reason (UA-03).
- Hurts: Safety is the card that says whether to worry. It says "Watch" every day for a state that is fine, so the owner learns to ignore it, and a real outage then looks the same.
- Cause: the link wrote `hosts.last_seen_at` only when its state changed. A link that stayed live wrote it once, so two minutes after the link came up every reader of the column called the peer stale. `docs/spec/protocol.md` section 9 defines the column as "the last frame from the peer link", so the write was the defect, not the readers.
- Fixed:
  - A live link now writes `last_seen_at` from its frames, at most once per 25 s, with no event (`Store::peer_seen`, `LinkTask::seen`).
  - `tiller machines` and the Health count call a paired host stale as soon as its link is not live (`stale_reason` set), as section 9 says and as the board's Machines row does. Before, both waited two minutes after the last frame.
- Verified on the bench (Mon 16 Nov 2026, Lisbon). Before: the peer's row of `tiller machines` on main read `stale` 2 minutes after the bench started, with the link live. After: 4 minutes after the start the row reads `ok` with a `last_seen_at` under 30 s old, the events table holds one `host.changed` row, and Health reads "Good", "Both machines are live" and "machines live 2 of 2". With `link freeze` the row reads `stale` with `peer_link_down` after 26 s and Health reads "Watch" and "Only 1 of 2 machines are live"; after `link thaw` the row reads `ok` on the next read. Health checked in light and dark at 1440 px and 390 px.
- Tests: `a_live_link_keeps_last_seen_at_fresh_with_no_event` (daemon), `machines_calls_a_peer_stale_as_soon_as_its_link_is_not_live` (CLI), and one more host in `safety_counts_breaches_overlapping_runs_and_live_machines` (core).
- Not done: main's `tiller machines` still shows no slots and no agent count for the peer (UA-03). The local host's own `last_seen_at` in `tiller machines` is the time the daemon started, not "now" (P2, added to UA-23). The Safety card does not name the machine that is not live (P2, added to UA-24).
- Seen during the check, for journey 8: `link freeze` holds an outage for about 2 minutes only. The link then goes `down`, starts a new `peer serve` and is live again 5 s later, while the frozen process stays. While the peer is out, the board shows it only by the amber chips in the sidebar; no header banner says that the peer does not answer. Judge this in the walk of journey 8.

### UA-26 - A run whose agent died reads "Unknown" and "running" (P1, fixed on the board)

- Journey: 5, 8. Found in the phase 3 walk.
- Tried: I started a task and left. The agent's process died (`scripts/usability-bench.sh kill-agent main <session>`). I came back to the board.
- Happened: the tile stays in "Working" with a yellow chip "Unknown" and loses its last report line. The card page says Status "Unknown", and its run says "running" beside "Ended". The only sign is `session.ended 91W2MVT` under "Problems in the event log". The Agents screen no longer lists the worker. Nothing says that no agent works on the card, whose move is next, or what the owner can do.
- Hurts: this is the "can I believe the status" case. By `docs/spec/recovery.md` R03 the orchestrator hears first, and the owner gets an Inbox item only after 30 minutes with no action. For those 30 minutes the board shows a task that looks alive. "Unknown" is also not true: the daemon knows that the session ended.
- Cause: the badge is `unknown` by contract (`docs/spec/state.md` section 6: the run stays active, and its session has no activity). The board showed the contract word and nothing more.
- Fixed on Mon 16 Nov 2026 (Lisbon), on the board, with no contract change. The board already reads every live session (`GET /api/v1/agents`). A card with an activity badge (an active run) and no live session of its own has lost its agent (`board/src/components/board/stopped.ts`).
  - The tile: a red chip "Agent stopped" in place of "Unknown" (or "Idle", "Working", "Stale"), and a tooltip that says so.
  - The card page: Status "Agent stopped" with the red mark. A "Next step" row: "No agent works on this card. Its agent stopped before the work ended. The work so far is kept. The Juniper orchestrator starts the agent again or ends the run. If it does neither in 30 minutes, the question comes to your Inbox." and the shell command `tiller agent relaunch juniper-1-build --note "..."`. With no orchestrator the row says so and has the "Start orchestrator" button.
  - The run row: "agent stopped" in red in place of "running". This is per run, so a card with a live reviewer and a dead builder names the dead one.
  - The verdict must hold 3 s before the board says it, because the board and the agents reads refresh one after the other. It says nothing while the card's host does not answer.
  - Verified on the bench with a rebuilt binary: killed the builders of `harbor-1` and `juniper-1`; within 30 s (one reconcile pass) the tile, the card and the run row changed with no reload; no other of the 39 cards changed. The printed command, run as the owner, started a new agent, and the card went back to "Idle" with the builder's report within 6 s, with no reload. Checked in light and dark at 1440 px and 390 px.
  - A first draft printed `tiller agent resume`. The bench refused it ("has no live session to resume"), so the card prints `relaunch`.
  - Tests: `board/src/components/board/stopped.test.ts` (10 tests: the verdict for a card and for a run, a worker with no orchestrator, an unread roster, a silent host, the two notes, the command) and `views.test.tsx` ("a tile whose run has no live agent says that the agent stopped, once the verdict has held").
- Not done:
  - The CLI: `tiller card show` reads `badge: unknown` and `tiller agents` drops the worker, with no word that the run has no agent. `tiller recap` does not list the death. P2, with UA-27.
  - The board has no "Start again" button: `POST /api/v1/agents/{session}/relaunch` needs a live session. This belongs with the "Start" control of UA-20 under "Needs owner".
  - The card is not in "Needs you" for the first 30 minutes. That is R03, a contract. The 30-minute Inbox item was not walked (the walk cannot wait 30 minutes); `reconcile::recovery::worker_crash_keeps_run_worktree_and_reservation` covers it.

### UA-27 - `tiller recap` lists raw events (P2, open)

- Journey: 2. Found in the phase 3 walk.
- Tried: `tiller recap` on main for the morning check.
- Happened: the 4 open decisions read well (question, recommendation, options), in 50 lines. Then `recap[43]` lists every event since the last ask as `at, card, kind, title` with raw kinds (`card.created`, `run.started`, `approval.decided` with a `null` card), the oldest first. There is no count per project and no sentence. A run that died is not in it (UA-26).
- Hurts: "what happened overnight" is 43 lines of plumbing. The command is written for an orchestrator that presents it (the `walkthrough` field), so the owner in a shell reads the agent's input.
- Fix: a `summary` sentence for a person, as UA-09 added to seven commands ("4 decisions wait for you, 1 unread answer. Since Sun 07:40: 6 runs started, 2 ended, 2 cards closed."), and the events in the owner's words, as the card timeline has them since UA-04. The short answer in place of the record waits with UA-09 under "Needs owner".

### UA-28 - "Needs you" on the board lists every project with nothing in it (P2, open)

- Journey: 2. Found in the phase 3 walk.
- Tried: the "Needs you" tab of the board, to see the 6 cards that wait for me.
- Happened: all 16 project lanes stay. 10 of them say "Nothing in this tab. No Atlas cards match "Needs you"." The 6 cards are spread over 4 screens of scroll at 1440 px.
- Hurts: the tab that should be the shortest list is as long as the board. The Inbox does this job well, so it is polish.
- Fix: in "Needs you" (and "Active"), hide a lane with no matching card, and end the list with one line: "10 projects have nothing here."

### UA-29 - Quota that runs out: small gaps (P2, open)

- Journey: 8. Found in the phase 3 walk.
- The main case is good: at 2% of the week left, `tiller spawn` is refused, the card is held until the reset, and the Inbox gets one decision ("quota is under the floor, so the builder run for atlas-3 did not start. Start it now anyway, or wait for the reset?").
- The refusal in the CLI prints the time as `2026-11-19T07:50:07.000Z` (UTC, ISO) in place of "Thu 19 Nov 07:50" (Lisbon), and has no `hint` with the next command (`tiller attention answer <item>`).
- The sidebar's machine row still reads "13 running, 6 free" at 2%. UA-01 left quota out of the "refuses new runs" verdict; the row should say "under the quota floor" in place of "6 free".
- A new quota value shows up to 5 minutes late (the poll's cadence), on the board and in the refusal.

### UA-30 - Away mode covers this host only, and did not say so (P1, needs owner; the silent part is fixed)

- Journey: 11, 7. Found in the phase 3 walk.
- Tried: on main's board I clicked "Away" to leave for a day, with 8 projects on main and 6 on the peer.
- Happened: the form offered main's 8 projects. The peer's 6 were absent, with no word. After "Start away mode" the header read "Away until Tue 09:00" beside "All projects 16". On the peer nothing changed: `tiller away brief` there answered "no away contract yet". So the peer's orchestrators had no orders and no cap on new runs, and with "Merges: None" on main the peer's grants still merged. The return brief counted main only and did not say so. The CLI was the same: `tiller away start --projects pylon` on main answered "no project pylon" (UA-03), and a start for main's projects said nothing about the peer.
- Hurts: this is the "start and leave" promise. The owner believes that all 14 projects run inside the limits, and 6 of them run with none. The owner must think about the machine to be safe, and nothing tells them to.
- Cause: away mode is one host's contract. `docs/spec/authority.md` section 6 ("the owner through a peer board") says that the board host never has "grants, away mode, landing, spawn, ..." on a peer. A forward of away mode needs a spec change: see "Needs owner".
- Fixed on Mon 16 Nov 2026 (Lisbon), the part inside the contract: every away surface names what it leaves out, and the step.
  - The "Start away mode" and "Edit away mode" form: under the projects, "Away mode covers main only, not peer's 6 projects. They keep running with no away limit, and their grants still merge. To cover them, start away mode on peer's own board." It shows only when a paired host has projects.
  - The status popover in the header: a row "Not covered: peer's 6 projects. Start away mode on peer's own board."
  - The return brief: a "Projects" row with the names of the covered projects (it had none: UA-13), and a row "Not covered: peer's 6 projects. This brief does not count them: see peer's own board."
  - `tiller away start` on a host with a paired host ends with a warning: "Away mode is on for main only. peer is paired and is not covered: its projects keep running with no away limit, and its grants still merge. To cover peer too, run: ssh -t <alias> tiller away start --until <date> --projects <ids>". `tiller away brief` and `tiller away end` end with "This covers main only. peer is paired and keeps an away mode of its own, which is not shown here." and the same command for the peer.
  - `tiller away start` also ends with a sentence and the next steps, as the seven commands of UA-09 do: "Away mode is on for Harbor and Lantern on main until Tue 2026-11-17 09:00 (Lisbon). At most 3 new runs start on main. Nothing merges."
  - Verified on the bench with a rebuilt binary: on main's board, the popover in dark at 1440 px and in light at 390 px, the form in light at 1440 px and in dark at 390 px, and the brief in dark at 1440 px. The printed `ssh -t bench-peer tiller away start --until 2026-11-17 --projects pylon` ran through the bench's SSH shim and turned away mode on on the peer.
  - Tests: `board/src/lib/away.test.ts` (2 tests), `away.test.tsx` (the note with a paired host, no note without one, the brief's "Projects" row), `cmd::elsewhere::tests::away_mode_says_that_the_peer_is_not_covered`, `cmd::summary::tests::away_mode_names_the_projects_the_end_and_the_limits`, and `a_paired_host_is_named_when_this_host_lacks_the_answer` in `crates/cli/tests/commands.rs`. The `away_start` snapshot has the new `summary` and `help`.
- Not done:
  - One "Away" for both hosts (needs owner).
  - Main's board does not show whether the peer is away. Its header reads "Away" or "Away until" for main only.

### UA-31 - Away mode: small gaps (P2, open)

- Journey: 11. Found in the phase 3 walk.
- The spend cap stops every start on the host, in and out of the listed projects, and the popover says only "Runs 1 of 1". It should say "the cap is reached: no new run starts" there and on the header's status. The refusal in the CLI reads "approval 01M7...: away mode allows 1 new runs and 1 have started; the owner raises the cap or returns": a raw id, "1 new runs", and no command (the CLI has no `away edit`; only the board edits a running contract, `docs/design/cli-board-parity.md`).
- The brief does not count the starts that the cap refused, so the owner cannot see that the cap was too low.
- The popover does not show the orders. The brief does.
- In the form with "None" chosen under Merges, the help line still reads "Only the grants you already have may merge."
- A project's dot has another colour in the form than in the sidebar (Harbor is orange in the sidebar and grey-blue in the form): the form counts colours over this host's projects only.
- `tiller away start` while away mode is on answers "approval 01M7...: away mode is already on", with no hint to `tiller away end` or to the board's Edit.
- The date field shows the browser's own date format ("11/17/2026, 06:00 PM" in a US browser) under a "Lisbon time" label. To verify in a browser with a Portuguese locale.

### UA-32 - Watch and steer: small gaps (P2, open)

- Journey: 10. Found in the phase 3 walk. The journey itself is good on both surfaces, for an agent on main and on the peer.
- `tiller send <run> --kind steer` ends with the message record in state `pending` and no sentence or next step (the UA-09 pattern is not applied to `send`). The owner sees that it arrived only in the agent's terminal, as a line `tiller inbox <id> # steer <card>`.
- `tiller send harbor-1 --kind steer` (the card's id, which `tiller attach` takes) answers "no run harbor-1 on main ... peer may have it" with an `ssh -t` hint for the peer. The card is on main; `send` takes only a run or a session. It should take the names that `attach` takes, and the hint for the peer is wrong here.
- `tiller attach` without a terminal answers "an agent never attaches", which reads as an accusation to an owner who ran it from a script.
- On the board, the typing field of a worker is named "Type to ├ pylon-1 build" for a screen reader: the tree mark is part of the name.
- "Open in terminal" is off for an agent on the peer, with no reason in view. To verify: whether its tooltip gives one.

### UA-33 - First run: the start that `project add` names is refused, one missing folder at a time (P1, fixed)

- Journey: 1.
- Tried: on an empty home, `tiller init`, `tiller project add notes --name Notes --delivery local-only`, then the next step that it printed, `tiller orchestrator start notes`. On the board of a second empty home: "Add project", then "Start orchestrator" on the new lane.
- Happened:
  - CLI: the start was refused with "config.toml: checkouts.notes is not set; tiller orchestrator start needs it" and the hint `tiller config set <key> <value>`. After that key was set, the same start was refused for `spawn.pool_root`, then for `spawn.checkouts_root`. Three refusals in a row, each for a key the one before did not name, and no word on what the value is.
  - Board: a toast, "Could not start the orchestrator. config.toml: spawn.pool_root is not set; tiller orchestrator start needs it". It named the first missing setting only, as a raw key, and offered no way to set it. The fix was two screens away, under Settings > Starting agents. The Ask box already had a form for this case; the lane's button did not use it.
- Hurts: this is the first thing a new owner does, and the printed next step fails. The owner must guess three values from three key names, or read the spec.
- Fixed (Mon 16 Nov 2026, Lisbon), with the reads and the refusal that `docs/spec/protocol.md` section 7.4 already defines (`current.settings`):
  - CLI: a `tiller orchestrator start` that is refused over settings lists every one in `current.settings`, as the board gets them, and its hint is one command per setting, then the start again. The host's two folders come first, since the checkout goes inside one of them. With a saved checkouts folder, the checkout's command has the full path (`tiller config set checkouts.maps /home/user/src/maps`).
  - CLI: `tiller project add` ends with the same steps. Its sentence says "Set its folders first: the steps are below." when a folder is missing, and the last step is the start.
  - Board: "Start orchestrator" on a lane or a card opens a dialog, "The Notes orchestrator cannot start yet", with every missing folder as a field with a Save (a checkout is filled in with its default), and "Start orchestrator" again. A refusal for another reason (no terminal holder) shows in the dialog. For a paired host, the dialog shows the command to run there, as the Ask box does.
  - Verified on two empty homes beside the bench, with a rebuilt binary: the CLI printed the three commands after `project add`, and one refusal listed the three settings; on the board, two folders were saved in the dialog, the start then named the missing holder in place, and with a holder running the same button started the orchestrator and the lane showed it. Checked at 1440 px in light and at 390 px in dark.
  - Tests: `orchestrator_start_needs_the_project_checkout_configured` (crates/cli/tests/commands.rs) asserts the list, the steps and their order, and the steps of `project add`; one new test in `board/src/components/board/views.test.tsx` walks the dialog to a start.

### UA-34 - First run: small gaps (P2, open)

- Journey: 1.
- `tiller init` needs `--host-label` and `--lane-capacity`, with no default and no suggested number. A daemon that starts on an empty home sets the host up by itself, with the machine's name and 2 runs.
- Before `init`, every command says "host : state.db is not initialized; run tiller owner set tiller, then tiller init" (an empty name before the colon). `tiller init` works without the first command.
- The next steps of `tiller init` name `tiller daemon` and not `tiller holder`. A start without a holder is refused with the right hint, but the board shows no banner on a home where no terminal holder runs.
- `tiller project add` needs `--delivery` with no default (the board defaults to `direct-pr`), and has no flag for the checkout folder, which the board's dialog asks for.
- The board's "Add project" dialog shows "give a folder; spawn.checkouts_root is not set, so there is no default" in red as soon as the id is typed, before the owner reached the folder field, and names a raw key.
- `tiller peer add` ends with a record and no sentence or next step (as UA-09).
- A hint of more than one command prints as one quoted string with `\n` in it. The short answer of UA-09 ("Needs owner") would print one command per line.
- Fix: defaults for `--lane-capacity` and `--delivery`; a `--checkout` flag (a spec question, `docs/spec/protocol.md` section 6.4); a board banner for a missing holder; plain words in the dialog.

### UA-35 - Scale: small gaps (P2, open)

- Journey: 9.
- The board has no key that moves between cards: Tab is the only way, through 39 tiles. The Agents screen has `J`, `K` and `N`. The palette (`Cmd+K`) is the fast path and works.
- The board has no sort and no text filter. The filters are All, Active, Needs you, one machine, and the Focus view.
- In the Focus view the sidebar still says "Agents 23", while the Board and Inbox counts follow the focus and the Agents list itself shows 3.
- A project's name in the palette lists its cards first; "Focus Harbor" is the eighth row.
- The banner "peer refuses new runs" arrives about a second after the first paint and moves the board down by its height.
- A tile with a one-line title keeps an empty second line.
- CLI: `tiller card list` filters by project, column and "needs you", but has no search by a word of the title; `tiller agents` prints 15 lines per agent (UA-09).
- Fix: `J`/`K` and Enter on the board as on Agents; the project's own rows first when the query is a project's name; the Agents count scoped as the others; `card list --search`.

### UA-36 - An approval asks the owner to merge a commit hash (P1, fixed)

- Journey: 5, 6, 7.
- Tried: a builder finished, the daemon opened its pull request, and the orchestrator asked for my approval. I wanted to see what I approve, approve it, and know that it landed.
- Happened:
  - The Inbox item read "Merge harbor-1 at 3cca7be00337 (squash)?". Under it, "Evidence" listed two raw lines, `approval_id: 01M7...` and `head_sha: 3cca7be...`. No title of the work in the question, no pull request link, no checks. The only way to judge the work was to find the pull request on the forge by myself.
  - The card's "Needs you" box said "This is an approval. Approve it in the pull request section of this card." That section is under the timeline and the runs, more than a screen down, with no link to it. A decision is answered in the box itself since UA-02; an approval was not.
  - CLI: `tiller recap` listed the item with the approval's id inside a JSON string and no command. `tiller attention answer <item>` was refused with "(tiller approve)". `tiller approve harbor-1` answered "no approval harbor-1 on main ... peer may have it". `tiller approve <id>` with no `--sha` said "the board shows the head each approval binds". After the right command, 30 lines of record and no sentence; `tiller land` the same.
- Hurts: approving is the owner's highest-stakes click, and it is daily. The owner either approves a hash blind or leaves Tiller to find the pull request. The card's sentence sends the owner to hunt on the page.
- Fixed on Mon 16 Nov 2026 (Lisbon):
  - Board: a pull request block right above the Approve button, in the Inbox and in the card's "Needs you" box: the pull request's title, "Open example/harbor#1" (opens the pull request), its checks, and the branch it merges into. With checks that still run it says "An approval now merges once the checks pass."; with failed checks, "An approval now merges nothing: the daemon lands only a head whose checks pass." It reads the card (`GET /api/v1/cards/{id}`, a peer's with `?host=`), so no contract changes. The two raw id lines are gone from "Evidence".
  - Board: the card's box has Reject and Approve in it, as a decision has its options. The pull request section keeps the facts and says "Approve or reject it at the top of this card": one pair of buttons on the page.
  - CLI: `tiller approve` takes a card (`tiller approve harbor-1`), as `tiller land` does. An accept with no `--sha`, a `tiller land` of an undecided approval and a `tiller attention answer` of a merge item all print the same steps: "It binds 3cca7be00337 of PR #1: <address>", then `tiller approve harbor-1 --sha 3cca7be00337`, then `tiller approve harbor-1 --reject`. `--sha` stays required: the owner still names the head they read.
  - CLI: `approve` and `land` end with a sentence (UA-09's `summary`): "Approved PR #1 of harbor-1 ('...') at 3cca7be00337. The daemon lands it now." (or "Nothing lands yet: the daemon is not running (tiller daemon)."), "Rejected the merge of ...", "Landed PR #1 of harbor-1 ('...'): merged as 3cca7be00337."
  - Verified on the bench with a rebuilt binary: approved the peer's `pylon-1` from the box on its card on main's board (the card read "Merging", then "Landed" and "Closed: done", and the peer's bare repository moved), and approved and landed `harbor-1` with the three CLI commands. Checked the Inbox and the card in light and dark at 1440 px and 390 px, with passing and with running checks. Not clicked after the fix: Approve in the Inbox (its button and receipt are unchanged code; before the fix that click went from "Recorded" to "Merged" within 20 s), and the text for failed checks (no screenshot).
  - Tests: `board/src/components/attention/inbox.test.tsx` ("an approval shows the pull request it lands, with a link and its checks"), `board/src/components/board/views.test.tsx` ("a PR waiting on the owner is decided at the top of the card, with the pull request it lands", which replaces the test of the buttons in the section), `crates/cli/tests/land.rs` (`approve_takes_a_card_and_says_what_to_review`, and the sentence in `land_waits_for_the_daemon_and_reports_the_result`); the snapshots `approve` and `approve_without_sha` are updated.
- What already worked, and needs no fix: after the click the receipt and the card tell the truth without a reload, on both hosts; a second `approve` is refused with the state it finds; the daemon lands by itself, so `tiller land` only waits.

### UA-37 - Decide: small gaps (P2, open)

- Journey: 5, 6.
- The question itself is "Merge harbor-1 at 3cca7be00337 (squash)?": a hash and a merge method in the headline, in the Inbox row and in the notification, and not the title of the work. It is the daemon's text (`approval.request`). Better: "Merge 'Add rate limits to the public API' (PR #1)?".
- The pull request's description, which the builder wrote for the owner, shows nowhere on the board: the card read has the title and no body.
- A card whose pull request is open with passing checks and no approval request yet reads "Idle", and its main button is "Close". Nothing says that its orchestrator asks for the approval next, and the board cannot ask for it.
- The card's timeline says "Tiller linked a pull request" and "Tiller updated a pull request" (once per poll that sees a change) with no number and no link.
- The card's "Problems in the event log" lists `attention.opened <id>` for an approval request: it is not a problem, and it is a raw event kind.
- After an approval in the card's box, the box stays "Answered" with "Saved an approval bound to <head>" once the card has landed. It should end with "Merged".
- On the peer, `tiller card show` said `state: open` for the pull request for about 30 s after the card had landed (the next poll set it to `merged`).
- CLI: `tiller recap` still prints the merge item's evidence as a JSON string, and names no command for it. `approve` and `land` print the full record above their sentence (UA-09, needs owner).
- The Inbox detail's "Card" line broke the card's id over two lines at 390 px ("harbor-" and "1"). Fixed in iteration 19 (`white-space: nowrap`).
- Walked in iteration 20: a grant, a rejection, a push after the request and failed checks (UA-38, UA-39). Not walked yet: `land_direct`.

### UA-38 - Failed checks after an approval leave an item that cannot be decided, with a false reason (P1, fixed; how the note leaves needs owner)

- Journey: 5, 6.
- Tried: I approved a pull request. Its checks then failed (also tried: an approval while the checks already failed). I wanted to know whether it merged, why not, and what I do next.
- Happened:
  - The receipt under my approval said "The head moved before the merge started, so the approval no longer applies." The head did not move: the checks failed. Its last step stayed "waiting" for good.
  - The card's banner said "Approval is stale. It was bound to ef9ac21 and the head is now ef9ac21. Merging needs a fresh approval.": the same hash twice. The pull request section said "stale, bound to ef9ac21 > head is ef9ac21" and "+0 commits since approval".
  - The daemon opened a new item, "PR #1 checks failed at ef9ac2194a116d861dead918985d9d8c4f885d03; the merge approval is stale" (cut off in the Inbox row), with Reject and "Approve ef9ac21" under it. Both can only fail: the approval behind the item is already `stale`. A click showed nothing.
  - The item cannot leave: an answer is refused (a merge item is answered by its approval), the approval is refused (stale), and the board's Withdraw is for items that no longer apply. The card stays in "Needs you" as "Merge approval". When the orchestrator asked again, the dead item stayed beside the new request; after I rejected the new request, the card still said "Needs you" and "Approval is stale".
  - With failing checks before any approval, the board said "An approval now merges nothing" and still offered Approve, which made exactly this dead item.
  - CLI: `tiller approve harbor-1` and `tiller land harbor-1` answered "harbor-1 has no pending merge approval". An approval while the checks ran said "The daemon lands it now."
- Hurts: "did my approval land?" is the trust question of journey 5, and failed checks after an approval are a normal day. The owner read a false reason, clicked buttons that did nothing, and kept a "Needs you" card with nothing to do.
- Fixed on Mon 16 Nov 2026 (Lisbon):
  - Board: a merge item asks for a decision only while its approval is `requested` (the item carries the approval's state). Any other merge item is a note: in place of the buttons, the Inbox and the card's box show "Nothing merged", what ended, that the orchestrator asks for a new approval when the pull request is ready, and how the note leaves ("when the card closes", and on this host the command `tiller attention withdraw <item> --reason seen`). An approval that still stands (the daemon waits on a pull request body it will not merge) reads "Your approval stands".
  - Board: while a pull request's checks fail, Approve is off, in the Inbox and on the card, with "Approve is off: the daemon lands only a head whose checks pass." Reject stays on. So the owner no longer makes an approval that can only end unused.
  - Board: the receipt's failed step reads "Nothing merged. The approval ended before the merge started: the head moved, or the checks failed." The card's banner and its pull request row tell an approval that ended at the same head ("Nothing merged. The approval of e4f7a64 ended before the merge: the checks failed. ...", "ended before the merge, at e4f7a64") from one whose head moved (the old text). Of two ended approvals of one pull request, the newer one decides what the card says, so a rejection after a stale approval reads as a rejection.
  - Daemon: the item's question is "PR #1: the checks failed at 713bdb25a76b after the approval. Nothing merged." (a 12-character hash). The timeline says "Tiller ended an approval before the landing: checks failed" (before: "recorded a approval stale: checks failed"), "You approved the landing" or "You rejected the landing", and "Tiller approved the landing under a standing grant" (before: "decided an approval" for all three).
  - CLI: `tiller approve <card>` and `tiller land <card>` with nothing pending say what became of the newest approval: "... the approval of PR #1 at 713bdb25a76b ended before the merge (checks failed). Nothing merged. The orchestrator asks for a new approval when the pull request is ready", or "was rejected", or "has landed". A `land` that waits while the approval goes stale ends with the same sentence. `approve` says "The daemon lands it once its checks pass." while they run, and "but its checks fail. Nothing lands: the daemon ends this approval unused." when they fail.
  - Verified on the bench with a rebuilt binary: approved `harbor-1` while its checks ran, failed them, and within about a minute `tiller land harbor-1` ended with the new sentence, the Inbox showed the new headline and the note with no buttons, and the card showed the banner, the note, the timeline line and the pull request row. The peer's `pylon-1` with failed checks showed Approve off on main's board. Checked the Inbox item and the card in light and dark at 1440 px and 390 px. Not seen on the bench after the fix: the receipt's new text (the approval of that pass was made in the CLI), and the "Your approval stands" note (no pull request body that the daemon refuses is seeded).
  - Tests: `board/src/components/attention/inbox.test.tsx` ("a merge item whose approval ended shows what happened and no buttons"), `board/src/components/board/card-model.test.ts` (the newer of two ended approvals decides; an approval that ended at the same head), `crates/cli/tests/land.rs` (`an_approval_that_ended_unused_says_why_and_that_nothing_merged`), and three timeline sentences in `crates/daemon/src/views.rs`.
- Still open, under "Needs owner": the note leaves the board only when the card closes. The board cannot withdraw an item that still applies, and a new request does not replace it.
- What already worked, and needs no fix: a rejection (one click, the receipt reads "Saved: not approved", "Nothing to merge", "The orchestrator hears it on its next turn"); a push after the request (within 10 s the old item left and "Merge pylon-1 at fc4903398e85 (squash)?" came, and its approval merged in 13 s); failed checks show on the item within a minute.

### UA-39 - Decide, part 2: small gaps (P2, open)

- Journey: 6, 7.
- `tiller grant create <project> --kind merge` is always refused with its default: `--conditions` defaults to `{}`, and the answer is "grant conditions must be version 1 (\"v\": 1)". The help says "as a JSON object (`authority.md`, section 6)": the owner must read a spec to write one. Better: the refusal prints a full example (the board form's defaults: `{"v":1,"actions":["merge"],"base_ref":"main","checks":"success","opened_by_fleet_run":true}`), or flags such as `--checks` and `--base`. A default that grants is the owner's call. `--expires tomorrow` is refused; only dates work.
- A rejection takes no reason, on the board and in the CLI. The orchestrator hears "rejected" and must guess why; the owner has to add a comment on the card as a second step.
- While the owner reads an approval in the Inbox and a push moves its head, the detail turns into "Not in the inbox any more. It was answered or withdrawn." It should say that the pull request got a new commit and link to the new request, which is in the list.
- A note item (UA-38) still shows "Still waiting" with a green dot over it, and its row and chip say "merge approval". Better: "Note" and no waiting line.
- Work that a grant landed is in no morning list by name: the card reads "Landed" and the timeline names the grant (since UA-38), but `tiller recap` lists `approval.decided` (UA-27) and the Inbox says nothing. The grant's row counts it ("used 1 time").
- To verify: once, right after a change of the browser's size, a peer's approval item stayed at "Reading PR #1..." for more than 4 s; a new load showed the pull request at once. Seen once.
- Not walked yet: `land_direct`. A grant on the peer was walked in iteration 22 (UA-43, UA-44).

### UA-40 - With the Inbox open, a permission prompt never came, and an answered one never left (P1, fixed)

- Journey: 5, 7, 8.
- Tried: I left the Inbox open, as I do all day. An agent then stopped at Claude Code's permission dialog ("Bash wants to run ..."). I waited for it in the list. In a second pass I opened the page again, answered the prompt with the board's "1 Yes" button, and watched what the item said.
- Happened:
  - The prompt did not come to the open Inbox: not in 50 s, not for an agent on main, not for one on the peer. The sidebar count stayed too. `tiller card list --needs-you` named the card at once, and a new page load showed the row.
  - After the answer the key reached the agent in under a second and the receipt "Sent 1 · Yes." showed for 2 s. Then the detail went back to "Allow Bash? harbor-3-build is stopped on a permission dialog. Press an answer, and the board types it in the pane." with no answers under it, and the row stayed in the list with its count. It stayed so for minutes, until a new page load; the agent had gone on long before.
  - A dialog with no hook (`--no-hook`) came and left by itself: those rows are read from the agents list, which the board reads again on every session event.
- Why: a permission prompt is not an attention row. The daemon derives it from the session's newest observation and lists it in the attention read (`docs/spec/state.md`, Attention). The board read that list once: no session or observation event marked it stale, and the `peer_reset` that brings a peer's rows replaced the peer's attention items but not its permission prompts.
- Hurts: "the system tells me clearly when it needs me" fails for the most common stop of an agent, unless the owner loads the page again. And after the answer the board says the agent is still stopped and offers no answer: the owner cannot tell whether the answer went, and checks in a terminal.
- Fixed on Mon 16 Nov 2026 (Lisbon), in the board only:
  - A `session`, `run`, `pane`, `observation` or `worktree` event now marks the attention read stale too, beside the agents list (`applyChange` in `board/src/lib/sse.ts`). The Inbox, the sidebar count and the Agents screen share that read.
  - A `peer_reset` now replaces that peer's permission prompts as it replaces its attention items (`applyPeerReset`).
  - Verified on the bench with the Inbox open and no page load: a prompt on main's `harbor-3` came to the list in 2 s and the count went from 11 to 12; "1 Yes" reached the agent, and 4 s later the detail read "Answered. Sent 1 · Yes. The agent went on, so it left the inbox." and the count was 11. The same for the peer's `pylon-2` on main's board: 3 s to come, 3 s to leave. Before the fix the same script showed no row after 13 s and the old detail 40 s after the answer.
  - Tests: `board/src/lib/sse.test.ts` ("a session or observation event marks the permission prompts stale", "peer_reset brings a peer's permission prompt and takes it away, and keeps this host's").
- Also fixed in this walk:
  - A dialog the board cannot read showed "What the pane shows" without the dialog: the pane read is cut at the first rule, and Claude Code draws a dialog under that rule. The owner read the lines above it ("Bash(npm publish)", "Waiting…") and no question. The read now keeps those rows there (`tidy(text, whole)`), so the detail shows the title, the command, the question and the options above "View live". The test's pane read is now drawn as Claude Code draws it, under a rule.
  - While a peer's screen is read (about 5 s) the detail was a headline over nothing. It now says "Reading the dialog from the agent's screen on peer...". Not seen on the bench: this line was added after the last screenshots, and only the board's gates ran on it.
- What already worked, and needs no fix: the answer buttons name the command and the three options, on a phone too; a tap carries the dialog it was drawn from; a peer's prompt has the same view and the same two clicks; the card's tile and page read "Permission prompt" and the run row reads "Waiting on a permission prompt" with "Terminal" beside it; "View live" opens the terminal, a click on the screen and the key answers, and the view closes by itself.

### UA-41 - An agent that waits on a prompt: small gaps (P2, open)

- Journey: 2, 8.
- `tiller recap` does not name an agent that waits on a prompt: its `decisions` list holds attention items only. The owner's morning check in the CLI misses the stop; `tiller card list --needs-you` (badge `permission_prompt`) and `tiller agents` (`waiting_permission`) show it. Better: a `prompts` list in `recap`, with `tiller attach <card>-<role>` as the next step.
- The Inbox row of a prompt reads "Bash: waiting for a keypress" and "keypress in a pane". It does not say what the tool wants to run; the detail does ("Bash wants to run `cargo publish`"). The daemon stores no tool input by rule (`docs/spec/protocol.md` 4.1), so the row would need the screen read that the detail makes.
- The card page of a waiting card has no answer buttons and no link to its Inbox item: the status reads "Permission prompt" and the way on is "Terminal" in the run row, under the timeline. Better: the "Needs you" box of UA-02 with the Inbox's answer strip.
- The age beside "waiting since" is the newest observation's, not the start of the wait: an item that waited 5 minutes read "just now" after each new read of the screen. To verify against a real agent.
- At 390 px the item's top line ends in a lone "/" before "waiting since" wraps to its own line.
- The bench's `fake-agent` sends no hook after an answer. With a real agent the `PostToolUse` hook follows; the board's refresh after an answer is verified with the holder's own read of the screen only.

### UA-42 - On a paired board the Ask box said "will start orchestrator" for every live project of this host (P1, fixed)

- Journey: 3, 7.
- Tried: `A`, `@atl`, Enter, a question for Atlas, whose orchestrator was live and idle on main.
- Happened: the row under the text read "Atlas, on this host, will start orchestrator" in the warning color. The send first posted a start for an orchestrator that ran, and the toast would read "Its orchestrator was not running, so one was started." A peer's project read "orchestrator running" correctly. On a host with no peer the row was right, so the tests did not see it.
- Why it hurts: the most common job of the day says a false thing about the state of the system, each time, and only after a peer is paired. The owner either stops to check the orchestrator, or learns to ignore the warning that is true when an orchestrator is really gone.
- Cause: with a paired host, this host's rows carry its label too (`host: "main"`), and only a peer's row has a `link`. The dialog compared the agent's label with the project's owner (`undefined` for this host), so no orchestrator of this host ever matched.
- Fixed (iteration 22): the dialog compares owners (`ownerOf`), as the board's lanes do. The card page had the same comparison in two lookups of a card's project (the fallback hid it; a bug card's "Route" could show the slug in place of the name); both compare owners now. The dialog's tests use rows of the paired shape; three of them fail on the old code.
- Verified on the bench: Atlas and Pylon in one ask read "orchestrator running" for both; the toast read "Sent 2 asks" with the note about Pylon's answer and no word of a start.

### UA-43 - A peer's project has no grants control on main's board, and nothing said where grants are (P1, needs owner; the silent part is fixed)

- Journey: 6, 7.
- Tried: let Pylon (on the peer) land its work with no question, as done for Juniper in 4 clicks: the project's sidebar row, "Grants and settings".
- Happened: a peer's project has "Edit" in that place, and the dialog has no grants. The panel showed no grants line and no word about them. "Switch to pre-launch" was off, with its reason in a tooltip only, which a phone never shows.
- Why it hurts: the owner must not have to think about which machine does the work. Here a job of journey 6 ends in a missing button, with no next step, for 6 of 14 projects.
- Fixed (iteration 22, the silent part): the panel of a peer's project says "Grants and the stage switch of this project are on peer's own board." Two tests cover it (the note for a peer's project, none for this host's).
- Verified on the bench at 1440 px (dark) and 390 px (light).
- Not fixed: grants for a peer's project from one board, and a read of a peer's grants. `docs/spec/protocol.md` section 7.4 and ADR 0003 keep grants and the stage on their own host. See "Needs owner".

### UA-44 - A merge that fails says nothing, and a grant tries it again without end (P1, fixed)

- Journey: 5, 6, 7.
- Tried: a merge grant for Pylon on the peer, then a second pull request (`pylon-2`) with passing checks. On the bench that pull request cannot merge: the stand-in forge only fast-forwards, and `pylon-1` had landed. A real pull request with a conflict gives the same answer from the forge ("Pull Request is not mergeable", HTTP 405).
- Happened, under the grant: every 15 s the daemon asked for an approval, accepted it under the grant, ran the merge and recorded `failed`. 7 times in 100 s, 10 uses of the grant in 5 minutes, and it went on until the grant was revoked. The card stayed in "Landing" with no word. The timeline grew by four lines per try: "Tiller asked for an approval", "Tiller approved the landing under a standing grant", "Tiller started the approved action", "Tiller finished the approved action". No line says that the merge failed. No item opened. `tiller recap` listed 30 raw `approval.*` events.
- Happened, with the owner's own approval of the same head: the card read "Saved an approval bound to 52fdb21", then the timeline read "Tiller finished the approved action". The pull request stayed open and nothing said so. `tiller land pylon-2` answered "pylon-2 has no pending merge approval: the approval of PR #2 at 52fdb2182d53 is failed", with no reason and no next step.
- Why it hurts: this is "can you believe done" turned around. The owner approved, or gave a grant, and left. The work did not land, and the only sign is a card that never moves. "Finished the approved action" reads as a success.
- The reason is recorded: the approval row has `result.error` ("gh: Pull Request is not mergeable (HTTP 405)"), and the `approval.finished` event has `state: failed`.
- Fixed (iteration 23), in four parts:
  1. The daemon tells the owner once. A landing that the forge or git refuses ends `failed`, and the land sequence then opens one `recovery` item on the card (`recovery.md` R08 asks for it "after a failed or unreadable execution"; until now only a failure that reconcile resolved got one). The question has the pull request, the short head, the forge's own reason and "Nothing merged", and two answers: "Fix it and ask again" and "Stop". The answer goes to the orchestrator with the question, as every answer does.
  2. A grant does not accept the same head again. The grant matcher skips a pull request whose newest try at this head `failed`, as it already did for `stale`. A new head matches again. `authority.md` section 6 says a grant matches "when a PR becomes eligible", so the retry was no contract.
  3. When a later landing of the card succeeds, the daemon withdraws the open question about the failed one ("the work landed after it, as <sha>"). Without this the card stayed open and in "Needs you" after its pull request merged: the store does not close a landed card that has an open item. Seen on the bench with the first build of this fix (`juniper-1`), then fixed.
  4. The words. The timeline line of `approval.finished` depends on its state: "landed the approved work", "could not land the approved work: <reason>. Nothing landed", or "sent the approved action and could not read its result". The card has a banner "Nothing merged. The merge of <head> failed after the approval. The pull request is still open. Merging needs a fix and a new approval.", and its pull request section reads "merge failed at <head>. Nothing merged." `tiller land` and `tiller approve` say "<card> did not land: the merge of PR #n at <head> failed: <reason>. Nothing merged." and where the item is. The Inbox no longer prints the approval's raw record (`result: {"error": ...}`, `state: failed`) as "Evidence".
- Tests: `a_refused_merge_tells_the_owner_once_and_a_grant_waits_for_a_new_head` (the land sequence: one item, one merge call in four passes, a new head lands, the item leaves, the card closes), `a_failed_merge_says_why_and_that_nothing_merged` (the CLI), the timeline sentences, `failedAtHead` on the board, and the evidence lines.
- Verified on the bench, with the page left open:
  - Harbor, the owner's own approval on the card: banner, item and timeline line within 10 s. "Fix it and ask again" reached the orchestrator's pane as the question and the answer; the card left "Needs you".
  - Juniper, a merge grant: one try and one item in 75 s (before: a new try every 15 s). A rebased head landed under the same grant within 30 s.
  - Pylon on the peer, approved in main's Inbox: the item came to main's Inbox in 3 s. A rebased head landed under the peer's grant in 24 s, the item was withdrawn, the card closed as landed, and the row left main's Inbox with no load.
  - Screenshots of the card and the Inbox item at 1440 px and 390 px in both themes: no defect seen.
- Left as P2 (in UA-45): the two answers are words for the orchestrator, and the daemon acts on neither.

### UA-45 - The open-page pass: small gaps (P2, open)

- Journey: 5, 6, 8.
- A card with an open question whose agent died shows the question only. `tundra-1` kept "Credential" on its tile and "Your answer" at the top of the card after its builder was killed. The run row says "agent stopped", far down. An answer goes to nobody until the orchestrator starts a new run. Better: one line under the question, "The agent that asked has stopped. The orchestrator gets your answer."
- A landed card's tile still shows the worker's last report ("Pull request ready: ...") beside "Landed".
- A started run reads "Idle" on its tile 2 s after the start (the bench's `fake-agent` does no work; to verify against a real agent).
- With an Inbox item open, every link of the sidebar carries `?item=<id>` to the other screens, and the Board's address keeps it.
- The Inbox row of a merge approval still reads "Merge harbor-1 at 73de1bda546f (squash)?"; the detail has the pull request's title (UA-36).
- `tiller attention answer <item> --option 1` is refused ("unexpected argument"): the CLI takes the answer as `--text` only, while the board answers an option with one key.
- A new pull request with no check runs reads "checks green" on the stand-in forge; to verify what a real pull request with no checks shows.
- After a failed merge (UA-44) the answered approval on the card still ends with "Saved an approval bound to f3ef0e9", under the banner that says nothing merged. In the Inbox the approval's receipt reads "Delivered: Delivery failed. The daemon opened a recovery item for it." and "Done: waiting"; "Delivery" is the wrong word for a merge.
- After a failed merge the card's badge is the live builder's "Idle", not "Sent back", while the run is still active (`state.md` section 6 ranks the run's activity first).
- "Stop" on a failed-merge item only tells the orchestrator. The owner who wants the card gone still uses "Close" or "More".
- A landed card with any other open item stays open, in "Landing" and "Needs you", after its pull request merged (the store skips the close). The failed-merge item is now withdrawn at the landing; an unrelated open question on the card still keeps it open. To verify what the board says for that card.

### UA-46 - "Start now anyway" did not tell the orchestrator how to start (P1, fixed; the second question needs owner)

- Journey: 8 (quota that runs out), 6. Found in the open-page pass, iteration 24, by following the owner's answer to the orchestrator that receives it.
- What the owner tried: at 2% of the week left a start of `meadow-1` was refused, and the Inbox asked "Start it now anyway, or wait for the reset?". The owner picked "Start now anyway", whose own line says "The run starts now, below the quota floor, and the hold ends."
- What happened: the answer released the hold and sent the orchestrator a note that ended "applied: start the builder run past the quota floor". The run starts only when the orchestrator runs `tiller spawn <card> <role> --past-quota-floor`. Neither the note, the refusal nor `roles/orchestrator.md` named that flag. An orchestrator that ran the start the way its role text says (`tiller spawn meadow-1 builder`) was refused with "the card is held until ... and the owner has a decision to make", the card was held again, and a second item with the same question opened in the Inbox, below the one the owner had just answered.
- Why it hurts: the owner said "start", and the system asks the same question again and starts nothing. The owner cannot tell whether the first answer counted. A run that the owner released by hand may never start.
- Fixed (the note, the refusal, the role text):
  - The note to the orchestrator ends with "next: run `tiller spawn <card> <role> --past-quota-floor` now (with `--promote` when it promotes a scout). Without that flag the spawn is refused and the owner is asked again" (`OwnerAction::next_step`).
  - A plain start that is refused while the owner's unused "Start now anyway" stands says so and prints the same command.
  - `roles/orchestrator.md` says that a quota refusal asks the owner, and that the start after "Start now anyway" needs `--past-quota-floor`.
- Not changed: a plain start with a standing answer still opens a second question. `docs/spec/protocol.md` section 8 says that a refused start opens a decision, and a test (`a_newer_wait_answer_shadows_an_older_start_answer`) rests on it. See "Needs owner".
- Verified on the bench with the Inbox left open: `cobalt-1` refused at 2%, the item came in under 3 s, "1" answered it, and the orchestrator's `tiller inbox` showed the note with the command. A plain start as the orchestrator printed "The owner's earlier "Start now anyway" still stands: `tiller spawn cobalt-1 builder --past-quota-floor` starts the run". The start with the flag ran, and the receipt's "Done" line read "The orchestrator read your answer".
- Tests: the note's `next:` line and the refusal's command are asserted in `crates/agents/src/spawn/tests.rs`.

### UA-47 - The end of the open-page pass: small gaps (P2, open)

- Journey: 4, 8, 10.
- A line typed to a peer's agent while the peer does not answer is dropped with no word. The screen's header says "Unknown, host not answering", but the line under the screen still reads "Typing goes to the agent. Board shortcuts are paused.", and after the link is back the line is not on the agent's screen. Better: no typing mode while the host does not answer, and "peer is not answering. Typing is off until it is back." under the screen.
- For about 25 s after the peer stops, the board still shows it as live, so a line typed in that time is lost the same way.
- A card with a live worker that reported "working" shows "Status: Idle" above "Builder says: Working" (the bench's `fake-agent` is idle; to verify against a real agent, with UA-45).
- The worker's report ("working: Reading the digest template") left the agent's row in the Agents list after the owner typed to the agent; the card kept it.
- The palette's project picker matches letters in any place: "La" lists Lantern and then Atlas.
- After "Start now anyway" the receipt's "Done" line reads "The orchestrator read your answer". It does not say whether the run started; the card's tile does.
- The quota refusal in the CLI still prints the reset as UTC (UA-29).
- `tiller card create` takes the project as a bare word, while `card list` takes `--project`; the refusal for `--project` is "unexpected argument" with no hint (UA-23).

### UA-48 - In the Ask box, `@la` and Enter sent the ask to Atlas, not Lantern (P1, fixed)

- Journey: 3. Found in the third pass, iteration 25.
- What the owner tried: `A`, `@la`, Enter, a question for Lantern, Enter. These are the steps this file's own cost table names.
- What happened: the list under `@la` read "Atlas, Lantern" with Atlas marked, so Enter took Atlas. The list kept the projects in their listed order and took every name that holds the letters in order. The toast said "Sent to the Atlas orchestrator", and the Atlas orchestrator got the question about Lantern's digest.
- Why it hurts: the fastest way to ask sends the question to the wrong project whenever an earlier name holds the typed letters. The chip shows the name, but the owner who types and presses Enter does not read it. A wrong ask cannot be cancelled on the board (UA-18, needs owner), and the wrong orchestrator answers it. The palette already ranked a name that starts with the letters first; the Ask box did not.
- Fixed: the Ask box ranks its projects with the palette's own score (`scoreMatch`): a name that starts with the typed letters, or holds them without a gap, comes first (`rankMentions` in `board/src/components/shell/ask-mentions.ts`). No letters typed keeps the listed order.
- Verified on the bench: `@la` lists "Lantern, Atlas" with Lantern marked, the ask went to Lantern, its tile showed in the Lantern lane at once, and the reply showed on the tile 1 s after the orchestrator sent it.
- Tests: `ask-mentions.test.ts` (the order for "la" and "ba", the listed order for no query, no match).

### UA-49 - "Close", then "Done", on a ship card always ended in a raw refusal (P1, fixed)

- Journey: 5, 6. Found in the third pass, iteration 25.
- What the owner tried: the builder of `meadow-1` reported "done", and the tile read "Reports done". The owner opened the card and clicked its green "Close" button to finish it.
- What happened: the dialog showed three outcomes with none chosen, an empty reason and a dim "Close card" button, with no word about what was missing. With "Done" and a reason, the close was refused: "card meadow-1: landed needs a succeeded merge or land approval, or a recovery item answered "Mark landed"". The store closes a ship card as done only after an approved merge, and a card whose work lands closes by itself. So "Done" on an open ship card fails every time (but for a landed card that another open item keeps open, UA-45).
- Why it hurts: the card's main button leads to an error in the store's words, with no next step. The owner cannot tell whether the work is lost, whether the card can be closed at all, or what "a recovery item" is. `tiller land meadow-1` in the CLI says only "meadow-1 has no pending merge approval".
- Fixed (the board's close dialog):
  - With "Done" chosen on a ship card, a line under the outcomes says "Done needs a landing: the card closes as done by itself when its approved work lands. With no landing, the close is refused. To end the card without one, pick Won't do." A scout card says the same about its report.
  - The two refusals of the store read as a sentence with the next step: "Not closed: the work of this card has not landed. The card closes as done by itself when an approved merge lands. To end it now, pick Won't do and say where the work is."
  - While "Close card" is off, a line above the buttons says why: "Pick an outcome and give a reason to close the card.", then "Give a reason to close the card."
- Not changed: the rule itself (a spec contract, `docs/spec/state.md`), and "Done" stays in the list, because the store takes it for a landed card that is still open. The CLI's refusals are under UA-50.
- Verified on the bench at 1440 px in the dark theme and at 390 px in the light theme: the three lines show as written, and the refused close leaves the card open with the sentence in the dialog.
- Tests: two in `card-lifecycle.test.tsx` (the lines of the dialog, and the two refusals in plain words).

### UA-50 - The third pass: small gaps (P2, open; the away count is fixed)

- Journey: 3, 4, 5, 6, 8, 10, 11.
- Fixed: the away status in the header kept "Runs 0 of 6" after a run started under the contract, until a page load or the next approval; `tiller away brief` said 1. The board now reads the away brief again on every run event of this host (`board/src/lib/sse.ts`, with a test). Verified with the page left open: "1 of 6", then "2 of 6" 2 s after a second start.
- "Ask again" on a reply opens the Ask box with the old question in it. A follow-up question is typed after the old words unless the owner deletes them first. Better: the project's chip only, with the old question quoted above the field.
- The timeline of a card filed on the board starts with "You commented: added from the board: <title>". The owner wrote no comment.
- A ship card of a `local-only` project whose worker reported done: `tiller land <card>` answers "has no pending merge approval", with no word that the orchestrator must ask for the landing first, and no command.
- `tiller land <card>` after the landing answers `ok: false`, "refused ... has no pending merge approval: PR #1 at <head> has landed". It is the answer the owner wants, in the shape of an error.
- A killed agent read "Unknown" on its tile for 35 to 60 s before "Agent stopped" (the table above says 21 s for an earlier pass).
- After "Start now anyway" and a start with the flag, the second quota item of a plain start stays open in the Inbox and in the away brief's "Still waiting for you" (UA-46, needs owner).
- The CLI's quota refusal under away mode says "2 percent left is at or under the 20 percent floor" and does not say that the floor is away mode's.
- `tiller init` with no flag prints "the following required arguments were not provided:" and the list is empty; the usage line above it names them (UA-34).
- The Agents list shows "9 min ago" on a row whose header reads "observed 2 min ago": the row's time is not labelled.
- On a card with a running agent, the green "Close" is the main button, beside "More". "Close" ends nothing by itself there (UA-49), so a quieter button would fit better.

### UA-51 - "Hold" promised no new run, and the orchestrator started one (P1, needs owner; the silent part is fixed)

- Journey: 5. Found in the fourth pass, iteration 26.
- What the owner tried: filed a card, then thought "not yet": "More", "Hold...", a reason, "Hold card". The dialog said "A held card starts no new run until the time below."
- What happened: the card read "Held until Tue 17 Nov, 09:00". The Atlas orchestrator, which had the note "new card atlas-1" from the filing, ran `tiller spawn atlas-1 builder` two seconds later and the run started. `run.start` has no guard for a hold (`docs/spec/state.md` section 6), and `card.hold` sends no note, so the orchestrator had no way to know but a `tiller card show` that nothing told it to run. The board then showed the card in "Working" with a live agent and the hold banner still on it.
- Why it hurts: the owner's "wait" was accepted, shown and not kept. It is the one control that says "do not start this", and it worked only when the orchestrator happened to look.
- Fixed, inside the contract:
  - The board sends a comment to the card's orchestrator after a hold ("Hold: start no new run on this card before Tue 17 Nov, 09:00 (Lisbon). <reason>") and after a release ("The hold is released: this card may start again."). The comment is the one call that reaches the orchestrator. It carries the key of the click, so a retry sends it once. When it fails, a toast says that the card is held and that its orchestrator was not told. With no orchestrator there is nobody to tell and no toast.
  - The dialog says what is true: "The orchestrator is told to start no new run on this card until the time below. A run that is live goes on."
  - `tiller card hold` by a person ends with "<card> is held until <date> (Lisbon). Its orchestrator is not told, and `tiller spawn` does not refuse a held card." and the `tiller card comment` line that tells it.
  - `roles/orchestrator.md`: a held card gets no run before its date, `tiller spawn` does not refuse one, so read `parked_until` in `tiller card show` before each start; only the owner releases the owner's hold.
- Verified on the bench: the note reached the Atlas orchestrator within a second of "Hold card", the release note after "Release", and both for the peer's `willow-1` from main's board.
- Tests: two in `card-lifecycle.test.tsx` (the comment after a hold, with the click's key, and after a release); the `card_hold` snapshot of the CLI.
- Not done: the refusal itself. See "Needs owner".

### UA-52 - On a peer's card page every header action went to the wrong host (P1, fixed)

- Journey: 7. Found in the fourth pass, iteration 26.
- What the owner tried: opened the peer's held card `willow-1` on main's board and clicked "Release".
- What happened: "no card 01M7...". The peer answers a card read from its own database, so the row in the answer names no host. The page adds the host for most of its parts, but the header and the "Depends on" row took the row as it came. So Release, Hold, Close, Cancel card, Edit and the priority were sent with no `host` and this host answered that it has no such card; "Depends on" listed this host's cards. The same card's answer buttons, comment and approval worked, which is why earlier passes did not see it.
- Why it hurts: six owner actions on a card fail for 6 of 14 projects, with an error that reads as if the card were gone. The owner has to know that the card is on the peer and open the peer's board.
- Fixed: the card page builds the detail with the owned row once and gives that to every part (`board/src/components/board/card-page.tsx`).
- Verified on the bench: "Release" on `willow-1` from main's board released the hold on the peer, and the peer's orchestrator read the release note.
- Tests: one in `federation.test.tsx` (a peer's read with no host: the dependency and the hold are sent with `host`). It fails without the fix.

### UA-53 - A project with no forge and no repo name could never land work (P0, fixed)

- Journey: 5, 6. Found in the fourth pass, iteration 26.
- What the owner tried: a builder of Atlas, a `local-only` project, committed its work and reported done. The orchestrator asked for the landing: `tiller approval request atlas-1 --kind land_direct`.
- What happened: "project atlas: project has no repo to bind". `approval.request` refused every project with no `repo`, and a `local-only` project has none by default (`tiller project add` says "leave out for a local-only project"; 11 of the bench's 14 projects). A ship card closes as done only with a landing (UA-49), so such a card could end only as "Won't do", and the work stayed on its branch. The landing step itself handled `local-only` (`docs/spec/authority.md` section 6: "the same binding to branch and head SHA, merged in the project's checkout by the daemon, with no forge"); its test gave the project a repo name.
- Why it hurts: the whole "approve and land" job did not exist for a project with no forge. The owner saw no question at all, only a card that never finished.
- Fixed: `approval.request` takes a `land_direct` request of a `local-only` project that names no repo; the project's id fills the snapshot column, which no forge call reads for such a project (`crates/core/src/state/approvals.rs`). Every other request with no repo is still refused, and the refusal now names `tiller project edit <project> --repo <owner/name>`.
- Verified on the bench with Quill (no repo): the request opened the question, "Approve" on the card at 390 px landed the commit on `main` in the checkout, and the card read "Landed" and "Closed: done".
- Tests: `land::tests::local_only_project_with_no_repo_requests_and_lands` in `crates/agents`.

### UA-54 - The fourth pass: small gaps (P2, open)

- Journey: 2, 5, 6, 7, 8.
- To verify: twice the board's reads stayed pending for 20 to 40 s (once in the Inbox during "Reject" on a peer's pull request, once on Settings, which showed only "This browser"), while `curl` to the same daemon answered in 1 ms. Chrome had its 6 connections to the daemon open. Both times were within minutes of a rebuild of the binary and a daemon restart, so it may be the bench. If reads forwarded to a slow peer can hold all 6 connections, every screen waits with them.
- The first approval of a `local-only` landing was bound to an old head: this is UA-55, fixed in iteration 29.
- The question of a direct landing reads "Push atlas-1 at 4674b7b4b1f1 (push)?", under the label "Merge approval". It shows no branch, no commit title and no list of files (UA-36 gave a pull request these).
- `tiller approve <card>` and `tiller land <card>` find only merge approvals: for a direct landing they answer "has no pending merge approval".
- "Cancel card" says "its runs stop". The run's record ends at once, but its agent stays live and listed under Agents until the orchestrator stops it (`docs/spec/recovery.md` R15; the owner is asked after 30 minutes). The card page of a cancelled or reopened card still shows "Builder says: Working".
- An edit, a cancel and a reopen made on the board send the orchestrator no note.
- A scout card's question says "The parser report is ready", and the card has no link to the report.
- The timeline reads "You released the hold: removed" and, after a landing, "Tiller released the hold: removed".
- The hold dialog shows the date as `11/17/2026` and the time as `09:00 AM` (the browser's locale), under the label "Until (Lisbon time)".
- A hold now adds a line "You commented: Hold: start no new run ..." to the timeline, under "You held the card" (UA-51).
- At 390 px two banners ("peer refuses new runs" and "14 sessions run an older tiller") fill 40% of the first screen on every page.
- "Reject" on an approval asks no reason, so the orchestrator reads only "answer: reject" (UA-37).

### UA-55 - The first request of a direct landing was bound to an old head (P1, fixed)

- Journey: 5, 6. Found in the fourth pass (as a "to verify" item of UA-54) and reproduced in the fifth pass, iteration 29.
- What the owner tried: the builder of `lantern-4` committed its work. The orchestrator asked for the landing at once: `tiller approval request lantern-4 --kind land_direct`. The owner clicked "Approve efef03a".
- What happened: the request was bound to `efef03a`, the commit before the work. `approval.request` binds the cached head of the worktree, and the cache moves only on the next reconcile pass. The landing then read the real branch head, found a different commit, and went stale. The owner had to approve a second time.
- Why it hurts: the owner approves a commit that is not the work, and the first approval is wasted.
- Fixed: before `tiller approval request --kind land_direct` binds the head, it reads the worktree of a run on this host and writes the reading to the cache, as `spawn` and `tiller done` already do (`crates/cli/src/cmd/approval.rs`). The binding is still the cached head, as `docs/spec/protocol.md` asks.
- Verified on the bench with Quill (`quill-3`): two requests, each made within a second of a commit, bound `6ca0d3d` and `adb4c57`, the heads of that moment.
- Tests: `a_direct_landing_binds_the_branch_head_and_not_a_stale_cache` in `crates/cli/tests/land.rs`. It fails without the fix.

### UA-56 - After a second request, a landed card stayed in "Needs you" and never closed (P1, fixed)

- Journey: 5, 6. Found in the fifth pass, iteration 29.
- What the owner tried: the worker of `lantern-4` committed again after the first request, so the orchestrator asked again at the new head. The owner approved the new question.
- What happened: the work landed, and the run ended `landed`. But the first request stayed `requested` with its question. The card kept "Needs you: Push lantern-4 at efef03ad6261 (push)?" and "Approve efef03a", and it did not close: `card.close` needs no open item. Before the landing, the card showed two "Needs you" items, and both buttons acted on the newest approval.
- Why it hurts: work that landed reads as work that waits on the owner. The card never reaches "Done", and the only exit is `tiller attention withdraw` in a terminal.
- Fixed: when a landing succeeds, the daemon marks every other `requested` or `accepted` merge or direct-landing approval of the card `stale` ("the work landed as <sha> through a later request"). That withdraws their questions in the same step, so the card closes `landed` (`crates/agents/src/land/mod.rs`). This is `approval.stale`, the daemon's transition for a binding that no longer holds.
- Verified on the bench with Quill (`quill-3`): after one "Approve" of the newer request, the older one read `stale`, the card read "Landed" and "Closed: done", and the timeline said "Tiller withdrew an item: approval went stale: the work landed as adb4c573fe6d through a later request".
- Not fixed: before the landing, the card still shows two questions, one per request. A new request that withdraws the older one needs a change of `approval.request` in `docs/spec/state.md` (see UA-38 under "Needs owner").
- Tests: `land::tests::a_landing_stales_the_older_requests_on_its_card` in `crates/agents`. It fails without the fix.

### UA-57 - On a `local-only` project, no run can start after Tiller lands work (P0, needs owner)

- Journey: 5. Found in the fifth pass, iteration 29.
- What the owner tried: `lantern-4` landed in Lantern's checkout, so `main` there moved to `26d6f4c`. The orchestrator then started the builder of `lantern-5`: `tiller spawn lantern-5 builder`.
- What happened: "git: the new worktree's HEAD 26d6f4c... is not on origin/main; it is not a fresh worktree". The pool cuts the worktree from the checkout's `main`. The spawn then compares it with `origin/main` and refuses anything ahead (P05). A `local-only` landing moves only the checkout's `main` and never pushes, so `origin/main` stays at the old commit. Every later run of the project is refused the same way. The board shows the card as "Working, between runs", and the owner hears nothing.
- Why it hurts: a `local-only` project can land one card. After that the project does no more work until the owner pushes `main` to `origin` by hand. 11 of the bench's 14 projects are `local-only`.
- Why it is not fixed here: `docs/spec/state.md` section 3 (Run) says that `base_sha` is "the remote default branch's head after a fetch made by the spawn". A fix must change that contract. See "Needs owner".

### UA-58 - The fifth pass: small gaps (P2, open)

- A card made on the board adds the timeline line "You commented: added from the board: <title>".
- The Inbox title of a pull request reads "Merge harbor-1 at cbe1f2000b27 (squash)?": a raw head and the merge method, not the pull request's title.
- `tiller card timeline` still prints raw events (UA-09).

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

Two more failed in the one full run and were not run again alone: `tiller-cli::holder_agents a_lost_holder_resumes_its_worker_and_notifies_the_owner_once` and `tiller-cli::soak a_short_soak_has_no_gap`. In iteration 21 the first of them failed twice more in a run of `holder_agents` alone (`-j 2`, after 122 s each time), while the other nine tests of that file passed; that iteration changed `fake-agent` only behind a new environment variable, which this test does not set. Eighteen `tiller-cli::doctor` and `commands` tests failed in that run too and pass with `-j 2`: other builds had the machine at a load average of 22, and those tests have deadlines.

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
6. Done: UA-25 (a live peer counts as live on Health and in `tiller machines`) and UA-17 (the CLI prints the delivery state and the answer of an ask). Done as far as the contract allows: UA-09 part 1 (a sentence and the next step after seven owner commands). Done as far as the contract allows: UA-03 part 1 (main's CLI names the peer and prints the command for it). Later: UA-10 and UA-23, the CLI as an owner surface.
7. Phase 3, in progress. Walked: journeys 2 and 8 (UA-26 found and fixed on the board; UA-27 to UA-29 are P2), journeys 10 and 11 (UA-30 found, fixed as far as the contract allows; UA-31 and UA-32 are P2), and journeys 1 and 9 (UA-33 found and fixed; UA-34 and UA-35 are P2). The stand-in forge is built, and the main path of journey 6 is walked on both hosts (UA-36 found and fixed; UA-37 is P2). The rest of journey 6 is walked too (a rejection, a moved head, failed checks, a grant): UA-38 found and fixed, UA-39 is P2. An agent that waits on a prompt is walked (journey 8; the bench's `fake-agent` paints the dialog and `prompt HOST SESSION` opens it): UA-40 found and fixed, UA-41 is P2. The open-page pass has begun (journeys 3, 5, 6, 7 and parts of 1, 8 and 11): UA-42 found and fixed, UA-43 found and fixed as far as the contract allows, UA-44 found and fixed (iteration 23), UA-45 is P2. Iteration 24 finished the open-page pass (journeys 2, 4, 9, 10, the peer offline and quota out): UA-46 found and fixed as far as the contract allows, UA-47 is P2. Iteration 25 walked all 11 journeys a third time: UA-48 and UA-49 found and fixed on the board, UA-50 is P2 (its away count is fixed). Iteration 26 began the fourth pass with the paths not taken before: UA-53 (P0) and UA-52 found and fixed, UA-51 found and fixed as far as the contract allows, UA-54 is P2. Next: one full pass of all 11 journeys.
8. UA-07 with UA-08: the host out of the owner's way.
9. UA-11, UA-13, UA-14, UA-24, UA-27, UA-28, UA-29, UA-31, UA-32, UA-34, UA-35, UA-37, UA-39, UA-41, UA-45, UA-47, UA-50, UA-54, UA-58: polish.

## Needs owner

- UA-57 (P0): let a `local-only` project start its runs from the checkout's own default branch. Today `docs/spec/state.md` section 3 says that `base_sha` is the head of `origin/<default>` after a fetch, and the spawn refuses a worktree ahead of it (`crates/agents/src/spawn/pool.rs`, `start_from_origin`). A `local-only` landing moves only the checkout's branch (`docs/spec/authority.md` section 6: "merged in the project's checkout by the daemon, with no forge"), so after one landing every spawn of the project is refused. Recommendation: for a `local-only` project, the spawn fetches `origin` as now, and takes as `base_sha` the newer of the checkout's `refs/heads/<default>` and `origin/<default>`, when one descends from the other. It refuses only when the two have diverged, with a refusal that names both heads. The sentence in section 3 becomes "the remote default branch's head after a fetch, or for a `local-only` project the checkout's default branch when it is ahead". Do not push to `origin` from a `local-only` landing: the delivery mode promises no push. The work after a yes is small: one branch in `start_from_origin`, a spawn test, and one sentence of spec.

- The test failures in "Gates on the bench machine": six tests fail on this machine without any change of this audit. Recommendation: one iteration finds the cause of the four holder failures (they may be real macOS bugs of the terminal holder); the two 10 ms budgets are judged against a quiet machine.

- UA-15 part 2 (the last P0): `docs/spec/protocol.md` section 10 says that ask text stays on the peer, and replies are not federated. With that rule the owner can never read, on one board, the answer to a question that the same board sent. Today the board says so in the Ask box, the toast and the tile (part 1, done), but the owner must still open the peer's board to read the answer. Recommendation: for a paired host with an `act` key, let the link carry the first line of each ask, an `answered_at` on each `asked` row, and the unread replies, and forward "Mark read". The owner's words already cross the link when the ask is sent. The work after a yes is about one iteration: the snapshot rows, a federated replies read, the forwarded "Mark read", and the tile and Inbox on the board.

- UA-18, the rest: let the owner cancel an ask on the board, and open one. Today a wrong ask leaves the board only when the orchestrator resolves it, or when the owner runs `tiller ask resolve <ask> cancelled` in a terminal. `docs/spec/authority.md` (the matrix row "Resolve an ask, create or edit a card") already gives the owner this right, but `docs/spec/state.md` names the board as an actor of `ask.resolve` only "implicit on add", and `docs/spec/protocol.md` section 7 lists no call. Recommendation: add `POST /api/v1/asks/{id}/cancel` (owner only, forwarded to a peer like the other mutations, `ask.resolve cancelled` with the reason "cancelled by the owner on the board") and `GET /api/v1/asks/{id}` (the full text, the note's delivery state, the replies), and add `answered_at` to the `asked` row (shared with UA-15 part 2). The board work after a yes is about one iteration: a panel that the tile opens, with the text, "Delivered", the answer, "Ask again" and "Cancel".

- UA-19, the rest: let a worker's `blocked` or `needs-decision` report flag its card. Today the card enters "Needs you" only when the orchestrator opens an item, so a worker that stops while its orchestrator is busy, slow or gone shows "Reports blocked" on its tile but is in no count and no Inbox. `docs/spec/state.md` section 6 fixes the flag. Recommendation: do not change the flag. Have the daemon open a `recovery` item when a `blocked` or `needs-decision` report is older than a few minutes and no open item exists for the card (a reconcile rule, so `docs/spec/recovery.md` gets a row). The owner then gets one Inbox item with the worker's words, and the existing flag, counts and notification apply unchanged. Also add `last_status_at` to the agents rows (`docs/spec/protocol.md` section 7.3), so the tile can say how old the report is. The work after a yes is about one iteration.

- UA-20, the "Start" control: let the owner start a run on the board. Today the board files a card and the orchestrator decides when it runs; the owner's own start is `tiller spawn <card> <role>` in a terminal. `docs/spec/authority.md` already gives the owner the right. `docs/spec/protocol.md` section 7 has no call for it. Recommendation: add `POST /api/v1/cards/{id}/runs` (owner only, forwarded to a peer like the other mutations, the same `run.start` as `tiller spawn` with the role from the card's kind and the model and effort from the dispatch table, and the same refusals for capacity, the floors and quota). The board then gets "File and start" in the new-card dialog and "Start" on a queued card, and shows a refusal with its reason. The work after a yes is about one iteration.

- UA-38, the rest: let a note about an ended approval leave the list. Today the daemon's "checks failed after the approval" item stays open until its card closes: an answer and an approval are refused by design, `POST /api/v1/attention/{id}/withdraw` answers 422 while the item "still applies" (`docs/spec/protocol.md` section 7.4), and `approval.request` writes only the new approval and its item (`docs/spec/state.md` section 6). So the card stays in "Needs you" with nothing to decide, also after a new request or a rejection. The owner's only exit is `tiller attention withdraw <item> --reason <t>` in a terminal, which the board now prints. Recommendation: (1) `approval.request` and a rejection withdraw the open notes of earlier approvals of the same pull request ("replaced by a new request"), and (2) the staleness rule of an attention item (`still_current`) counts a merge item whose approval is `stale`, `rejected`, `succeeded` or `failed` as no longer current, so the board's Withdraw works on it. Both change a spec table. The work after a yes is under one iteration.

- UA-46, the second question: what a plain `tiller spawn` does while the owner's unused "Start now anyway" stands. Today it is refused, holds the card again and opens a second decision with the same words (`docs/spec/protocol.md` section 8: a refused start opens a decision; `--past-quota-floor` is the only way to use the answer). The note and the refusal now name the flag, so an orchestrator that reads them starts the run at the first try. One that does not still sends the owner the same question twice, and the second item stays open after the run started. Recommendation: let a plain `tiller spawn` use the owner's unused answer for that card and role, exactly as `--past-quota-floor` does, and keep the flag as a no-op for old prompts. The owner's answer is the authority; the flag adds a step that only an agent can get wrong. If the flag must stay: refuse the plain start with the command and open no new hold and no new item while the answer is unused, and withdraw the card's open quota items when a run starts on that answer. Both change section 8 and the `run.start` row of `docs/spec/state.md`, and one test. The work after a yes is well under one iteration.

- UA-51, the refusal: let the store refuse a start on a held card. Today `run.start` has no guard for an open hold (`docs/spec/state.md` section 6), and `card.hold` writes no note, so a hold is kept only by an orchestrator that knows of it. The board now sends the note as a comment (two calls from the browser, not one transaction), and the CLI prints the line that sends it. Recommendation: (1) `run.start` refuses a card with an open hold whose date is in the future, with "<card> is held until <date>: <reason>. Release it with tiller card unhold <card>", and (2) `card.hold` and `hold.release` by anyone but the project's own orchestrator enqueue one note to it in the same transaction, as `card.create` does. Check the quota hold first: a refused start sets a hold until the reset, and "Start now anyway" releases it, so the guard fits; a plain start after quota came back by itself would need the release too. Both change rows of the mutation table and one sentence of the Hold section. The work after a yes is under one iteration, and the board's own comment then goes.

- UA-53, the spec text: the fix lets a `local-only` project with no repo ask for a landing, with the project's id in `approvals.repo`. `docs/spec/authority.md` section 6 asks for this landing; the DDL keeps `repo IS NOT NULL` for `land_direct`, and `docs/spec/state.md` calls `repo` "a snapshot of what was approved". Recommendation: add one sentence to the Approval section ("for a `local-only` project that names no repo, `repo` holds the project's id"), or make the column nullable for that case in a migration. No code changes with the first.

- UA-01, the spec text: the fix added the field `refuses_runs` to the Machines row and a `floors` line to `tiller doctor`. `docs/spec/recovery.md` R16 asks for the first ("The Machines row shows the breached floor"), but the field table of `docs/spec/protocol.md` section 9 and the list of checks in its section 6.4 do not name them, and this audit may not edit a spec. Recommendation: add one row to the table (`refuses_runs`: one line per breached RAM or disk floor, absent while the host can start a run; a peer's from its own row while the link is live) and "the floors" to the list of doctor checks. No code changes with a yes.

- UA-17, "Mark read" in the CLI: `docs/spec/authority.md` (the row "mark a reply read") says the owner marks read "on the board", and `docs/spec/state.md` lists the board as the actor of `reply.read`. The store itself asks only for the owner. Recommendation: let `tiller ask show <ask> --mark-read` run the same `reply.read` for the owner, and change the two spec rows to "the owner, on the board or in the CLI". The work after a yes is small: one flag, one test, and the parity row "Mark a reply read" becomes Full.

- UA-09, the short answer: `docs/spec/protocol.md` section 6.1 says that the default output is TOON with the full `data`, for agents. The owner's commands now end with a `summary` sentence and `help`, but 40 to 70 lines of record print first. Recommendation: when the caller is a person (no agent session) and stdout is a terminal, print only `summary`, a `warning` if any, and `help`; `--json` and a pipe keep the full record, and so does a new `--full`. Add `summary` to the list of response fields in 6.1 ("a sentence for a person; absent for an agent session"). The work after a yes is small: one branch in `emit` (crates/cli/src/main.rs) and a sentence for the commands that have none yet.

- UA-03, the rest: let the owner's CLI on the board host reach a peer's work through the local daemon. Today the CLI reads and writes this host's `state.db` only. The daemon holds the peer's rows in memory (`docs/spec/protocol.md` 10.2), gives its reads to a browser's owner session only (7.1), and ADR 0003 keeps `tiller spawn`, grants, away mode and landing local. So on main, `tiller recap` cannot count the peer's decisions and `tiller card show pylon-2` cannot find a peer's card; since part 1 they say so and print `ssh -t <alias> tiller ...`. Recommendation: a second listener of the daemon on a Unix socket inside `~/.tiller/run` (mode 0600; every fleet role is already denied `~/.tiller`, G20), which serves the same federated reads and forwarded mutations as the board to the owner's CLI, with the CLI's own owner check done first. The CLI then uses it when a name is not on this host, and for `recap`, `board`, `agents`, `attention list` and `ask list`, and falls back to this host with today's warning when the daemon is down. Keep `spawn`, grants, away mode and landing local, as ADR 0003 says. The work after a yes is about two iterations: the socket and its credential rule in `authority.md`, then the CLI reads, then the forwarded writes.

- UA-30, the rest: one "Away" for every paired host. `docs/spec/authority.md` section 6 says that the board host never has away mode on a peer, so the owner who leaves must start away mode twice, once per board, and read two briefs. Today every away surface says so (part 1, done). Recommendation: let the board host forward `away.start`, `away.edit` and `away.end` to a host paired with the `act` scope, as it forwards a project edit (D22), with one form that lists every host's projects and applies the limits per host; and carry each host's away state and brief over the link, so that one header status and one brief cover all. Temporary grants made by "Conditions" stay each host's own records. The work after a yes is about two iterations.

- UA-43, the rest: grants for a peer's project from one board. `docs/spec/protocol.md` section 7.4 forwards an edit, an archive and a delete of a peer's project (D22), and ADR 0003 keeps grants, the stage switch, away mode and landing on their own host. So the owner sets a grant for 6 of 14 projects on a second board, and main's board cannot show whether a peer's project lands by itself. Today the project's panel says so (part 1, done). Recommendation: forward `grant.create` and `grant.revoke` to a host paired with the `act` scope, as a project edit is forwarded, and carry each project's active grants over the link, so "Grants and settings" is the same dialog for every project. The stage switch can follow the same way. The work after a yes is about one iteration.

One more candidate, to settle when its fix starts:

- UA-07: whether the host chip on every card is wanted. Recommendation: show it only when a host needs attention.
