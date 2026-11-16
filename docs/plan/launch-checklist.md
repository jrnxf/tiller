# Launch checklist

What is still open before Tiller has a proper launch. Tick a box in the same commit that does the work. Details of each `UA-` item are in `usability-audit.md`, under "Findings" and "Needs owner".

Started Monday 2026-11-16, Lisbon time.

## Owner decisions (spec changes)

On Monday 2026-11-16 at about 15:30 Lisbon time, the owner said yes to every item below. Build each one as its recommendation in `usability-audit.md` ("Needs owner") says. For UA-15 part 2 the owner added the model: a person visits one board, and that board listens to its peers and shows their work in its own view. So every reply, read state and action on a peer's ask goes through the board the owner is on.

- [x] UA-57 (P0): a `local-only` project starts its runs from the checkout's default branch when it is ahead of `origin` (`docs/spec/state.md` section 3, `base_sha`)
- [x] UA-15 part 2 (P0): the answer to an ask sent to a peer project shows on the board that sent it
- [x] UA-18: cancel and open an ask on the board (`POST /api/v1/asks/{id}/cancel`, `GET /api/v1/asks/{id}`)
- [x] UA-19: a worker's `blocked` or `needs-decision` report puts its card in "Needs you" (a `recovery` item)
- [x] UA-20: "Start" a run on the board (`POST /api/v1/cards/{id}/runs`)
- [x] UA-38: a note about an ended approval leaves the list
- [x] UA-46: a plain `tiller spawn` uses the owner's unused "Start now anyway"
- [x] UA-51: the store refuses a start on a held card
- [x] UA-53: one spec sentence for `approvals.repo` of a `local-only` project with no repo
- [x] UA-01: the spec names `refuses_runs` and the `floors` doctor check
- [x] UA-17: "Mark read" in the CLI (`tiller ask show <ask> --mark-read`)
- [x] UA-09: a short answer for a person at a terminal, the full record with `--json` or `--full`
- [x] UA-03: the owner's CLI reaches a peer's work through the local daemon
- [x] UA-30: one "Away" for every paired host
- [x] UA-43: grants for a peer's project from one board
- [x] UA-07: show the host chip on a card only when the host needs attention

## Work after the decisions

- [ ] Build each decision that gets a yes, with a test that fails without it
- [ ] Walk a seventh full pass on a new bench: all 11 journeys, on the board with one page left open and on the CLI, with the work on main and on the peer
- [ ] Walk UA-57 again on the bench: land a card in a `local-only` project, then start the next run
- [ ] Stop the passes when one full pass finds no new P0 or P1

## Untested

The owner skipped the full test passes on Monday 2026-11-16 to get a release out. These shipped with unit and integration tests only, and no walk on the bench:

- [ ] UA-57: the `local-only` start from the newer of the checkout and `origin` (f89bd22)
- [ ] UA-20: "Start" and "File and start" on the board (`POST /api/v1/cards/{id}/runs`): unit and peer tests only, no bench walk
- [ ] UA-07: the host chip shows only when the host needs attention: board tests only, no bench walk
- [ ] UA-53: the Approval sentence in `docs/spec/state.md`: text only
- [ ] UA-01: `refuses_runs` and the floors check in the spec: text only
- [ ] UA-63 and UA-64 from iteration 33: no seventh pass confirmed that the sixth pass found every P1
- [ ] UA-09: the short answer at a real terminal, on the bench; only a pseudo-terminal test ran. A read with no sentence still prints its record
- [ ] UA-43: a grant made and revoked, and a stage switched, on a peer's project from main's board, through a real SSH link
- [ ] UA-30: away mode started, edited and ended on main and the peer from main's board, with one status and one brief, through a real SSH link
- [ ] UA-03: the owner CLI socket, the peer rows in the CLI lists and the forwarded card writes, with no walk on the bench
- [ ] UA-15 part 2: a peer's answer on the tile, in the Inbox and in the ask panel, and Mark read forwarded, over a real `act` link
- [ ] UA-18: the ask panel and its Cancel, on this host and on a peer (checked in fixture mode only)
- [ ] UA-17: `tiller ask show <ask> --mark-read` from the owner's shell, then the Inbox and `tiller recap`
- [ ] Every owner decision above, as each one lands
- [ ] UA-19: the daemon's `recovery` item for a worker that waits 5 minutes with no item (R23), and the age of the report on the tile
- [ ] UA-38: a new request or a rejection withdraws the notes of the PR's earlier approvals, and Withdraw works on a note whose approval ended
- [ ] UA-46: a plain `tiller spawn` uses the owner's unused "Start now anyway" answer (`--past-quota-floor` is a no-op)
- [ ] UA-51: a start on a card with a dated hold is refused, and a hold or release by the owner sends one note to the orchestrator

## Test failures on the macOS machine

These fail without any change of the audit. Find out if they also fail on a quiet machine and on Linux.

- [ ] `tiller-cli::startup startup_is_under_ten_milliseconds` (CPU median about 24 ms)
- [ ] `tiller-cli::hook hook_is_inside_the_ten_ms_budget_with_the_daemon_down` (CPU median about 27 ms)
- [ ] `tiller-cli::holder` - two tests (a slow subscriber, and screen read with scrollback)
- [x] `tiller-cli::holder_upgrade` a counter through the upgrade (also flaky on Linux): the test's wrapper ran `cat` as the holder's pid after the exec, and the shell's `waitpid(-1)` reaped a pane's child before the new image could. Fixed in the test, which now taps the record without a child
- [ ] `tiller-cli::holder_upgrade` field by field: its record no longer reads `Short` (the same wrapper fix: macOS `/dev/fd` shares the record fd's offset), but on macOS 300 000 bytes queued to a raw PTY whose app reads nothing never reach the app, with no upgrade at all
- [ ] `tiller-cli::holder_upgrade` a close under way at the upgrade: on macOS the pane's shell keeps the PTY as its controlling terminal, so its output never ends while it lives, and the close's kill comes first
- [ ] `tiller-cli::holder_agents a_lost_holder_resumes_its_worker_and_notifies_the_owner_once`
- [ ] `tiller-cli::soak a_short_soak_has_no_gap`

## Polish (P2, open)

- [ ] UA-07 and UA-08: the host out of the owner's way
- [ ] UA-10: `tiller host edit` needs a rev that no command prints
- [ ] UA-11: cards with nothing running sit in "Working"
- [ ] UA-13: the away brief counts runs outside the away scope
- [ ] UA-14: an ask from the CLI is labelled "script"
- [ ] UA-23: small CLI refusals with no next step
- [ ] UA-24: small board defects from journeys 3 to 5
- [ ] UA-27: `tiller recap` lists raw events
- [ ] UA-28: "Needs you" lists every project with nothing in it
- [ ] UA-29: quota that runs out - the machine row says nothing of quota
- [ ] UA-31, UA-32: away mode, watch and steer - small gaps
- [ ] UA-34, UA-35: first run and scale - small gaps
- [ ] UA-37, UA-39: decide - small gaps
- [ ] UA-41: an agent that waits on a prompt - small gaps
- [ ] UA-45, UA-47, UA-50, UA-54, UA-58, UA-60: small gaps from the passes
- [ ] UA-61: `attention list` lists answered items, `approval` has no `list`, the spawn rate limit gives no time
- [ ] UA-62: a hyphenated option label wraps, and `card create` takes the project as a positional argument while `card list` takes `--project`

## Ship the audit branch

- [ ] Independent `/code-review` of the audit branch (30 commits)
- [ ] `no-mistakes`, then a PR
- [ ] Merge on the owner's go
