# CLI and board parity

What the owner can do from the `tiller` CLI, what from the board, and where the two differ. Both surfaces call the same core functions in `tiller-core`, so a rule such as "close cancels the open obligations" holds on both. This page lists the surfaces, not the rules. The rules live in [`../spec/state.md`](../spec/state.md), [`../spec/protocol.md`](../spec/protocol.md) (sections 6 and 7) and [`../spec/authority.md`](../spec/authority.md).

Checked against the code on Tuesday 2026-11-10 (Lisbon time).

How to read the table:

- **Full**: the surface does it with the same effect.
- **Part**: the surface does it with fewer options. The Notes column says which.
- **No**: the surface does not do it.

The board is a client of the daemon's API (`crates/daemon/src/api.rs`). The CLI writes `state.db` directly. Board mutations always carry an `idempotency_key`. The CLI mints one per run unless you pass `--key` (protocol 6.3).

## Capabilities

| Capability | CLI | Board | Notes |
|---|---|---|---|
| Create a card | Full: `card create` | Part | The board takes project, kind, title, priority, category and body. It has no size, acceptance, fixes or ask link. It also records a Board ask and notes the orchestrator. |
| Edit a card | Full: `card edit` | Part | The board edits title, body and priority. An empty value clears a field on the CLI. The API sets an empty body to an empty string. |
| Comment on a card | Full: `card comment` | Full | Both need a live orchestrator to read the comment. Both cap a comment at 20,000 characters. |
| Hold and release | Full: `card hold`, `card unhold` | Full | Both on the card page. The CLI accepts a date, a local datetime or RFC 3339. The board sends an instant it builds from a Lisbon date and time. |
| Close a card | Full: `card close --outcome` | Part | The board Close dialog offers Done, Won't do and Duplicate, and always asks for a reason. |
| Complete an ops card | Full: `card complete --evidence` | No | The board closes an ops card as Done, which uses the reason as evidence. |
| Cancel and reopen | Full: `card cancel`, `card reopen` | Full | Both need a reason. |
| Card dependencies | Full: `card dep add`, `card dep rm` | Full | The card page's Depends on row: a chip per dependency with an x to remove it, and Add, which picks an open card of the same project and turns off the cards that would make a loop. Both run the same core checks. The board changes only this host's cards. |
| Read cards | Full: `card show`, `card list`, `card timeline` | Full | The card page and the lanes. |
| Add a note to the orchestrator | Full: `ask note` | Full | The Ask dialog sends one note per mentioned project. Both cap a note at 8,000 characters. |
| Resolve, link and list asks | Full: `ask resolve`, `ask link`, `ask list`, `ask show` | No | These are orchestrator tools. |
| Answer an attention item | Full: `attention answer` | Part | The CLI can cite a chat ask with `--ask`. The board answer has no ask citation. Both cap a typed answer at 20,000 characters. An answer cited with `--ask` is the owner's recorded words and has no cap. |
| Withdraw an attention item | Full: `attention withdraw` | Part | The board withdraws only stale items, with a fixed reason, and does not forward the call to a peer. |
| Open and list attention | Full: `attention open`, `attention list` | Part | The board lists the items in the Inbox. `attention open` is an agent tool. |
| Decide an approval | Full: `approve` | Full | An accept names the head commit. The CLI takes `--sha` and the board sends the head it shows. |
| Land a change | Full: `land` | No | |
| Grants | Full: `grant create`, `grant revoke` | No | |
| Away mode | Part: `away start`, `away end`, `away brief` | Full | The board also edits an away window in place. |
| Recap | Part: `tiller recap` | Part | Both read the same events. Only the board marks the recap seen. |
| Start an orchestrator | Full: `orchestrator start` | Part | The board has no restart. |
| Restart stale sessions | Full: `agent restart --stale` | Full | The CLI acts on this host. The board acts per host. |
| Focus an agent | Full: `agent focus` | Part | The CLI prints the pane and machine. The board opens the configured terminal app. |
| Run and session control | Full: `agent relaunch`, `resume`, `interrupt`, `stop`, `send`, `inbox`, `status`, `run`, `pr`, `done`, `obligation`, `artifact`, `review`, `handoff`, `spawn` | No | Agent tools and owner controls. |
| Add a project | Full: `project add` | Part | The board takes id, name, repo, delivery, capacity and a checkout (a path or a clone). |
| Edit a project | Full: `project edit` | Part | The board edits a project this host owns, but not `fleet`. The id stays fixed. Checks are one command per line, with an optional lighter set. |
| Stage a project | Full: `project stage` | Part | The CLI also has `--from <commit>` for pre-launch. |
| Retire a project | Full: `project retire` | Full | The CLI flag is `--stop-agents`. The board has a checkbox. |
| Delete a project | Full: `project delete` | Full | The CLI asks for the id at a prompt, or takes `--yes`. The board asks for the typed id and shows a preview. |
| Config | Full: `config set`, `get`, `hash`, `diff` | Part | The board sets only `spawn.pool_root`, `spawn.checkouts_root` and `checkouts.<id>`. |
| Pick the bug project | Full: `host edit --bug-project`, `--no-bug-project` | Full | |
| Host lanes, floors, label and boundary | Full: `host edit` | Full | The board edits this host only, in its Settings > Hosts card, with the floors in GB. Both refuse a rename while a peer is paired and a boundary change once anything records the boundary. |
| File a Tiller bug | Full: `bug file` | No | The board closes or moves a triage card, but does not file one. |
| Peers | Full: `peer add`, `accept`, `list`, `remove`, `revoke`, `serve`, `pane` | Part | The board adds a host. |
| Memory | Full: `mem note`, `propose`, `approve`, `retro`, `learn` | Part | The board edits, decides (Keep or Discard) and shows revisions. |
| Read the fleet | Full: board, agents, machines, quota, health, events, snapshot | Full | |
| Owner marker | Full: `owner set`, `owner show` | No | While the marker names another system, both surfaces refuse every mutation with `owner_marker` (exit 6, HTTP 403). The check is in the store, so they share it (`recovery.md` R20). The board shows the refusal in the daemon's words. It has no read-only banner yet. |
| Sign in | Full: `tiller board --login` issues a code | Full | The board signs in with the code and signs out. |
| Run the daemon | Full: `tiller daemon` | No | |

