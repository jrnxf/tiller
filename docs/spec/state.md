# State model

This is the contract for `state.db`, the one SQLite database each host keeps as the authority for its workflow records (ADR 0002). It names every entity, the facts each one owns, how the board's columns are derived from those facts, and the transaction and event behind every mutation. `schema.sql` next to it is the draft DDL; T2.1 turns both into `crates/core`.

Written Monday 2026-11-02, Lisbon time (WET). Task T1.1. Status: approved at CP2 (Monday 2026-11-02), with the owner's decisions from `cp2-open.md` applied. A contract change from here on needs a stop-and-ask. Amended by ADR 0003 (federated board), accepted Tuesday 2026-11-03: principle 1, Host, Peer key, sections 8, 9, 10 and 11.

Every other spec builds on this one: recovery (T1.2) repairs these records, the protocol (T1.3) exposes them, authority (T1.4) says who may mutate them, and memory (T1.5) adds the tables that are never loaded into context.

## 1. Principles

1. **One authority per host.** `state.db` at `~/.tiller/state.db` owns asks, cards, runs, attention, approvals, reservations, the outbox and events for the projects this host owns. A project has exactly one owner host in v1. Another host's cards never enter this database as rows. A paired peer's cards are read live over the peer link and held only in the daemon's memory, as a disposable cache (`protocol.md` section 10); a change to one is made by the peer, in the peer's own `state.db`, from a forwarded owner mutation the peer verified (ADR 0003). Hosts that are only herdr saved machines still appear as pane caches.
2. **Facts, not columns.** The database stores lifecycle facts: work state, holds, dependencies, attention, run and session activity, PR state. The board column is computed from them by the precedence table in section 6 and is never stored.
3. **One owner per fact.** Section 9 names the record that owns each fact. Everything else is a cache with a `fetched_at`, or a projection that can be rebuilt by a query.
4. **Every mutation is one transaction with its event.** All writes go through `tiller` or the daemon, in `BEGIN IMMEDIATE`, in WAL mode with `busy_timeout`. The record change and its event row commit together or not at all. Hooks write the same way, without the daemon.
5. **Edits carry `rev`; transitions carry preconditions.** A field edit (title, priority, reason) must name the `rev` it was made against and is refused with a readable conflict when stale. A state transition (close, start a run, answer a decision) is guarded by the facts it needs (open, no open attention, same head SHA), which is what makes a stale click harmless.
6. **Stable ids inside, short ids outside.** Every row has a ULID primary key. Cards also carry a display id, `alpha-42`, minted from a per-project counter in the same transaction that creates the card.
7. **Nothing destructive is derived.** No query result closes a card, removes a worktree or merges a PR. Those are explicit mutations with their own preconditions, and reconcile (T1.2) may only open attention items when evidence conflicts.

## 2. Identity

### ULIDs

Every entity's primary key is a ULID: 26 characters of Crockford base32, time-ordered, minted by the writer (`ulid` crate). ULIDs are stored as `TEXT` and sort by creation time. They never change, are never reused, and are the ids the CLI, the API and the SSE stream use. The CLI also accepts an unambiguous short form of a ULID (its last 7 characters) wherever it accepts a ULID (`protocol.md` section 6.1).

### Display ids

Cards get a display id for chat and the board: `<project>-<n>`, for example `alpha-42`. `n` is `projects.next_card_seq`, read and incremented inside the `card.create` transaction, so two concurrent creates cannot mint the same id. A display id is unique per host (`cards.display_id` is unique) and survives the card's whole life, including import: an imported Firstmate task keeps its original id as `legacy_id` and gets a fresh display id. A display id is never reused while anything still names it. A deleted project's id that is added again starts at `<project>-1` only when none of the deleted project's run branches is left (section 8, Deleting a project).

Project ids are short lowercase slugs, 2 to 32 characters: words of lowercase letters and digits joined by single hyphens, each word starting with a letter (`^[a-z][a-z0-9]*(-[a-z][a-z0-9]*)*$`), so an id can match its repo, as in `acme-web2-staging`. The id `fleet` is reserved on every host for host-level work that belongs to no project.

No word of a project id starts with a digit, so the first hyphen-separated part of a display id that does is its card number: `acme-web2-staging-12` is card 12 of `acme-web2-staging`, and `acme-web2-staging-12-build-2` is that card's second build. A display id splits at its last hyphen, and `foo-12` is never a project id. Ids from before schema 7 had no hyphen and at most 16 characters; they still parse unchanged.

Runs are addressed through their card and role: `alpha-42 build`. The herdr agent name is `<display id>-<role>`, `alpha-42-build`; a second run of the same role on the same card appends its attempt number, `alpha-42-build-2` (`runs.attempt`).

Asks, attention items, approvals and outbox messages are addressed by ULID. The board shows them with their card's display id beside them; the CLI prints both.

## 3. Entities

Each entity lists the facts it owns. Columns the entity only caches say so.

### Host

`hosts`: this host and every saved herdr machine it can see.

