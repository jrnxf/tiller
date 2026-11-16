# Launch checklist

What is still open before Tiller has a proper launch. Tick a box in the same commit that does the work. Details of each `UA-` item are in `usability-audit.md`, under "Findings" and "Needs owner".

Started Monday 2026-11-16, Lisbon time.

## Owner decisions (spec changes)

- [x] UA-57 (P0): a `local-only` project starts its runs from the checkout's default branch when it is ahead of `origin` (`docs/spec/state.md` section 3, `base_sha`)
- [ ] UA-15 part 2 (P0): the answer to an ask sent to a peer project shows on the board that sent it
- [ ] UA-18: cancel and open an ask on the board (`POST /api/v1/asks/{id}/cancel`, `GET /api/v1/asks/{id}`)
- [ ] UA-19: a worker's `blocked` or `needs-decision` report puts its card in "Needs you" (a `recovery` item)
- [ ] UA-20: "Start" a run on the board (`POST /api/v1/cards/{id}/runs`)
- [ ] UA-38: a note about an ended approval leaves the list
- [ ] UA-46: a plain `tiller spawn` uses the owner's unused "Start now anyway"
- [ ] UA-51: the store refuses a start on a held card
- [ ] UA-53: one spec sentence for `approvals.repo` of a `local-only` project with no repo
- [ ] UA-01: the spec names `refuses_runs` and the `floors` doctor check
- [ ] UA-17: "Mark read" in the CLI (`tiller ask show <ask> --mark-read`)
- [ ] UA-09: a short answer for a person at a terminal, the full record with `--json` or `--full`
- [ ] UA-03: the owner's CLI reaches a peer's work through the local daemon
- [ ] UA-30: one "Away" for every paired host
- [ ] UA-43: grants for a peer's project from one board
- [ ] UA-07: show the host chip on a card only when the host needs attention

## Work after the decisions

- [ ] Build each decision that gets a yes, with a test that fails without it
- [ ] Walk a seventh full pass on a new bench: all 11 journeys, on the board with one page left open and on the CLI, with the work on main and on the peer
- [ ] Walk UA-57 again on the bench: land a card in a `local-only` project, then start the next run
- [ ] Stop the passes when one full pass finds no new P0 or P1

## Test failures on the macOS machine

These fail without any change of the audit. Find out if they also fail on a quiet machine and on Linux.

- [ ] `tiller-cli::startup startup_is_under_ten_milliseconds` (CPU median about 24 ms)
- [ ] `tiller-cli::hook hook_is_inside_the_ten_ms_budget_with_the_daemon_down` (CPU median about 27 ms)
- [ ] `tiller-cli::holder` - two tests (a slow subscriber, and screen read with scrollback)
- [ ] `tiller-cli::holder_upgrade` - three tests (a handover record reads `Short`, a close under way at the upgrade, a counter through the upgrade)
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