## Board only

- The live terminal: stream, input, fit and history.
- Answering a permission prompt in an agent pane.
- Relabelling an intervention.
- Editing memory and reading its revisions.
- Folder, checkout and setup previews, and the start settings.
- Signals and contributions on the Health page.
- Editing an away window in place.
- Marking a recap seen.
- Acting on a paired host's records from this board (the `host` field).
- Signing out.

## Differences that are intended

- **Board create records an ask.** Every message the owner sends an orchestrator is an `asks` row (protocol 2.1). A board create, its ask and the wake for the orchestrator are one transaction. The key comes from the click, so a double click never makes two cards. The CLI `card create` is a plain create.
- **The board withdraws only stale items.** A withdrawn item stops being a question, and a stale item stays open until someone withdraws it (state.md). The board is the owner's way to clear one. The CLI can withdraw any item because an agent or the owner may know it no longer applies.
- **An accept names the head.** The owner's accept binds the head commit they reviewed (authority.md). The CLI makes that explicit with `--sha`. The board sends the head it shows, and the daemon refuses a mismatch with the bound head.
- **The CLI mints a key per run.** A weekly script may send the same note twice on purpose (protocol 6.3). Pass `--key` to make a retry safe. The board mints a key when the control renders and reuses it on a retry.
- **Hold dates parse differently.** The CLI reads a date as 09:00 in Lisbon. The board builds an exact instant from the date and time the owner picks, so the API takes RFC 3339 only.
- **`tiller daemon` refuses `--db`.** It must use `$TILLER_HOME/state.db`. Honoring `--db` alone would split one daemon across places (the lock, the backups and the mirror).
- **The board sets few config keys.** It sets only the keys a start reads, as the fix flow in Settings "Starting agents".
- **Project add stays narrower on the board.** The spec keeps `--checks`, `--remote`, `--default-branch` and the deploy flags on the CLI (state.md).
- **Reason not recorded: actor `board` and `owner`.** Authority gives both the same rights. No doc says why they are separate actors.
- **Reason not recorded: an empty body on edit.** The CLI clears the field. The API sets an empty string.
- **Reason not recorded: CLI-only controls.** Dependencies, `land`, grants, `orchestrator start --restart`, peer management beyond add, and the memory undo route (the API has it, and no surface calls it).