- `id` (ULID), `label` (the role label the owner uses, for example `host-a`), `kind` (`local` or `remote`), `herdr_machine` (null for local, the saved machine id for remote).
- `lane_capacity`: how many runs may be active at once on this host (required for the local host, null for remote ones). `ram_floor_mb`, `disk_floor_mb`: the free-resource floors `tiller spawn` checks live (P03).
- Why capacity and the floors live in `state.db`, not the config file (decided at CP2, C11; reason corrected by S09): the capacity check is a query inside `run.start`'s `BEGIN IMMEDIATE` transaction, which compares open reservations with `hosts.lane_capacity` and `projects.capacity` in one read, and the `capacity` view (free slots per host) that the Machines section and spawn read is defined over the same rows. A config file cannot take part in that transaction. Quoting the checked value is not the reason: a config value could be quoted in the `run.started` payload just as well, and every value checked is recorded there either way. The floors stay beside the capacity so one owner command (`tiller host edit`) changes all three with a `rev` and an event.
- `last_seen_at`, `stale_reason` (cache from the herdr poller, T2.5, and from the peer link for a paired peer): feed the Machines section. For a peer, `stale_reason` adds `peer_link_down` (the link is not live), `peer_refused` (the peer refused the hello: a revoked key, `federation.accept = false`, or a changed `host_id`) and `peer_version` (no common peer protocol version).
- `peer_scope` (`read` or `act`; null for the local host and for a herdr-only machine) and `peer_host_id` (the peer's `meta.host_id`, recorded at pairing): set by `peer.pair` on the board host (ADR 0003). The link checks `peer_host_id` on every hello, so a peer whose database was replaced is refused until the owner runs `tiller peer remove` and `tiller peer add` again. The peer's private signing key is a file, `~/.tiller/peers/<label>.key` (mode 0600), never a column.

The owner marker is a file, `~/.tiller/owner`, not a row (plan v2, point 15). `tiller` refuses every mutation while the marker names another system. `meta.host_id` records which host this database belongs to, so a copied database cannot be mistaken for another host's.

### Peer key

`peer_keys`: on a host that accepted a pairing, one row per board host allowed to read it, and to act on it when the scope is `act` (ADR 0003). This is the only record that authorizes a forwarded mutation.

- `id` (ULID, the key id), `viewer_label`, `viewer_host_id` (the board host's `meta.host_id`), `public_key` (Ed25519, base64), `scope` (`read` or `act`), `accepted_at`, `revoked_at`, `rev`.
- At most one unrevoked row per `viewer_host_id`. A new pairing from the same board host revokes the old row in the same transaction.
- The public half only. A fleet role that read it would gain nothing, and `~/.tiller` is denied to fleet roles anyway (`authority.md` G20).

### Project

`projects`: one row per project this host owns, plus `fleet`.

- `id` (the slug, primary key), `name`, `repo` (`owner/name` or null for local-only), `remote` (git remote URL; at add a github.com remote fills `repo` when none is given, and a Gerrit remote, an SSH remote on port 29418, is refused unless the project is `local-only`), `default_branch`, `delivery` (`no-mistakes`, `no-mistakes-prod-only`, `direct-pr`, `direct-push`, `local-only`; P10 and P11), `checks` (JSON: the commands a direct-push worker runs before it reports done: an array of commands, or `{"full": [...], "no_logic": [...]}` where `no_logic` is the lighter set for a change that touches no logic; any other shape is refused on add and edit), `capacity` (max active runs for this project), `owner_host_id`, `added_at`, `next_card_seq`, `rev`, `retired_at`.
- `deploy_workflow` (the workflow file a `direct-push` project deploys with, or null), `deploy_environment` (the GitHub environment that deploy must reach, or null when the workflow has none), `deploy_timeout_minutes` (default 60): what "the deploy was watched to its result" matches against (section 7; decided at CP2, C02 amended by S07).
- `merge_gate`: `enforced` or `detect` (decided at CP2, S03). `enforced` only after the daemon's ruleset check confirms the repo's default branch accepts updates from the daemon's GitHub App alone; `detect` is the labelled fallback (`authority.md`, section 6).
- `stage` (`pre-launch` or `launched`, default `launched`), `reviewed_through_sha` (the last commit a batched review covered), `stage_changed_at`: pre-launch validation (`docs/design/pre-launch-validation.md`). A pre-launch project skips `no-mistakes` and gets one chunked review a week instead. The derived `effective_delivery` is what builders and the land loop follow: `pre-launch` with `no-mistakes` or `no-mistakes-prod-only` gives `direct-pr`; every other pair keeps `delivery`.
- The project's own checkout path is not stored; treehouse and the dotfiles own it.
- `tiller init` creates one `ops` card in `fleet` per host, display id `fleet-1`, titled "Host health". Host-level recovery items (T1.2: backups, migrations, resource floors, a crashing orchestrator) attach to it, so `attention.card_id` stays required. It never closes: `card.close` and `card.cancel` refuse it.

Ownership moves between hosts only through export and import (M6). There is no in-place reassignment in v1.

A project ends in one of two ways. `project.retire` sets `retired_at` and keeps every row: the project leaves the board, stays listed as retired in the CLI, and its id stays taken. There is no command that un-retires a project. `project.delete` removes the row and everything recorded for it on this host, which frees the id for a later `project.add` (section 8, Deleting a project).

### Session

`sessions`: every Claude Code session Tiller launched or recognized: the orchestrator per project and each worker session.

- `id` (ULID), `claude_session_id` (the harness id, unique when known), `role` (`orchestrator`, `scout`, `builder`, `reviewer`, `verifier`), `project_id`, `run_id` (null for an orchestrator), `host_id`, `pane_id` (cache from herdr), `started_at`, `ended_at`, `end_reason`.
- `activity`, `activity_observed_at`, `activity_source`: the newest observation's verdict, copied in the same transaction that inserts the observation. These three columns are a projection of `observations` kept for cheap reads; the observation row owns the fact.
- `transcript_path`: where the tailer reads (host-capabilities, transcript rule).
- `harness`: which agent harness runs the session, `claude` unless an adapter for another one exists (parity D04, kept at CP1; `protocol.md` section 10).
- `config_hash`: the configuration that produced this session: role file, skills, settings fragment, memory revision, model, CLI version (plan v2, point 22).

An orchestrator session has no run. Its card-level facts are its project's.

### Ask

`asks`: every message the owner sends an orchestrator, and every note a script drops (P27, P60). Recorded by the `UserPromptSubmit` hook or `tiller ask note` before the agent reads it.

- `id` (ULID), `project_id`, `session_id` (null for a script note), `source` (`prompt`, `board`, `script`, `import`), `text`, `received_at`.
- `disposition`: `unresolved` until `tiller ask resolve` runs, then `new` (a card was created), `followup` (an existing card was touched), `chat`, or `cancelled`. `resolved_at`, `resolved_by_session_id`.
- A board "add card" records an ask with `source = board` and resolves it to `new` in the same transaction as the card create, so the wake-up line the orchestrator sees never creates a second card (plan v2, point 13).

Ask text is work data. It stays in this database and is never exported to git.

### Card

`cards`: the unit of work. Every ask that is not chat becomes or touches a card.

- `id` (ULID), `display_id`, `legacy_id` (the Firstmate task id on import, else null), `project_id`, `kind`: `ship` (a change that lands), `scout` (research that is reported), `question` (something only the owner can answer; kept as its own kind at CP2, C09, so a bare question has a display id to tag in chat), `ops` (an operation with evidence, such as a cutover step).
- `title`, `body` (markdown brief), `priority` (0 highest to 4), `category`, `size` (`S`, `M`, `L` or null; never estimated by the importer, plan v2 point 20), `acceptance` (text), `fixes_card_id` (a later defect pointing back at the card that introduced it).
- `dispatch_override`: JSON `{"model": ..., "effort": ...}` or null. The owner's per-card override of the `[dispatch]` rules (decided at CP2, C04; `protocol.md` 6.4). Only the owner sets or clears it; `card.edit` refuses the field from any session.
- `state`: `open` or `closed`. `outcome` (null while open): `landed`, `reported`, `answered`, `completed`, `cancelled`, `failed`, `abandoned`, `superseded`. `closed_at`, `closed_by` (session or `board` or `daemon`).
- `created_at`, `created_by_ask_id`, `rev`.

A card never stores a column, a hold, a dependency, an attention state or an activity. Those live in their own tables below, so each has one owner and the column can be derived.

### Ask to card link

`ask_cards`: many to many. One ask can create several cards; several asks can touch one card.

- `ask_id`, `card_id`, `relation` (`created`, `followup`), `linked_at`, `linked_by_session_id`.
- Written by `ask.resolve` in the same transaction as the disposition.

### Dependency

`card_deps`: `card_id` waits on `depends_on_card_id` (P32).

- `added_at`, `added_by`, `released_at`, `release_reason` (`landed`, `reported`, `answered`, `completed`, `removed`).
- A dependency is released by reconcile in the transaction that closes the dependency card with a satisfying outcome, or removed by hand. A dependency on a cancelled or failed card is not released; it opens an attention item instead, so the owner decides.

### Hold

`holds`: a dated park (P19, P30). At most one open hold per card.

- `id`, `card_id`, `until_at` (the re-ask moment in UTC, set from a Lisbon date), `reason`, `set_at`, `set_by`, `released_at`, `release_reason` (`due`, `answered`, `removed`).
- A hold with `until_at` in the future keeps the card in Queued with no alarm. When `until_at` passes, reconcile releases it and opens an attention item of kind `decision` with the hold's reason as the question, in one transaction. That is how a dated hold re-asks on its date.
- Undated waits are not holds; they are attention items (P31), because what they wait for is the owner's word.
- Blocked waits are not holds; they are dependencies.

### Attention

`attention`: everything that needs the owner (P29, P31, P35, plan v2 point 36). One row per question; a card may have several over its life.

- `id`, `card_id`, `kind`: `decision` (a product or direction question), `credential` (a secret or login only the owner can supply), `merge` (a PR ready and the project has no standing grant), `review` (a page or artifact to look at and answer, P47), `recovery` (reconcile found conflicting evidence and will not act), `report` (a scout finished with choices, P33).
- `question`, `evidence` (text or a JSON list of pointers: event ids, artifact ids, PR urls), `recommendation`, `consequence` (what happens after each answer), `options` (JSON list, optional): each a bare label or `{label, consequence, recommended, action}`. `action` is an owner action, the owner-only change the option makes when the owner picks it. The one kind is `project_capacity` (`project`, `from`, `to`): `from` is the capacity the question saw.
- `opened_at`, `opened_by_session_id`, `opened_by_run_id`.
- `answered_at`, `answer` (the owner's exact words), `answered_via` (`board`, `chat`, `cli`), `answer_idempotency_key`.
- `withdrawn_at`, `withdraw_reason`: the item stopped being a question (the subject landed, the run was cancelled, the project was deleted). A withdrawn item was never answered and says so.
- `subject_session_id`, `subject_project_id`: what the question is about, when it is about more than its card. `subject_session_id` is a live session the question asks the owner to act on (a session that is blocked or stuck); an item about a crash names no session, because it asks what to do after the session ended. `subject_project_id` is the project a session-related or merge-gate item is about. Both are soft ids, not foreign keys: a project delete keeps them so the page can still say what became of the subject.
- `rev`.

Rules:

- A card cannot close while it has an open attention item. `card.close` is refused with the item's id. Cleanup never closes a question (matches Firstmate's hold lifecycle).
- `still_current` is derived, not stored: open, card open, the run that opened it is still active or the kind does not need a run, the subject project exists and is not retired, and the subject session has not ended. When it is false the read also carries `stale`: `reason` (`card_closed`, `run_ended`, `session_ended`, `project_deleted`, `project_retired`), the `subject` id the reason is about and `at` when the state records it. The board shows the reason and offers Withdraw instead of the options; the answer path checks it and warns when the item is no longer current but still records the answer.
- A stale item stays open until someone withdraws it. `attention.withdraw` from the board is the owner's way to clear one; `project.delete` withdraws the items about the project itself (section 8, Deleting a project).
- Permission prompts are not attention rows. They are derived from the newest observation of a session (`activity = waiting_permission`) and shown in their own section, so a decision list never fills with dialog clicks (plan v2, point 36).
- An owner action is checked when the item opens (the project is the card's own, exists and is not retired, its capacity is `from` now, `to` is at least 1 and differs, and the option has a label no other option on the item uses) and again when it is picked. Picking the option means the answer's first line is its label, ignoring case and surrounding space; the board sends the label, then a blank line and the owner's note. The action applies in the answer's own transaction, and only when the owner principal answers here: the board or an owner terminal. The orchestrator recording chat words and a forwarded peer answer are refused (`forbidden`), so a chat "yes" relayed by an agent never edits a project (`authority.md`, section 3). A capacity that moved since the question is refused (`refused`, naming the current value), and so is any action on an item that is no longer current; the item stays open, so an old question never undoes a newer change, and another option still answers it. An `action` that does not parse (an item opened before actions existed) applies nothing. The `attention.answered` payload carries `applied`, and the answer message to the asker ends `applied: <what changed>` or `applied: nothing` whenever the item offered an owner action.
- Answering records the exact words and resolves the item in one transaction. A second answer with the same idempotency key returns the first result and changes nothing (`idempotency_keys`; the key is also unique on the row). A different answer to an already answered item is refused: one row holds one answer.

### Run

`runs`: one attempt by one worker role on one card.

- `id`, `card_id`, `role` (`scout`, `builder`, `reviewer`, `verifier`), `attempt` (1, 2, ... per card and role), `predecessor_run_id` (set by a relaunch that keeps the worktree, P24), `model`, `effort`, `host_id`.
- `worktree_path`, `branch`, `base_branch`, `base_sha`: what the run works in. Treehouse owns the worktree; these are the values recorded at spawn. `base_sha` is the remote default branch's head after a fetch made by the spawn, so every run starts from current origin (P05); a stale checkout is fast-forwarded first, never worked on.
- `supervisor_session_id`: the orchestrator session that owns this run. Exactly one at a time (P21). Reconcile reassigns it to the project's live orchestrator after a restart, with an event.
- `started_at`, `ended_at`, `outcome` (`landed`, `reported`, `failed`, `abandoned`, `relaunched`, `cancelled`, `cut`), `outcome_note` (for example the partial-work note a 30-minute cut leaves, P55).
- `pause_until`: a worker's dated pause (`paused ... until`), which silences alarms until then (P19).
- `last_status_line`, `last_status_at`: the worker's newest status line, copied from its status observation in the same transaction.
- `config_hash`, `idempotency_key` (from the spawn), `rev`.

A run is active while `ended_at` is null. Several runs of different roles may be active on one card (build and review). Two active runs of the same role on one card are refused (unique partial index), and so are two live sessions on one run (unique partial index on `sessions`): a relaunch or a resume ends the old session row in the transaction that inserts the new one.

A run's sessions are in `sessions` (`run_id`). A resume after a reboot that keeps the Claude session id keeps the session row; a relaunch with a fresh session adds a session row to the same run when the worktree and brief are reused, and the run's `predecessor_run_id` is used only when the relaunch is recorded as a new attempt (a new brief). T1.3 fixes which command does which.

### Pull request

`prs`: GitHub owns PR state; this is the cache with its fetch time.

- `id`, `project_id`, `run_id` (the run that opened it, nullable for a PR found by reconcile), `card_id`, `number` (in the project's repo), `url`.
- Cached from the forge: `state` (`open`, `merged`, `closed`), `head_sha`, `base_ref`, `checks` (`pending`, `success`, `failure`, `none`), `review_decision` (`approved`, `changes_requested`, `none`), `mergeable` (`clean`, `dirty`, `blocked`, `unknown`), `merged_at`, `merge_sha`, `fetched_at`, `fetch_error`.
- `pipeline_state` (`none`, `running`, `passed`, `failed`): the `no-mistakes` run state, cached from its records.
- `linked_at`, `linked_by`.

A run may have several PRs (a replacement PR). The newest open one is the run's PR for column derivation.

### Publish

`publishes`: one row per direct-pr builder run whose worker described its PR (`authority.md` section 6, "Publishing a direct-pr run"). The worker only commits; the daemon pushes the branch and opens the PR.

- `run_id` (primary key), `title`, `body`, `described_at`: the PR the worker described with `tiller pr describe`.
- `pushed_sha`, `pushed_at`: the head the daemon last pushed. `pr_id`: the PR it opened or found, never replaced by another.
- `attempted_sha`, `attempted_at`, `error`: the last failed publish and its real error, cleared by the next success.

The record is current for a head when that head is pushed, `pr_id` is set, `error` is null, and both `described_at` and the run's `last_status_at` are earlier than `pushed_at`. `publish.done` on a current record writes nothing; the daemon publishes a run again when it is not.

### Approval

`approvals`: a bound decision to execute one action (plan v2, point 16).

- `id`, `kind` (`merge`, `memory`, `land_direct`, `away`), `card_id`, `run_id`, `project_id`.
- Binding, by kind:
  - `merge`: `card_id`, `repo`, `pr_id`, `head_sha`, `action` (`merge`, `squash`, `rebase`).
  - `land_direct`: `card_id`, `repo`, `branch`, `head_sha`, `action = push`.
  - `repo` is a snapshot of what was approved; it stays even if the project's repo is renamed later.
  - `memory`: `diff_sha256`, `base_rev` (the memory file's git revision), `action = apply`.
  - `away`: `scope` JSON (the standing orders and spend cap), `action = away`; T1.4 owns the shape.
- `state`: `requested` (a decision was asked for), `accepted` (the owner approved, or a standing grant matched), `rejected`, `executing` (the daemon has started the action), `succeeded`, `failed`, `uncertain` (the action was sent and its result could not be read; reconcile resolves it from the forge), `stale` (the binding no longer matches reality: the head SHA moved, the PR closed, the base revision changed).
- `requested_at`, `requested_by`, `decided_at`, `decided_by` (`owner` or `grant:<id>`), `decision_idempotency_key`, `executed_at`, `finished_at`, `result` (JSON: merge SHA, error text).
- `grant_id`: the standing grant that accepted it, or null.
- `memory_paths`: for a `merge` or `land_direct` approval, the JSON list of source-map paths (`memory.md`, section 3) the bound diff touches, or null when it touches none. A non-null value is the board's "memory change" flag. `linked_approval_id`: set only on a `memory` approval accepted with "Approve memory change and merge" (decided at CP2, S01); it names the merge or land-direct approval it was accepted with. One merge approval can have several memory approvals (one per touched source), so the merge side finds them by reverse lookup.

Rules:

- Only the daemon moves an approval to `executing` and beyond, and only `tiller land` run by the daemon merges (T1.4). Before `executing`, the daemon re-reads the PR head; a mismatch marks the approval `stale` and opens a `merge` attention item with the new SHA.
- A PR refresh that sees a new head SHA while an approval is `accepted` marks it `stale` in the same transaction (T4.3 acceptance: a push after approval invalidates it).
- A click that arrives twice with the same `decision_idempotency_key` returns the first outcome and causes no second action.
- A `merge` or `land_direct` approval with `memory_paths` set is accepted only by the owner's "Approve memory change and merge" click, which writes one `memory` approval per touched source bound to that file's diff at `head_sha` and its base blob, in the same transaction. A grant never accepts it (`schema.sql` CHECK). A new head makes both halves `stale` together (`memory.md`, section 7).
- `grants` (standing approvals, P13) are per project and action with conditions (checks green, PR opened by a fleet run, branch pattern). A matching grant creates an `accepted` approval bound to the head SHA at that moment, so the audit trail is the same as a hand approval. `authority.md` (T1.4, section 6) defines the conditions and who may create a grant; away mode (P34) is an `away` approval whose temporary merge grants are ordinary `merge` grants.

### Reservation

`reservations`: capacity held for a run (plan v2, point 35).

- `id`, `host_id`, `project_id`, `run_id`, `reserved_at`, `reserved_by`, `released_at`, `release_reason` (`ended`, `spawn_failed`, `reconciled`).
- `spawn` reserves inside `BEGIN IMMEDIATE`: it counts active reservations for the host and the project, compares them with `hosts.lane_capacity` and `projects.capacity`, and inserts or refuses. Two spawns cannot both pass, because the second waits for the first transaction. The reservation is created before the worktree and pane exist; a spawn that fails later releases it with `spawn_failed` in its own transaction.
- A reservation is released when its run ends. Reconcile releases a reservation whose run is ended or whose run row is missing, and never the other way around.
- The RAM and disk floors (P03) and the quota floor (P04) are checked live at spawn, inside the same transaction window, and the values seen are written into the `run.start` event payload. Quota windows are cached in `quota_windows` (owned by `quota-axi`, fetched every 5 minutes, never with an account id).

### Outbox

`outbox`: every message to a session, stored before anything is typed (plan v2, point 13).

- `id`, `target_session_id`, `target_pane_id` (cache at enqueue time), `kind` (`wake`, `steer`, `answer`, `note`), `body`, `card_id`, `reply_to` (an ask or attention id), `idempotency_key`.
- `state`: `pending` (stored, not yet delivered), `delivered` (herdr confirmed the session is the target and the wake-up line was submitted: it left the composer after Enter), `acked` (the target ran `tiller inbox <id>`, which reads the body and acks in one transaction), `failed` (attempts exhausted, or the target session is gone and no successor exists).
- `attempts`, `next_attempt_at`, `delivered_at`, `acked_at`, `failed_at`, `failure_reason`.
- `rev`.

Rules:

- The pane only ever receives one line, `tiller inbox <id>`. The payload is the row.
- A retry checks that the pane still hosts `target_session_id` before typing; if the session moved, the message is retargeted to the successor session of the same run or project in a new transaction, with an event.
- `delivered` but not `acked` after the idle threshold, with the session observed idle, means the doorbell was missed: the daemon rings again (`attempts` grows, state stays `delivered`) (P23).
- A busy pane or a visible permission dialog defers delivery (`next_attempt_at`), it does not fail it.
- Enqueue with an existing `idempotency_key` returns the existing row.

### Observation

`observations`: append-only evidence about sessions. Hooks, the transcript tailer, the herdr poller and liveness checks write here. Nothing here is ever loaded into an agent's context.

- `id`, `session_id` (nullable when the source could not map it yet), `run_id`, `source` (`hook`, `transcript`, `herdr`, `liveness`, `status_line`), `event` (the hook event or detected kind: `UserPromptSubmit`, `Stop`, `StopFailure`, `Notification:permission_prompt`, `interrupt`, `session_end`, `pane_gone`, ...), `verdict` (the activity it implies: `working`, `idle`, `waiting_permission`, `waiting_input`, `errored`, `ended`, `unknown`, or null when it implies none), `observed_at` (when the source saw it), `recorded_at`, `payload` (JSON, with unknown fields kept).
- T1.3 maps every hook event and `notification_type` to exactly one `event` and `verdict`.
- The session's `activity` columns are updated in the same transaction when the new observation is newer than the current `activity_observed_at`. Out-of-order arrivals are stored but do not move the projection backwards.
- Staleness is derived: an activity whose `activity_observed_at` is older than the role's threshold reads as `stale` (thresholds: T1.2). `unknown` is explicit: a session with no observation yet, or one whose last source has gone away.
- Retention: 30 days, except rows referenced by an `evidence` row (T1.5) or a retro.

### Event

`events`: the append-only log, one or more rows per mutation, written in the mutation's transaction.

- `seq` (integer, autoincrement; the SSE `Last-Event-ID`), `id` (ULID), `at`, `txn` (the mutation name from section 8), `actor` (`session:<id>`, `board`, `board@<viewer label>`, `daemon`, `hook`, `import`, `owner`; `owner` is the developer acting through the CLI or the board, see T1.4; `board@<viewer label>` is the owner acting through a paired board host, ADR 0003), `entity` (`ask`, `card`, `run`, ...), `entity_id`, `kind` (`card.created`, `run.ended`, ...), `rev_after` (the entity's rev after the write, null for append-only entities), `card_id` (denormalized for timeline queries), `payload` (JSON, the fields that changed and the preconditions checked), `idempotency_key`.
- Events are never updated or deleted, with one exception: `project.delete` removes the events of the project it deletes, so that project's audit trail is gone for good, and writes one `project.deleted` tombstone in their place (section 8, Deleting a project). They are exported in the JSONL export (T2.3).
- The board's SSE stream carries `seq`, `entity`, `entity_id` and `rev_after`, so a client ignores an event older than what it holds and resumes from `seq` after a gap.
- Firstmate's fleet ledger events map onto these: `task.dispatched` is `run.started`, `task.status` is `run.status`, `task.pr_ready` is `pr.linked`, `task.merged` is `approval.succeeded` or `card.closed` with `landed`, `task.cleaned_up` is `run.cleaned`.

### Intervention

An intervention is a time the developer was needed: an attention item that was answered, an owner or board note, or a rework. It is not stored. The classifier (`tiller-core` `Store::interventions`, T7.1) computes it on read from `attention`, the outbox and `events`, and costs no tokens. First match wins:

1. Merge, review and report attention is `planned`.
2. A rework (a reverted or re-run attempt) is `rework`.
3. An owner or board note is `planned` when no run of the card had started, `correction` after.
4. A decision answered against its recommendation is a `correction`.
5. Everything else is an `unplanned_escalation`.

`intervention_labels`: only the owner's overrides are stored. `ref_id` (an attention id or the id of an owner note in the outbox; primary key), `label` (`correction`, `unplanned_escalation`, `rework`, `planned`), `labeled_at`, `labeled_by`. The Health page counts `correction`, `unplanned_escalation` and `rework` as avoidable and reports them per accepted outcome. Removing a label hands the item back to the classifier. Because the classifier stores nothing, improving a rule changes the numbers for history too, and a relabel is never lost.

### Obligation

`obligations`: a promise tied to an event (P15, P16). "Report it ready once CI passes" is a row, not a memory.

- `id`, `card_id`, `project_id`, `what`, `trigger_kind` (`event`, `date`, `standing`), `trigger_event` (an event kind and entity filter, JSON), `due_at`, `state` (`open`, `met`, `cancelled`), `opened_at`, `opened_by`, `met_at`, `met_by_event_seq`.
- Reconcile marks an obligation `met` in the transaction that writes the triggering event, and enqueues the outbox message that fulfils it in the same transaction. A `standing` obligation re-arms instead of closing.
- The deploy watch is an obligation the daemon opens (decided at CP2, C02 amended by S07): `trigger_kind = event`, `trigger_event` = `{"kind": "deploy.finished", "sha": <pushed sha>, "environment": <deploy_environment or null>}`, `due_at` = push time plus `projects.deploy_timeout_minutes`, `opened_by = daemon`. Section 7 says what meets it.

### Artifact

`artifacts`: pages, screenshots and reports made for the owner, so they stay findable (P50).

- `id`, `card_id`, `run_id`, `kind` (`page`, `screenshot`, `report`, `video`, `other`), `title`, `url` (a link the owner can open), `path` (local, outside the repo), `created_at`, `created_by_session_id`, `retired_at`.

### Review

`reviews`: review coverage and findings per run (plan v2, point 20).

- `id`, `run_id`, `card_id`, `reviewer` (`code-review`, `spec-review`, `no-mistakes`, `human`), `coverage` (`diff`, `spec`, `full`), `findings_high`, `findings_medium`, `findings_low`, `verdict` (`pass`, `fail`, `ask-user`), `recorded_at`, `source_url`.

### Review pass

`review_passes`: one batched review of a project's first-parent commit range, `base_sha..head_sha` (`docs/design/pre-launch-validation.md`).

- `id`, `project_id`, `card_id` (the pass card, kind `scout`, category `pre-launch-review`), `kind` (`weekly`, `final`), `base_sha`, `head_sha` (fixed at start), `chunks` (the JSON plan `tiller review plan` prints), `state` (`pending`, `running`, `finished`, `abandoned`), `report_artifact_id`, `fix_card_ids`, `obligation_id` (the weekly due date that started it), `created_at`, `started_at`, `finished_at`, `rev`.
- At most one pass per project is `pending` or `running` (the `review_passes_live` index).
- The weekly due date is an ordinary `date` obligation with no card, `what = "pre-launch review"`, due Saturday 06:00 Lisbon time.
- Finishing opens one `review` attention item on the pass card; answering it closes the card `reported`. Scheduling and `reviewed_through_sha` never wait for that answer.

### Handoff

`handoffs`: the record a fresh orchestrator resumes from (plan v2, point 34; T4.5).

- `id`, `project_id`, `session_id`, `written_at`, `active_cards` (JSON list of card ids), `pending_attention` (JSON list), `workers` (JSON list of run ids with one line each), `conclusions` (text), `next_action` (text).
- Most of it is a projection of the tables above at write time; `conclusions` and `next_action` are the only new facts.

### Caches owned elsewhere

These tables hold a copy of a fact another system owns. Each row carries `fetched_at`; a stale cache is shown as stale, never trusted as current.

- `panes` (herdr): `pane_id`, `host_id`, `workspace_label`, `agent_name`, `agent_status`, `session_id` (the Tiller session bound to it), `metadata` (JSON row tokens), `fetched_at`, `gone_at`.
- `worktrees` (treehouse and git): `path`, `host_id`, `project_id`, `run_id`, `branch`, `head_sha`, `dirty`, `unlanded_commits` (commits reachable from any local ref that exists only in this worktree and are not landed: not on the default branch, not covered by a merged PR's head, not patch-equivalent by `git cherry`; null when unknown; recovery.md, section 2), `dirty` (1 when `git status --porcelain` lists anything, ignored files excluded), `fetched_at`, `gone_at`.
- `quota_windows` (quota-axi): `label`, `percent_remaining`, `resets_at`, `pace`, `reserve`, `burn_multiple`, `elapsed_percent`, `projected_exhaustion_at`, `title` (the window's display label), `kind` (`weekly`, `model`, `credits`, ...), `fetched_at`, `fetch_error`. `label` is `<provider>:<window id>` (`protocol.md` section 8). No account id, no email, ever.

### Bookkeeping

- `meta`: key-value: `schema_version`, `host_id`, `created_at`, `import_manifest_sha256`, `boundary` (`personal` or `work`; memory.md), `boot_id` (the boot the daemon last reconciled against; recovery.md).
- `idempotency_keys`: `key`, `command`, `entity`, `entity_id`, `response` (JSON), `created_at`. A command that carries a key already present returns the stored response and performs nothing. Keys expire after 30 days.
- `memory_sources`, `memory_revisions`, `memories`, `evidence`, `memory_evidence`, `proposals`, `proposal_evidence`, `session_loads`: owned by `memory.md` (T1.5), which defines them in its section 11.

## 4. Relationships

| From | To | Cardinality | Where it lives |
|---|---|---|---|
| ask | card | many to many | `ask_cards` (`created` or `followup`) |
| card | run | one to many | `runs.card_id`; several roles may be active at once, one per role |
| run | session | one to many | `sessions.run_id`; a resume or relaunch adds a session |
| run | PR | one to many | `prs.run_id`; the newest open one is the run's PR |
| card | PR | one to many | `prs.card_id`, denormalized from the run for PRs found without a run |
| card | dependency | many to many | `card_deps` |
| card | hold | one to at most one open | `holds`, partial unique index on open rows |
| card | attention | one to many | `attention.card_id` |
| run | approval | one to many | `approvals.run_id`; a stale approval stays and a new one is requested |
| run | reservation | one to one while active | `reservations.run_id`, partial unique on open rows |
| session | outbox message | one to many | `outbox.target_session_id` |
| session | observation | one to many | `observations.session_id` |
| run | supervisor session | many to one | `runs.supervisor_session_id`; exactly one at a time |
| project | owner host | many to one | `projects.owner_host_id`; one host per project in v1 |
| project | orchestrator session | one to at most one live | `sessions` where `role = orchestrator` and `ended_at` is null |
| card | artifact, review, obligation | one to many | each table's `card_id` |
| card | later defect | one to many | `cards.fixes_card_id` on the later card |

## 5. Lifecycle facts

The facts that decide what a card is doing, each with its one owner:

| Fact | Owner | Values | Written by |
|---|---|---|---|
| Work state | `cards.state`, `cards.outcome` | `open`; `closed` with an outcome | `card.close`, `card.cancel`, `card.reopen` |
| Dated park | `holds` (open row) | `until_at`, `reason` | `card.hold`, `hold.release` |
| Dependency | `card_deps` (unreleased rows) | the card waited on | `dep.add`, `dep.release` |
| Attention | `attention` (open rows) | kind, question, still current (derived) | `attention.open`, `attention.answer`, `attention.withdraw` |
| Run state | `runs.ended_at`, `runs.outcome`, `runs.pause_until` | active, paused until, ended with an outcome | `run.start`, `run.pause`, `run.end` |
| Agent activity | newest `observations` row per session; cached on `sessions.activity` with `activity_observed_at` | `working`, `idle`, `waiting_permission`, `waiting_input`, `errored`, `ended`, `unknown`; `stale` is derived from age | `observation.record` |
| PR state | `prs` (cache of GitHub) | `open`, `merged`, `closed`, with `checks`, `head_sha`, `pipeline_state` | `pr.refresh` |
| Delivery of a decision | `approvals.state` | requested to succeeded, or stale | `approval.*` |
| Capacity | `reservations` (open rows) | reserved or released | `reservation.reserve`, `reservation.release` |
| Message delivery | `outbox.state` | pending, delivered, acked, failed | `outbox.*` |

Nothing in this table is a column name on the board.

## 6. Column derivation

The board has five columns: Asked, Queued, Working, Landing, Done. A column shows how far the work has come, nothing else, so a card only moves right. A column is computed per card (and per unresolved ask) by the first matching row below. `schema.sql` carries the same logic as the view `card_column`, so the daemon, the CLI and a test all read one definition.

A card that waits on the owner is not a column. It stays where its work has reached, and the view's `needs_you` flag marks it: an open attention item exists for the card, or an active run's session activity is `waiting_permission` or `waiting_input` and not stale. Its badge is then the attention kind, or "permission prompt" kept visually apart from decisions, ahead of every badge in the table. The board's Needs you tab keeps exactly the flagged cards; the `needs` filter of the API and the CLI also keeps the unresolved asks.

| Order | Column | A card is here when | Badge |
|---|---|---|---|
| 1 | Done | `cards.state = closed`. Shown for 3 days after `closed_at`, then only in history | the outcome: landed, reported, answered, completed; or cancelled, failed, abandoned, superseded in their own colours |
| 2 | Landing | `cards.kind = ship` and, since the card was created or last reopened, a PR was linked to it (in any state) or a `merge` or `land_direct` approval was requested for it | merging (a `merge` or `land_direct` approval `accepted`, `executing` or `uncertain`), checks (an open PR with `checks` pending or `pipeline_state` running), deploying (an open deploy-watch obligation), then the active run's activity as in row 3, then parked, blocked, sent back (the owner rejected the merge or direct land, or the merge failed, and no run started since), then checks next (not read yet), checks failed (`checks` failure or `pipeline_state` failed), ready (green, waiting for its merge approval), between runs |
| 3 | Working | a run started on the card since it was created or last reopened | the most urgent activity across the card's active runs: errored, working, paused until a date, idle, stale, unknown (a paused run that is idle shows its pause, not idle); with no active run: parked, blocked, sent back, between runs |
| 4 | Queued | otherwise: nothing has started since the card was created or last reopened | parked with its reason and date; blocked by `<display id>`; filed (waiting for capacity) |
| 5 | Asked | not a card: an ask with `disposition = unresolved`. It should last seconds; one older than a minute shows its age | the orchestrator it waits on |

Notes:

- A card moves forward only. Each column rests on a fact that stays true until the card closes or is reopened: a run started (Working), a PR linked or a merge or direct-land approval asked for (Landing). What changes between the steps of the work shows in the badge, never as a move left: a builder still pushing to its open PR, a run started to fix failed checks, a hold or a dependency on a card that has started, and the owner rejecting a merge approval or the merge failing (sent back, until the next run starts). A ship card whose run ended with no PR, say a relaunch or a builder that exited early, stays Working until its next run. Queued is for a card never started; a hold or a dependency keeps it there. Reopening a closed card starts it over in Queued.
- A flagged card's badge comes from its oldest open attention item.
- A card with an active run whose session is `errored` is Working with an error badge until reconcile ends the run or opens a `recovery` item.
- A closed card with an open attention item cannot exist (the close is refused). A closed card with an open run cannot exist either: `card.close` requires every run ended, except `cancel`, which ends them with `cancelled`.
- `stale` never changes the column by itself. Reconcile decides, by the recovery table, when a stale run becomes a `recovery` attention item.

## 7. Definition of done per card kind

| Kind | Done means | Outcome | Who records it |
|---|---|---|---|
| `ship` | The change is on the project's default branch through the project's delivery mode: an approval of kind `merge` or `land_direct` reached `succeeded` for this card, or reconcile found the PR merged outside Tiller and the owner confirmed through a `recovery` item. For a `direct-push` project with a `deploy_workflow`, the deploy has also been watched to success (below). The worker's own "done" is never enough (P08) | `landed` | the daemon, in the transaction that records the approval result, the confirmed merge, or the deploy result |
| `scout` | A report artifact is recorded on the card, and no attention item is open. A scout that ends with choices files a `report` attention item and the card stays flagged for Needs you until it is answered; then the card closes | `reported` | the orchestrator (`tiller card close --reported`), refused while attention is open |
| `question` | Its attention item is answered | `answered` | the same transaction as `attention.answer` |
| `ops` | `tiller card complete` with an evidence line (a command output, a link). No run is required | `completed` | the orchestrator or the owner |
| any | The owner or orchestrator cancels it; the subject landed elsewhere; every run failed and nobody retries; the worker abandoned it | `cancelled`, `superseded`, `failed`, `abandoned` | an explicit `card.cancel` or `card.close --outcome`, always with a reason; never reconcile alone |

Deploy watch for `direct-push` projects (decided at CP2, C02 amended by S07; the source is `protocol.md` 4.6). When a `land_direct` approval succeeds on a project with `deploy_workflow` set, the card's runs end `landed` (the commit is on the default branch) but the card stays open with a deploy-watch obligation (section 3, Obligation) and shows in Landing with the `deploying` badge. The daemon's forge poller reads the runs of `deploy_workflow` on the default branch and resolves the watch by `deploy.record`:

| Result the poller sees | What the daemon records |
|---|---|
| A run for the pushed SHA (its `head_sha` equals the obligation's `sha`), reaching the obligation's `environment` when one is named, concludes `success` | `deploy.finished` (`success`), `obligation.meet`, `card.close landed` |
| That run concludes `failure` | `deploy.finished` (`failed`), a `recovery` attention item on the card naming the run URL |
| That run is `cancelled` and no later run covers the SHA | `deploy.finished` (`cancelled`), a `recovery` item |
| A later run of the same workflow and environment, for a commit that descends from the pushed SHA, concludes `success` before the pushed SHA's own run does (the pushed SHA's run was skipped, cancelled by concurrency or never queued) | `deploy.finished` (`superseded`, carrying the later run's SHA and URL), `obligation.meet`, `card.close landed` |
| No run for the pushed SHA or a descendant has appeared 10 minutes after the push | `deploy.finished` (`missing`), a `recovery` item; a run that shows up later still resolves the watch by the rows above |
| A run exists but has not concluded by `due_at` | `deploy.finished` (`timed_out`), a `recovery` item; the watch stays open and a later conclusion still resolves it |

The `recovery` item offers the owner "Mark landed" (closes the card `landed` with the reason in the event, for a deploy fixed by hand) and "Reopen work" (cancels the obligation and leaves the card open for a new run). A project with no `deploy_workflow` closes the card `landed` in `approval.finish` as before. PR-mode projects are not watched in v1: their deploy, if any, belongs to the pipeline.

Every close requires: no open attention item, no active run, and (for `landed`) the approval or confirmation above. Runs are cleaned up (worktree returned, pane closed) only after the card is closed or the run has ended with no unlanded commits (P51). Cleanup is a run mutation, not part of close, and refuses while `worktrees.unlanded_commits` is non-zero or unknown.

## 8. Mutations

Every mutation runs in one `BEGIN IMMEDIATE` transaction named after it, checks its preconditions inside the transaction, writes its rows, writes its event rows, and commits. The event's `txn` column carries the name. A mutation that composes others (a spawn) runs them in one transaction and writes one event per sub-step.

`rev` is bumped on every write to a row that has one. Edits name `expected_rev`; a mismatch is refused with `conflict: <entity> <id> is at rev <n>, you sent <m>; <field> is now "<value>"`. Transitions check preconditions instead and refuse with the fact that failed. A transition whose entity is already in the requested state with the same arguments returns that state and writes nothing (`protocol.md` section 6.3).

| Mutation | Caller | Preconditions | Writes | Events |
|---|---|---|---|---|
| `ask.record` | `UserPromptSubmit` hook, transcript tailer (hook-less hosts), `tiller ask note`, board | project exists; idempotency key new | `asks` | `ask.recorded` |
| `ask.resolve` | orchestrator, board (implicit on add), `UserPromptSubmit` hook (a prompt typed in a worker pane is a follow-up on its card) | ask `unresolved`; target card open for `followup` | `asks.disposition`, `ask_cards` | `ask.resolved`, `card.linked` |
| `ask.link` | orchestrator | ask resolved `new` or `followup`; target card open for `followup` | `ask_cards` (and `card.create` for a new card) | `card.linked`, `card.created` for a new card |
| `card.create` | `ask.resolve new`, `ask.link new`, board add, `tiller card create`, import | project not retired; `next_card_seq` read and bumped here | `projects.next_card_seq`, `cards`, `ask_cards` | `card.created` |
| `card.edit` | board, orchestrator | `expected_rev` matches; card open; `dispatch_override` only from the owner (board or `tiller card edit` passing the owner check, `authority.md` section 3) | `cards` fields, `rev` | `card.edited` (fields changed) |
| `card.hold` | orchestrator, board | card open; no open hold; `until_at` in the future | `holds` | `hold.set` |
| `hold.release` | reconcile (due), orchestrator, board | hold open | `holds.released_at`; on `due` also `attention` | `hold.released`, `attention.opened` |
| `dep.add` | orchestrator, board, import | both cards exist; no cycle; dependency open | `card_deps` | `dep.added` |
| `dep.release` | reconcile in `card.close`, orchestrator | dependency unreleased | `card_deps.released_at` | `dep.released` |
| `attention.open` | orchestrator, worker (through its orchestrator), reconcile, daemon | card open; idempotency key new | `attention` | `attention.opened` |
| `attention.answer` | board, `tiller attention answer`, chat intake | item open; key new or identical | `attention` answer fields, `rev`; for a `question` card also `card.close` | `attention.answered`, maybe `card.closed`; `outbox.enqueued` for the owning session |
| `attention.withdraw` | reconcile, orchestrator, owner (board) | item open; reason given; from the board, the item is no longer current | `attention.withdrawn_at` | `attention.withdrawn` |
| `run.start` | `tiller spawn` | card open; no active run of this role; reservation fits host and project capacity; floors and quota pass; key new | `reservations`, `runs`, `sessions` (placeholder until the hook binds the Claude id), `events` | `reservation.reserved`, `run.started`, `session.started` |
| `run.place` | `tiller spawn` (T4.2), as soon as the worktree is leased, again once its base is read, and once the pane exists | run active; caller is the run's supervisor session, the owner, the board or the daemon; a value already recorded never changes; a pane needs the run's live session | `runs.worktree_path`, `branch`, `base_branch`, `base_sha`, `config_hash`; `sessions.pane_id`, `sessions.config_hash` | `run.placed` |
| `session.start` | `tiller orchestrator start` | project not retired; no live orchestrator for the project | `sessions` (role `orchestrator`, no run); `outbox` (one note per unresolved ask of the project) | `session.started`, `outbox.enqueued` |
| `run.bind_session` | `SessionStart` hook | run active, or the session is a live orchestrator session | `sessions.claude_session_id`, `pane_id`; on `clear`, `resume` or `fork` it rebinds to the new Claude id, and the old one goes in the event payload | `session.bound` |
| `session.end` | `SessionEnd` hook (an ending reason), reconcile (pane gone), `tiller agent stop` (`stopped`) and the daemon's stop loop (`run_ended`) when the hook has not ended it first | session live | `sessions.ended_at`, `end_reason` | `session.ended` |
| `run.status` | `tiller status` (or the tailer on a hook-less host) | run active | `runs.last_status_line`, `observations`, `outbox` (one `wake` to the supervisor) | `run.status`, `outbox.enqueued` |
| `run.pause` | worker status line | run active; date parses | `runs.pause_until` | `run.paused` |
| `run.reown` | reconcile | orchestrator session for the project is live and differs | `runs.supervisor_session_id` | `run.reowned` |
| `session.resume` | daemon (reconcile, after a crash or a reboot only, recovery.md R03 and R04); `tiller agent resume` (the owner, or the orchestrator for its own runs) | session dead or host rebooted; worktree or checkout exists; floors pass; rate limit | launches `claude --resume`; on a `SessionStart` with the same id the row continues, with a fresh id the old row is ended in the transaction that inserts the new one | `session.resume_attempted`, then `session.resumed` or `session.ended` plus `session.started` |
| `run.end` | `tiller done`, reconcile, `card.cancel`, `approval.finish` | run active; for `tiller done`: `unlanded_commits` not unknown (P51); outcome `landed` only from `approval.finish` or a confirmed outside merge (recovery.md R07, R13); a builder on a ship card whose PR is unmerged ends `reported` only once its worktree has no uncommitted changes and its head is on an open PR of the card (the PR's head as the forge reports it, or the head the daemon recorded pushing as the run's publish); otherwise it gives `failed` or `abandoned`, or its run ends `landed` with the merge (protocol.md, `tiller done`) | `runs.ended_at`, `outcome`; `reservations.released_at` | `run.ended`, `reservation.released` |
| `run.relaunch` | `tiller agent relaunch` | run active or just ended; worktree exists; note given (P24); the run's live session, if any, has been stopped first (`agent-control`: the stop is proven by the ladder's *dead* verdict or an `exit` the harness confirmed; a session that cannot be proven stopped refuses the relaunch) and its row is ended in this transaction | `runs` (new attempt with `predecessor_run_id`) or `sessions` (new session on the same run); `sessions.ended_at` on the old row; outbox note | `run.relaunched`, `session.ended`, `session.started` |
| `run.clean` | daemon cleanup after `tiller done`, `tiller run clean` | run ended; `worktrees.unlanded_commits = 0` and fresh; pane process not alive or pane closed | `worktrees.gone_at`, `panes.gone_at` | `run.cleaned` |
| `observation.record` | `tiller hook`, tailer, pollers | none (append) | `observations`; `sessions.activity*` when newer | `observation.recorded` (only when the verdict changed; raw rows are not events) |
| `pr.link` | worker (`tiller pr link`), reconcile | run active or card open | `prs` | `pr.linked` |
| `pr.describe` | builder (`tiller pr describe`) | run active; role `builder` | `publishes` (title, body) | `pr.described` |
| `publish.done` | daemon only | a described run; the PR is the recorded one or none is recorded; natural when the record is current | `publishes` (pushed head and time, PR; error cleared) | `publish.done` |
| `publish.failed` | daemon only | a described run | `publishes` (attempted head, error) | `publish.failed` |
| `pr.refresh` | daemon poller | PR exists | `prs` cache columns; `approvals.state = stale` for every `requested` or `accepted` approval on the PR when the head moved or the PR closed or merged | `pr.changed` (only on a change), `approval.stale` |
| `approval.request` | orchestrator, daemon (PR green, no grant) | card open; binding complete; no open approval with the same binding | `approvals` (`requested`), `attention` (`merge`) | `approval.requested`, `attention.opened` |
| `approval.decide` | board, `tiller approve`, grant match | approval `requested`; key new or identical; binding still matches the cache | `approvals` (`accepted` or `rejected`), `attention` answered | `approval.decided` (carrying the cited `ask_id` for a chat decision, G06), `attention.answered` |
| `approval.execute` | daemon only | `accepted`; head re-read from the forge equals `head_sha`; one at a time per PR | `approvals` (`executing`) | `approval.executing` |
| `approval.stale` | daemon only (the land sequence, T4.3) | `requested` or `accepted`; the forge or a grant no longer holds the binding for a reason `pr.refresh` does not see: failed checks, a closed PR, a grant condition that stopped holding, a branch that no longer descends from the default | `approvals` (`stale`), and the memory approvals whose `linked_approval_id` names it | `approval.stale` |
| `approval.finish` | daemon only | `executing`, or `uncertain` resolved by reconcile (recovery.md R08) | `approvals` (`succeeded`, `failed`, `uncertain`); on success `run.end landed` for the card's active runs, then either `card.close landed`, `dep.release` for dependants and `obligations.met`, or, for a `land_direct` on a project with `deploy_workflow`, `obligation.open` for the deploy watch instead of the close (section 7); the memory approvals whose `linked_approval_id` names it finish in the same transaction (`memory.md`, section 7) | `approval.finished`, `run.ended`, then `card.closed`, `dep.released`, `obligation.met`, or `obligation.opened` |
| `deploy.record` | daemon forge poller (T2.5) | an open deploy-watch obligation on the card; the result is new for that obligation (the idempotency key is the obligation id, the run id or none, and the result; `protocol.md` 4.6) | `events`; on `success` or `superseded`: `obligation.meet`, `card.close landed`, `dep.release`; on `failed`, `cancelled`, `missing`, `timed_out`: `attention.open` (`recovery`) | `deploy.finished`, then `obligation.met`, `card.closed`, `dep.released`, or `attention.opened` |
| `grant.create` | owner only (T1.4) | project exists; conditions valid for the kind | `grants` | `grant.created` |
| `grant.revoke` | owner; daemon on expiry or `away.end` | grant not revoked | `grants.revoked_at`, `revoke_reason`; `accepted` approvals from it not yet executing become `stale` | `grant.revoked`, `approval.stale` |
| `away.start` | owner only (T1.4) | no active `away` approval; scope valid | `approvals` (kind `away`, `accepted`, decided by `owner`); temporary `grants` when the scope asks | `approval.decided`, `grant.created` |
| `away.end` | owner; reconcile when `until_at` passes | `away` approval `accepted` | `approvals` (`succeeded`); revokes its temporary grants | `approval.finished`, `grant.revoked` |
| `card.close` | orchestrator, daemon, owner, board | card open; no open attention; no active run; outcome allowed for the kind (section 7) | `cards.state`, `outcome`; releases holds and dependants | `card.closed`, `hold.released`, `dep.released` |
| `card.cancel` | owner, orchestrator, board | card open; reason | ends active runs (`cancelled`), withdraws attention, releases reservations, closes the card | `run.ended`, `attention.withdrawn`, `reservation.released`, `card.closed` |
| `card.reopen` | owner, orchestrator, board | card closed; reason | `cards.state = open`; `outcome`, `closed_at`, `closed_by` null | `card.reopened` |
| `outbox.enqueue` | board, orchestrator, daemon, owner, and inside `ask.record`, `run.status` and `session.start` | target session exists; key new | `outbox` (`pending`) | `outbox.enqueued` |
| `outbox.deliver` | daemon | `pending` or `delivered` (re-ring); pane hosts the target session | `outbox` (`delivered`, `attempts`) | `outbox.delivered` |
| `outbox.ack` | `tiller inbox <id>` in the target session, or its `UserPromptSubmit` hook on the wake-up line | `delivered` (or `pending`, when the session found it by polling) | `outbox` (`acked`) | `outbox.acked` |
| `outbox.fail` | daemon | attempts exhausted or target gone with no successor | `outbox` (`failed`) | `outbox.failed`; a `recovery` attention item when the message was an answer |
| `notification.record` | daemon | attention item open; no `attention.notified` event for it yet | `events` | `attention.notified` |
| `outbox.retarget` | daemon | target session ended; a successor session exists for the same run or project | `outbox.target_session_id`, `target_pane_id` | `outbox.retargeted` |
| `obligation.open` | orchestrator; daemon inside `approval.finish` (the deploy watch) | card open | `obligations` | `obligation.opened` |
| `obligation.cancel` | orchestrator, owner | obligation `open` | `obligations.state` | `obligation.cancelled` |
| `obligation.meet` | reconcile; daemon inside `deploy.record` | trigger event written in this transaction | `obligations.met*`, `outbox` | `obligation.met`, `outbox.enqueued` |
| `artifact.add` | worker, orchestrator | card exists | `artifacts` | `artifact.added` |
| `artifact.retire` | daemon, orchestrator, owner (inside run cleanup, P48) | artifact exists; retiring a retired one changes nothing | `artifacts.retired_at` | `artifact.retired` |
| `artifact.feedback` | daemon (the review page poller, `protocol.md` 3.5) | artifact exists; keyed by the feedback entry | nothing | `artifact.feedback` on the artifact's card |
| `review.record` | worker, daemon | run exists | `reviews` | `review.recorded` |
| `handoff.write` | orchestrator | session live | `handoffs` | `handoff.written` |
| `cache.refresh` | pollers | none | `panes`, `worktrees`, `quota_windows`, `hosts.last_seen_at` | `host.changed`, `pane.changed`, `worktree.changed`, `quota.changed` only on a change |
| `reservation.release` | reconcile | reservation open; its run ended or missing | `reservations.released_at` (`reconciled`) | `reservation.released` |
| `daemon.start`, `daemon.stop` | daemon | single-instance lock held | `meta.boot_id` (written last in a reboot restore); `approvals` `executing` to `uncertain` at start | `daemon.started` (with tool versions), `daemon.stopped`; `host.degraded` and `host.recovered` from the host checks |
| `guard.breach` | reconcile, `tiller land` | a breach detected per authority.md section 11 or recovery.md R19 | nothing but the event | `guard.breached` |
| `project.add`, `project.edit`, `project.retire` | owner | id free (add); `expected_rev` matches (edit); no open card (retire) | `projects` | `project.added`, `project.edited`, `project.retired` |
| `project.delete` | owner, on this host only (`tiller project delete`, board; `Actor::Peer` is refused and it is not in the forwarding allowlist), after the delete has stopped the project's running agents | not `fleet`; this host owns the project; no live run or session and no executing approval among its rows; no row of another project depends on one of its rows, except a link | removes the project row, every row that belongs to it and its events, and link rows to rows that stay; sets nullable references from rows that stay to null (see Deleting a project) | `project.deleted` |
| `project.merge_gate` | daemon only: its ruleset check at `project.add` and hourly (`authority.md`, Enforced merge gate) | project not retired; the value changes | `projects.merge_gate`, `rev`; on a move to `detect`, `attention` (`recovery` on `fleet-1`) | `project.merge_gate_changed`, `host.degraded` and `attention.opened` on a move to `detect` |
| `project.stage` | owner (`tiller project stage`, board) | `expected_rev` matches; not `fleet`, not retired; the same stage is a no-op. To `pre-launch`: a default-branch head is given and no pass is live. | `projects.stage`, `stage_changed_at`, `rev`; to `pre-launch`: `reviewed_through_sha` and the first weekly `date` obligation. To `launched`: open weekly obligations cancelled; a pending weekly pass abandoned (card closed `superseded`) and a pending `final` pass with its card opened, unless a weekly pass is running (its finish opens the final pass) | `project.staged`; `obligation.opened`, `obligation.cancelled`, `review_pass.created`, `card.created`, `card.closed` as they apply |
| `review_pass.start` | orchestrator, owner (`tiller review start`) | a pending final pass, or a pre-launch project with a met weekly obligation no pass started from; a running pass with the same head and plan is a no-op | `review_passes` (`running`, `head_sha`, `chunks`, `started_at`); for a weekly pass, its card and row first | `review_pass.started`; `card.created`, `review_pass.created` for a weekly pass |
| `review_pass.finish` | orchestrator, owner (`tiller review finish`) | pass `running`; for each chunk `n`, a live `report` artifact titled `chunk <n> of <total>` on the pass card from a run that recorded a `reviews` row there, both since start; a live `page` artifact on the card; fix cards in the project; `reviewed_through_sha` still equals `base_sha` | `projects.reviewed_through_sha` (compare-and-set to `head_sha`), `review_passes` (`finished`), one `review` attention item; the next weekly obligation while pre-launch; the final pass for a weekly pass that finishes after launch | `project.reviewed`, `review_pass.finished`, `attention.opened`, `obligation.opened` or `review_pass.created` |
| `review_pass.abandon` | orchestrator, owner (`tiller review abandon`) | pass `pending` or `running`; a reason | `review_passes` (`abandoned`), the card closed `abandoned`; the next weekly obligation while pre-launch, or a new pending final pass | `review_pass.abandoned`, `card.closed`, `obligation.opened` or `review_pass.created` |
| `host.add`, `host.edit` | owner | label free (add); `expected_rev` matches (edit) | `hosts` | `host.added`, `host.edited` |
| `peer.accept` | owner, on the peer, through `tiller peer accept` run by `tiller peer add` over `ssh -t` (`authority.md` section 3) | config `federation.accept` is not `false` (also checked on every hello, `mutate` and `query`: `protocol.md` 10.2); the public key parses; the viewer is not this host | `peer_keys` (and `revoked_at` on an older row for the same viewer) | `peer.accepted`, maybe `peer.revoked` |
| `peer.revoke` | owner, on the peer (`tiller peer revoke <viewer label>`) | key not revoked | `peer_keys.revoked_at`; an open link from that key is closed at once | `peer.revoked` |
| `peer.pair` | owner, on the board host, inside `tiller peer add`, after the peer's `peer.accept` succeeded | label free, or a row with that label whose `peer_scope` is null (never paired, or removed) or whose `peer_host_id` is the same; the key file was written first | `hosts` (`kind = remote`, `herdr_machine`, `peer_scope`, `peer_host_id`) | `host.added` or `host.edited` |
| `peer.remove` | owner, on the board host (`tiller peer remove <label>`) | `peer_scope` set | `hosts.peer_scope` and `peer_host_id` null (the row stays as a herdr machine); the key file is deleted after the commit | `host.edited` |
| `config.set` | owner | key known; value valid | the config file, outside `state.db` | `config.changed` |
| `health.relabel` | owner, on this host only (`Actor::Peer` is refused; it is not in the forwarding allowlist) | the intervention exists; a label equal to the current one writes nothing | `intervention_labels` (set, or deleted when the label is cleared) | `intervention.relabeled` (`from`, `to`, `classified`, `cleared`) |
| `import.apply` | `tiller import` | manifest valid; `meta.import_manifest_sha256` differs (a second import is a no-op) | every table, with `legacy_id` set | one `import.applied` plus the per-row events, all under `actor = import` |

### Deleting a project

`project.delete` is the hard delete. It is for a project added by mistake or one the owner wants gone, and it is the only mutation that removes history. A project that is merely finished is retired instead.

- **What goes.** The project row and every row that belongs to it. A row belongs to the project when a `NOT NULL` foreign key of it points at a row that does: its cards, asks, runs, sessions, attention, approvals, artifacts, memories and their revisions, proposals, evidence, obligations, review passes and the rest, followed transitively. Observations and memory evidence go with their sessions and runs, and cached worktree rows go with the project. The rows to remove are found from the schema's foreign keys, not from a table list, so a new table is covered the day its migration lands; a nullable or unchecked reference must be sorted as owning, pointing or kept before the build passes (`core::state::deletion::every_reference_is_classified`).
- **Attention about the project.** An open attention item on a card that stays, whose `subject_project_id` is the project or whose `subject_session_id` is one of its sessions, is withdrawn in the same transaction with the reason `project <id> was deleted`, in any project including `fleet`. Items on the project's own cards go with those cards. The count is in the delete result (`withdrawn_attention`), in the CLI confirmation and in the `project.deleted` tombstone.
- **The audit trail.** The events about the project, its cards and every removed row, and the idempotency keys and intervention labels that name them, are removed in the same transaction. This is the one exception to the append-only events table (section 3, Event). What remains is one `project.deleted` event, the tombstone: its payload holds the project's name, its `next_card_seq`, and the rows removed and unlinked per table. Tombstones of earlier deletes of the same id stay.
- **What stays.** A row of another project or of `fleet` that only points at a removed row keeps its place with that column set to null (for example `cards.fixes_card_id`, or `memories.authored_by_session_id` on a host-wide memory a removed session wrote). A link row that ties a removed row to one that stays is removed: a dependency between one of the project's cards and another project's card (`card_deps`, either way), an ask linked to another project's card (`ask_cards`), and the evidence, session-load and change-set links of host-wide memories and proposals. Both kinds are counted per `table.column` as `unlinked`, so the owner sees them before confirming. `memories.proposal_id` keeps naming a removed proposal, because the memory's own revisions carry its text. Nothing outside `state.db` is touched: the checkout, its worktrees, the forge repository and `checkouts.<id>` in `config.toml` stay as they are, and the CLI says that the key remains.
- **Running agents.** The owner's delete stops the project's running agents first, the orchestrator first so it starts nothing new, then each worker, with the proven stop `tiller agent stop` uses (`protocol.md` section 6): each session ends `stopped` and its pane closes, and each card a stopped worker's run was on is cancelled with the reason `deleting project <id>`. Only a stop that fails refuses the delete, naming the agent; nothing is removed then, and the agents stopped before it stay stopped. An agent another host runs refuses before anything is stopped. The delete itself runs after the stops, in its own transaction. `project.retire` stops them the same way when the owner asks; otherwise they keep running.
- **Work not landed.** The delete never removes a worktree, a branch or a PR. The confirmation lists the worktrees with commits not pushed or uncommitted changes and the open PRs, so nothing goes unsaid, and the owner still types the id.
- **Preconditions.** Nothing may be in flight when the delete transaction runs: a live run, a live session (the orchestrator included) or an executing approval refuses it with the one it found. After the stops above, only an executing approval or a session that started since is left to refuse it. Any other row of another project that requires one of the project's rows (a `NOT NULL` reference across projects that is not a link) refuses it too, naming the table. `fleet` and a project another host owns are refused before any row is counted.
- **Confirmation.** The CLI refuses without `--yes` and prints the counts it would remove, the agents it would stop and the work it leaves; on a terminal it may instead ask the owner to type the id. The board shows the same (`GET /api/v1/projects/{id}/delete`) and sends the typed id as `confirm` (`protocol.md` section 7.4).
- **The id afterwards.** `project.add` may take the id again. A run branch is named after its card (`tiller/<project>-<n>-<role>`), so the add first looks for the deleted project's run branches: the local and remote-tracking branches of the configured checkout (`checkouts.<id>`) and the branches on its `origin`, or, with no checkout configured, the branches on the project's remote (`remote`, else `https://github.com/<repo>.git`).
  - No run branch is left: the cards start at `<project>-1`.
  - A run branch is left, or the branches cannot be listed: the cards number on from the tombstone's `next_card_seq`. The add applies and its response carries a `warning` that gives the first display id and names the branches, or the reason they could not be listed. The board's add dialog shows the warning. To start at 1, delete the branches, then delete and add the project again.

  An import for that id always numbers on from the tombstone (section 2, Display ids). Test: `deletion::tests` in `tiller-core` and `numbering::tests` in `tiller-agents`.

Every row in the events table names its mutation, so a timeline query answers "who changed this, under which transaction, with which preconditions".

A forwarded mutation (ADR 0003) is not a new mutation. The peer runs the same row of this table that its own board would run, with the same preconditions, and writes the event with `actor = board@<viewer label>` and `payload.via_peer = {viewer_host_id, key_id}`.

## 9. Fact ownership

The table from plan v2, expanded to every table in `schema.sql`.

| Fact | Owner (authoritative) | Projections and caches |
|---|---|---|
| Asks, cards, runs, sessions, links, dispositions, holds, dependencies | `state.db`: `asks`, `cards`, `runs`, `sessions`, `ask_cards`, `holds`, `card_deps` | the board, `tiller` output, the JSONL export, the `card_column` view |
| Attention and answers | `state.db`: `attention` | the inbox, receipts |
| Approvals and their outcomes | `state.db`: `approvals`, `grants`; bound to a SHA or a diff revision | board receipts, the Safety signal |
| Capacity | `state.db`: `reservations`, with `hosts.lane_capacity` and `projects.capacity` | the Machines section |
| Delivery of messages | `state.db`: `outbox` | receipts (recorded, delivered, done) |
| Obligations, artifacts, reviews, handoffs | `state.db`: their tables | card detail, Health |
| Events | `state.db`: `events` | SSE, timelines, Health signals, the export |
| Agent activity | nobody: it is observed. Newest `observations` row per session, with `observed_at` | `sessions.activity*` (same-transaction cache), the activity badge, the permission-prompt section |
| PR state | GitHub, through `gh-axi`; `prs` is a cache with `fetched_at` | the Landing column, the PR chip |
| Pipeline state | `no-mistakes` records; `prs.pipeline_state` is a cache | the Landing badge |
| Deploy results | GitHub workflow runs, through `gh-axi`; no cache table, each result is a `deploy.finished` event and the open watch is an obligation | the Landing `deploying` badge, card detail |
| Panes and sessions in herdr | herdr; `panes` is a cache with `fetched_at` | jump links, the Agents view |
| A peer's cards, attention, agents and summary | the peer's own `state.db` | the board host's in-memory peer cache, never stored here; labelled with the host and its link state (`protocol.md` section 10) |
| Who may read or act on this host from another host | `state.db`: `peer_keys` | the peer link's hello and every forwarded mutation |
| This host's right to read or act on a peer | the key file `~/.tiller/peers/<label>.key`, with `hosts.peer_scope` | the Machines row |
| Worktrees | treehouse plus git; `worktrees` is a cache | reconcile input, the cleanup guard |
| Quota | `quota-axi`; `quota_windows` is a cache | the sidebar footer, Health, the spawn check |
| Owner marker | the file `~/.tiller/owner` | `tiller` refuses mutations when it names another system |
| Approved memory and rules | canonical files under git (CLAUDE.md, AGENTS.md, memory dirs, the dotfiles source) | the memory view (T1.5) |
| Unapproved evidence and proposals | `state.db`: `evidence`, `proposals` (T1.5); never loaded | the proposal view |
| Raw transcripts | Claude Code's own JSONL files; read only, never committed | the tailer, retros, the baseline |
| Board column, staleness, still-current, free slots | nobody: derived by query (`card_column`, `session_activity`) | the board |

## 10. Durability

| Class | Records | Policy |
|---|---|---|
| Authoritative | `state.db` tables in section 9's first seven rows; the memory files under git | WAL mode, `synchronous = NORMAL` (WAL makes a committed transaction durable against a process crash; an OS crash can lose the last transactions but never corrupt), nightly backup through SQLite's online backup API inside the host's boundary (T2.3), versioned migrations with a down path, a restore drill in CI |
| Rebuildable | `observations`, `events`-derived projections, every cache table, `sessions.activity*` | kept, pruned by retention, rebuilt from their source when missing; an `exit_sent` observation is pruned only once its session has ended, since it is all that keeps the stop loop from typing a second `/exit` |
| Disposable | logs, the daemon's in-memory SSE buffer, the in-memory peer cache | not backed up; the peer cache is refilled from the peer's snapshot on reconnect |
| Credential | `~/.tiller/peers/<label>.key` | not backed up; a lost key is replaced by running `tiller peer add` again, which revokes the old one on the peer |

Schema version lives in `meta.schema_version` and is managed by `rusqlite_migration`. Every migration has an up and a down, and T2.1 runs both on a fixture.

## 11. Invariants

Tests in T2.1 and T2.7 prove each one.

1. One owner per fact: no column in `schema.sql` duplicates another table's fact except the labelled caches (`sessions.activity*`, `prs.*`, `panes`, `worktrees`, `quota_windows`, `hosts.last_seen_at`) and denormalized `card_id` keys.
2. No stored column: `cards` has no column named for a board column, and the only definition of a column is the `card_column` view.
3. Every mutation has an event: a transaction that changes a row in an authoritative table and commits without an `events` row with that `txn` fails the property test `core::events::every_mutation_writes_its_event`. Events are removed only by `project.delete`, and only the deleted project's.
4. A closed card has no open attention and no active run.
5. At most one open hold per card; at most one active run per card and role; at most one live session per run; at most one open reservation per run; at most one live orchestrator session per project.
6. Two concurrent `run.start` calls on a full host or project cannot both succeed.
7. An `accepted` approval whose PR head differs from `head_sha` is `stale` before any execution starts.
8. An outbox message in `acked` has a `delivered_at` or a recorded poll pickup; a `failed` answer always has a `recovery` attention item.
9. Display ids are unique per host. A display id is never reused across import, nor across a deleted project's id being added again while one of its run branches is left.
10. A mutation carrying a known idempotency key returns the stored response and writes nothing.
11. No row in `state.db` belongs to a project another host owns: every `cards.project_id` names a project whose `owner_host_id = meta.host_id` (ADR 0003 keeps principle 1).
12. Every event with `actor = board@<label>` carries `payload.via_peer.key_id`, which names an `act` row of `peer_keys` that was unrevoked when the event was written.

## 12. Coverage

### Second opinion points this spec covers

| Point | How |
|---|---|
| 6 | One authority per host, the fact-ownership table, every projection labelled |
| 7 | `BEGIN IMMEDIATE`, `rev` on editable records, readable conflicts, event in the same transaction |
| 8 | Durability classes, backup, migrations, the restore drill as a test |
| 9 | Lifecycle facts stored separately; the precedence table; done per card kind; cancelled, failed, abandoned, superseded as outcomes |
| 10 | ULIDs behind display ids; asks to cards many to many; card to runs; run to sessions and PRs; owner host per project |
| 11 | Observations with `observed_at`; `unknown` and `stale` explicit; verdict per event type (mapping in T1.3) |
| 12 | `asks.disposition` and `ask_cards` recorded by a structured command; an unresolved ask stays visible in Asked |
| 13 | The outbox with four states, idempotent retries, target-session check, board adds that cannot double-create |
| 14 | The tables reconcile reads and the `recovery` attention kind; the rows themselves are T1.2 |
| 15 | The owner marker and `meta.host_id` |
| 16 | Approvals bound to repo, PR, head SHA and action, or diff and base revision; states requested to uncertain; stale on a moved head; idempotent clicks |
| 17 | `approval.execute` and `approval.finish` are daemon-only; the roles behind that are T1.4 |
| 20 | `category`, `size`, `acceptance`, `fixes_card_id`, every attempt as a run, `reviews` |
| 22 | `config_hash` on sessions and runs |
| 24 | `observations` is never loaded; `evidence` and `proposals` reserved for T1.5 |
| 34 | `handoffs` |
| 35 | `reservations` inside the spawn transaction; quota cached per account window; stale quota visible |
| 36 | `attention` carries question, evidence, recommendation, consequence; still-current derived; permission prompts kept apart; receipts from outbox and approval states |
| 38 | Stable ids and idempotency keys as records |

Points 1 to 5, 18, 19, 21, 23, 25 to 33, 37, 39 to 43 are owned by other specs or the build plan.

### Firstmate contracts matched or departed from

| Contract | Matches | Departs, and why |
|---|---|---|
| `captain-hold-lifecycle` | Cleanup never closes an open call; an answer records the exact words and resolves in one act; an exact replay is idempotent and a drifted answer is refused; a dated deferral re-asks on its date; a landed subject retires its decision (withdraw); a question whose state cannot be established is kept | A decision is an `attention` row on a card, not the card itself. A ship card can carry several decisions over its life without changing kind, each one auditable; a `question` card is the case where the answer is the whole work. Snapshot buckets (`blocked`, `dated`, `aged`, `live`) become the derived column plus age |
| `fleet-ledger` | The five event names map one to one onto event kinds; readers ignore unknown fields | Events have a sequence number, are written in the mutation's transaction, and are never duplicated, so a reader does not need to tolerate repeats |
| `supervision-host` | One supervisor per run (`runs.supervisor_session_id`); a captain outcome is presented until acknowledged (`attention` stays open until answered); a dated pause silences alarms | The wake path is the outbox, with acknowledgement as a state, not a drain cursor; routine outcomes are events, not notes |
| `agent-control` | A relaunch is transactional, keeps the worktree, and requires a note for a worker; a harness or model change is an ordinary relaunch | The journal file is replaced by the transaction and the `run.relaunched` event; Claude Code is the only harness with an adapter at cutover; `sessions.harness` keeps room for others (D04) |
| `herdr-backend` | Pane identity lives in metadata, not labels; status is cached with a fetch time | Panes are a cache table, not a backend record; the layout rules are in `herdr-layout.md` |

### Parity rows this spec gives a home

P07 (scout to ship as a new run on the same card and worktree), P10 (project delivery mode), P15 (obligations), P19 and P30 (holds with `until_at`, `runs.pause_until`), P21 (`supervisor_session_id`), P29 and P31 (attention), P32 (dependencies), P44 (events), P50 (artifacts). The rest are tested by the tasks the matrix names.

## Decided at CP2

Monday 2026-11-02, Lisbon time. The owner's picks from `cp2-open.md`:

1. **`question` stays a card kind** (C09), so a bare question has a display id to tag in chat (section 3, Card).
2. **Staleness thresholds and the idle re-ring interval** (C10): accepted as `recovery.md` proposes them; this spec still only derives them from `observed_at`.
3. **Capacity and floors live in `state.db`** (C11, reason corrected by S09): the capacity check runs inside `run.start`'s transaction against `hosts`, `projects` and open reservations, and the `capacity` view reads the same rows (section 3, Host).
4. **Done for `direct-push` projects** (C02 amended by S07): the deploy watch matches the pushed commit and the project's environment, with missing, cancelled, superseded and timed-out results defined (section 7); `projects.deploy_workflow`, `deploy_environment` and `deploy_timeout_minutes` hold what it matches.
5. **Dispatch override** (C04): `cards.dispatch_override`, owner-only (section 3, Card; `protocol.md` 6.4).
6. **Enforced merge gate** (S03): `projects.merge_gate` records whether the daemon's ruleset check holds (`authority.md`, section 6).
7. **Combined memory approval** (S01): `approvals.memory_paths` and `linked_approval_id` (section 3, Approval).

## Accepted after CP2: ADR 0003

Tuesday 2026-11-03, Lisbon time, accepted by the owner. The owner asked for one board across hosts, which reopens C12 (`cp2-open.md` item 12).

1. **Principle 1 kept, worded for federation.** One authority per host and one owner host per project stay. A peer's cards are a disposable in-memory cache on the board host, never rows (section 1).
2. **Pairing records.** `hosts.peer_scope` and `peer_host_id` on the board host; `peer_keys` on the peer; `peer.accept`, `peer.revoke`, `peer.pair` and `peer.remove` (sections 3 and 8). `schema.sql` carries the draft DDL; T2.1's migration path adds them in a new migration with a down step (task F2).
3. **Forwarded mutations reuse section 8.** The actor is `board@<viewer label>`; invariants 11 and 12 pin both rules.
