# Protocol and CLI

This is the contract for every way in and out of `state.db`: how an ask becomes a record, how a message reaches a session, how each hook event becomes an observation, the full `tiller` command surface, the board's HTTP and SSE API, the quota feed and the machine summary. `state.md` and `schema.sql` define the records; this spec defines who writes them, through which command or endpoint, and what a caller gets back.

Written Monday 2026-11-02, Lisbon time (WET). Task T1.3. Status: approved at CP2 (Monday 2026-11-02), with the owner's decisions from `cp2-open.md` applied. A contract change from here on needs a stop-and-ask. Amended by ADR 0003 (federated board), accepted Tuesday 2026-11-03: sections 6.2, 6.4, 7, 9, 10, 11 and 12.

T2.2 builds the CLI from section 6, T2.6 builds the hook and transcript handlers from sections 4 and 5, T2.5 builds delivery from section 3, T2.8 builds the API and the feeds from sections 7 to 9, and T4.4 wires intake from section 2. Authority (who may run what) is T1.4; this spec names the caller each command expects and leaves the deny rules to `authority.md`.

Claims about Claude Code are tagged: [D] documented at code.claude.com/docs/en/hooks (read Monday 2026-11-02), [T] tested on host A or B, [I] inferred and pinned by a named test before it is relied on.

## 1. Principles

1. **One writer path.** Every mutation in `state.md` section 8 is a function in `crates/core`. The CLI, the hooks, the daemon and the board API all call the same functions, so a board click and a CLI command cannot differ. The board API never writes SQL of its own.
2. **The CLI does not need the daemon.** `tiller` opens `state.db` itself (WAL, `busy_timeout` 5 s) and commits. The daemon notices new rows by polling `PRAGMA data_version` every 200 ms and pushes them over SSE. Only actions that touch the outside world on a timer or after an approval (delivery, landing, polling) need the daemon.
3. **Hooks are observations, not state** (second opinion point 11). A hook records what it saw, with `observed_at`. It never closes a card, ends a run or answers a question. Reconcile (T1.2) turns conflicting evidence into attention, never into a destructive action.
4. **Hooks fail open.** A hook that cannot write (database locked past its budget, schema mismatch, a crash) logs to `~/.tiller/log/hook.log`, exits 0 and lets the session continue. The only hooks that may change what Claude does are the PreToolUse guard (T1.4) and the Stop hook in `block-once` mode (section 2.5).
5. **AXI style.** Structured output, stable ids, documented exit codes, idempotency keys and safe retries (point 38). No command calls a model.
6. **No secrets, no account identity.** No command, response, event or log line carries an email, an account id or a token. The quota feed parses an allowlist of fields (section 8).

## 2. Ask intake

### 2.1 What is an ask

Every message the owner sends an orchestrator, and every note a script drops, is an `asks` row before the agent reads it (P27, P60). Wake-up lines (section 3) are not asks. An ask ends with exactly one disposition: `new`, `followup`, `chat` or `cancelled`. Until then it is `unresolved` and sits in the Asked column.

### 2.2 Sources

| Source | Trigger | Mutation | `asks.source` | Idempotency key |
|---|---|---|---|---|
| Owner types in an orchestrator pane | `UserPromptSubmit` hook (`tiller hook UserPromptSubmit`) | `ask.record` | `prompt` | `prompt:<claude session id>:<prompt_id>` (`prompt_id` is a common hook field [D]); when absent, `prompt:<claude session id>:<sha256 of prompt text>:<transcript byte length>` |
| Owner types in a worker pane | the same hook, in a worker session | `ask.record`, `ask.resolve followup:<run's card>` and `outbox.enqueue` (`note`) to the run's supervisor, in one transaction | `prompt` | as above |
| Hooks blocked on the host (reduced behavior) | the transcript tailer sees a user entry in an orchestrator transcript | `ask.record` | `prompt` | `transcript:<claude session id>:<entry uuid>` |
| A script or the owner from a shell | `tiller ask note <project> [--text <t> \| -]` | `ask.record` and `outbox.enqueue` (`note`) to the project's live orchestrator, in one transaction | `script` | `--key`, or a minted one (section 6.3) |
| Board "add card" | `POST /api/v1/cards` | `ask.record`, `ask.resolve new`, `card.create` and `outbox.enqueue` (`note`) in one transaction | `board` | the click's key |
| Import | `tiller import` | `import.apply` | `import` | the manifest hash |

The ask and its wake are one transaction, so a crash cannot leave a recorded ask that nobody is told about. When the project has no live orchestrator, the ask is recorded without the message, the response carries `warning: no_orchestrator`, and `session.start` enqueues one note for every unresolved ask of the project when the orchestrator starts.

On a host where hooks fire, the tailer never records asks, so one prompt cannot become two asks. Which source a host uses is the host config value `intake_source` (`hook` or `transcript`), set from the probe result (host-capabilities, "Hooks blocked by managed policy"). On a `transcript` host no hook tells the orchestrator the ask id, so the orchestrator role (T4.1) runs `tiller ask list --unresolved` at the start of each turn there.

### 2.3 What the hook tells the orchestrator

For an ask, the `UserPromptSubmit` hook returns one line of `additionalContext` [D]:

```
tiller: ask 01JC2Z8Q4M7V3R5T9W1X6Y0B2D (short X6Y0B2D) recorded. Before this turn ends run: tiller ask resolve X6Y0B2D new|followup:<card>|chat|cancelled. Start each line about a card with its tag, like [alpha-42].
```

It prints the full ULID once and the short form the CLI accepts (section 6.1). The orchestrator role (T4.1) carries the judgement; the hook carries only the mechanics.

### 2.4 `tiller ask resolve`

```
tiller ask resolve <ask> new --kind ship|scout|question|ops --title <t> [--priority 0-4] [--body <t> | --body-file <f> | -] [--size S|M|L] [--acceptance <t>] [--category <c>]
tiller ask resolve <ask> followup:<card>
tiller ask resolve <ask> chat
tiller ask resolve <ask> cancelled [--reason <t>]
tiller ask link <ask> new ... | followup:<card>
```

- `new` runs `ask.resolve` and `card.create` in one transaction. The response carries the card's ULID, display id and the tag to use (`[alpha-42]`).
- `chat` and `cancelled` create no card; a `--reason` goes in the `ask.resolved` event payload.
- `followup:<card>` takes a display id, a ULID or a short form. The card must be open; a follow-up on a closed card is refused with the card's outcome, and the orchestrator reopens it (`tiller card reopen`) or files a new card.
- One ask that creates or touches several cards: resolve once, then `tiller ask link` for each further card. `ask link` (`ask.link`) requires the ask to be resolved `new` or `followup` and adds an `ask_cards` row (and a card for `new`) in its own transaction.
- Retry: resolving an ask again with the same target returns the first response and writes nothing (key `ask.resolve:<ask id>`). Resolving it with a different target is refused (exit 5) with the current disposition. A resolved ask is never re-resolved; a wrong disposition is fixed by `ask link` or by editing the card.
- An ask that is never resolved stays visible in Asked with its age (point 12). Nothing resolves an ask automatically except the board's add card, which records and resolves in one transaction.

### 2.5 The Stop hook: observe mode and block-once

The Stop hook runs on every turn end in every Tiller session [D: it does not run on a user interrupt, and API errors fire `StopFailure` instead]. In an orchestrator session it also checks the intake protocol:

1. **Unresolved asks:** asks recorded for this session that are still `unresolved`.
2. **Unknown tags:** every `[<project>-<n>]` token at the start of a line in `last_assistant_message` [D] that names no card on this host. `[chat]` is always valid.

It writes one observation (`event = Stop`, `verdict = idle`) whose payload holds `unresolved_ask_ids`, `unknown_tags`, `tags` (valid card ids mentioned) and `would_block` (true when either list is non-empty). It never copies the reply into a card: the card timeline shows "mentioned in a reply at <time>" from the observation, which is enough to de-interleave without mixing discussions (point 12).

Modes, from the host config value `stop_hook_mode`:

| Mode | Behavior |
|---|---|
| `observe` (the default, and the only mode at cutover) | Record the observation. Never block. Exit 0 with no output |
| `block-once` | When `would_block` is true and the input's `stop_hook_active` is false [D], print `{"decision":"block","reason":"<one line naming each unresolved ask and unknown tag>"}`. When `stop_hook_active` is true, never block, so one turn is blocked at most once. Workers are never blocked by this check |
| `off` | Record only the turn end, no intake check |

**The switching rule.** Observe mode measures what blocking would have done. A `would_block` stop is a *false block* when, within 2 minutes and without another owner prompt, the same session resolves every listed ask and every unknown tag turns out to be a card created in that window. `tiller intake stats` reports the stop count, the would-block count and the false-block rate over a window. The owner may switch to `block-once` only when the last 7 days hold at least 200 orchestrator stops and the false-block rate is under 2% (numbers accepted at CP2, C14). The switch is a config change the owner makes (`tiller config set stop_hook_mode block-once`), never automatic, and it is recorded as an event (`config.changed`). `config set` refuses the switch to `block-once` (exit 5, with the current stats) while the rule is not met. Switching back to `observe` is always allowed.

The turn-end guard Firstmate keeps (P26, never go idle with work in flight) is not a block here: the daemon delivers pending outbox messages to an idle orchestrator (section 3), which wakes it.

## 3. Wake-up lines and the inbox

### 3.1 Format

The outbox (`state.md`, Outbox) stores every message first. The pane only ever receives one wake-up line, typed by the daemon through herdr and followed by Enter:

```
tiller inbox <ULID>[ # <kind>[ <display id>]]
```

for example `tiller inbox 01JC2ZB6N0E2K4M6P8R0S2T4V6 # steer alpha-42`. The comment is for the person watching the pane; it carries no data. The grammar, as a regex on the whole submitted prompt with surrounding whitespace trimmed:

```
^tiller inbox [0-9A-HJKMNP-TV-Z]{26}( # (wake|steer|answer|note)( [a-z][a-z0-9]*(-[a-z][a-z0-9]*)*-[0-9]+)?)?$
```

The project part of the display id is also 2 to 32 characters (`state.md` section 2).

### 3.2 How hooks recognize it

The `UserPromptSubmit` hook matches the prompt against the grammar before anything else:

- **A match is never an ask.** The hook looks up the outbox row. If the row's `target_session_id` is this session, it runs `outbox.ack` (actor `hook`) and returns the message body, plus the body of every other `pending` or `delivered` message for the same session (each acked in the same transaction, oldest first), as `additionalContext`. The agent reads its messages without a tool call.
- **A match for another session** (a stale pane, a retargeted message): the hook does not ack. It records an observation `wake_mismatch` and returns one context line telling the agent to ignore the line. Reconcile retargets the message (`state.md`, Outbox rules).
- **Edge cases.** An id with no outbox row, or a session the hook cannot map, is treated as a mismatch: no ack, one context line. A row already `acked` returns its body again and writes nothing. A row in `failed` returns its body with a note that it was given up on, and writes nothing; reconcile owns it.
- **No hooks on the host:** the agent runs the typed line as a command. `tiller inbox <id>` performs the same ack and prints the same bodies. The role files tell every agent to run a line of that shape when it sees one.
- The transcript tailer treats a matching user entry the same way it treats a hook match: it records the wake, it never records an ask.

Because the line is recognized, a board-created card (already recorded and resolved) is never created a second time by the orchestrator reading its wake-up line (point 13; T4.4 acceptance).

### 3.3 Delivery rules

T2.5 implements these; they are listed here because they are part of the contract a session sees.

- **Store, then type.** `outbox.enqueue` commits before any keystroke. A crash between store and send leaves a `pending` row that the next delivery pass sends.
- **One line per batch.** When several messages are pending for one session, the daemon types one line naming the oldest; the ack returns them all. A worker's done, blocked or decision therefore reaches the orchestrator once, without flooding the dialog (P20).
- **Target check.** Before typing, the daemon checks that the pane's herdr metadata names the target session. A mismatch retargets or fails the message; it never types into the wrong session.
- **Empty composer.** It also reads the pane's composer (`herdr pane read`, the full visible viewport) and types only on a positive `empty` verdict, as Firstmate's herdr backend does. A composer holding text (the owner's half-typed draft) or one that cannot be read defers delivery (`next_attempt_at`, 30 s); the daemon never clears it, since the text may be the owner's. A composer holding exactly the wake-up line for one of the session's unacked messages (however its rows wrapped) is that line typed but not submitted, and is finished with Enter; a draft that merely starts with one is the owner's. A bare shell prompt is never an empty composer: it means the agent is gone, and the message waits for a successor (recovery.md, R09). The line is typed once; only Enter is retried.
- **Verified submit.** Claude Code can read an Enter that arrives right after typed text as part of it, adding a newline instead of submitting; a Claude just relaunched in the same pane does this every time. So the daemon waits half a second between the line and Enter, then reads the composer back and presses Enter again while the line is still there, up to four presses a pass. A message is `delivered` only on evidence the line left: the composer reads empty, busy or behind a dialog, or the hook acked the batch. Other text or an unreadable screen is read again, never pressed. A line not shown to have left stays `pending`, deferred 30 s, and the next pass finishes it with Enter. After three such passes the line is left like the owner's text and reported after 10 minutes.
- **Busy panes wait.** A session whose effective activity is `working` or `waiting_permission`, or whose pane shows a dialog, defers delivery (`next_attempt_at`).
- **Re-ring.** A `delivered` message not acked after the idle threshold, with the session observed `idle`, is typed again (P23). The threshold is T1.2's.
- **Polling pickup.** `tiller inbox` with no id lists and acks every pending or delivered message for the calling session. It is how a session drains its inbox when it suspects a missed doorbell.

### 3.4 Notifications (P36)

The daemon sends the owner a desktop notification through herdr (`herdr notify`, with `--machine` for a remote agent) only when something new needs them, once per cause, never for a routine turn:

| Cause | Once per |
|---|---|
| An attention item opens (`attention.opened`), including a `merge` item for a PR ready for the owner's word | item |
| A session has been in `waiting_permission` or `waiting_input` for 30 minutes (recovery.md, thresholds) | session, per prompt |
| A reboot restore finished (recovery.md, R04) | boot |
| The daemon entered read-only mode (recovery.md, R10, R11) | episode |

The notification names the project and the card, and opening it runs `herdr agent focus` on the project's orchestrator (the saved machine first for a remote host). A re-ring, a worker's status line, a PR event or a card moving between columns never notifies. Each notification is recorded in the event that caused it (`notified_at` in the payload), so a daemon restart cannot send it twice. Test: `delivery::notify::new_attention_item_notifies_once`.

### 3.5 Review-page answers (P47)

Decided at CP2 (C05): in v1. A worker that publishes a live review page records it with `artifact.add` (`state.md`, Artifact), and the owner's annotations on that page reach the worker through the outbox, not through the worker polling the page.

- **Source.** A T2.5 poller reads the review page tool's feedback queue for every artifact that has a live page and an open card. It runs on the daemon's schedule (every 30 s while the artifact's run is active) and records its last success in `/health` like every poller.
- **Delivery.** Each new feedback entry becomes one `outbox.enqueue` of kind `steer` to the live session of the run that published the artifact (`artifacts.run_id`), with actor `daemon` (the role matrix in `authority.md` gives the daemon the steer right and gives scripts none). The message body is the entry's text and the element it annotates, never the page itself. The ordinary delivery rules apply (3.3): store, then type; one line per batch; busy panes wait.
- **Idempotency.** The key is `page:<artifact id>:<feedback entry id>`, so a re-poll, a daemon restart or a second poller pass never delivers an entry twice.
- **No owning session.** When the publishing run has ended, the entry goes as a `note` to the card's supervisor orchestrator instead, which decides whether to start a run for it. An entry for an artifact whose card is closed is recorded as an event on the card and wakes nobody.
- Test: `delivery::sources::page_answer_reaches_owning_worker`, plus `delivery::sources::page_answer_replay_is_not_delivered_twice`.

## 4. Hook events

### 4.1 One entry point

Every hook Tiller installs runs `tiller hook <event>`, with the hook input JSON on stdin, except `PreToolUse`, which runs `tiller guard pretool --role <role>`, the one decision owner `authority.md` names; it records the same coalesced observation as the other tool events. Tiller registers hooks in project or local scope, never user scope (host-capabilities), and only for events whose handler below is not `ignore`. The input struct keeps unknown fields (`#[serde(flatten)] extra`), so a new Claude Code field never breaks a hook. The observation payload is a per-event allowlist of fields, not the raw input: `prompt`, `last_assistant_message`, `tool_input`, `tool_response` and `error_details` are never stored, because they can hold whole replies, file contents or secrets. An ask's text lives only in `asks.text`, and the Stop hook stores only the tags it extracted. T2.6 snapshots each event's allowlist.

Observations are append-only. A hook delivered twice with identical input is written once: the handler keys it `hook:<claude session id>:<event>:<sha256 of the input>` in `idempotency_keys`.

The handler resolves the Tiller session in this order: the `TILLER_SESSION_ID` environment variable set at launch, then `sessions.claude_session_id = session_id`, then none. An unmapped event is still recorded (`observations.session_id` null) so reconcile can bind it later.

Budget: p95 under 10 ms with the daemon down (T2.6 acceptance). A handler that needs more (none in this table) must move the work to the daemon.

### 4.2 Event to handler

Claude Code documents 33 hook events [D]. Each maps to exactly one handler. `event` is the value written to `observations.event`; `verdict` is the activity it implies, or null when it implies none (`state.md`, Observation).

| Hook event | Handler | `observations.event` | Verdict | Also |
|---|---|---|---|---|
| `SessionStart` | `session_bind` | `SessionStart:<source>` (`startup`, `resume`, `clear`, `compact`, `fork`) | null | `run.bind_session`: binds `claude_session_id` and the pane; the model goes in the payload. On `clear` or `fork` the Tiller session row is rebound to the new Claude id and the old id goes in the payload. It fires before the login is checked [T], so it never proves the session works. The daemon runs `mem.load` (`memory.md`, section 7) on this observation to record which memory revisions the session could load; the hook itself hashes nothing, so it stays inside the 10 ms budget |
| `Setup` | `ignore` | - | - | Tiller never launches with `--init` or `--maintenance` |
| `UserPromptSubmit` | `intake` | `UserPromptSubmit`; `wake` for a wake-up line for this session; `wake_mismatch` for any other wake-up line | `working` | section 2 for asks, section 3.2 for wake-up lines |
| `UserPromptExpansion` | `ignore` | - | - | `UserPromptSubmit` records the prompt the owner typed |
| `PreToolUse` | `guard` (`tiller guard pretool --role <role>`) | `PreToolUse` (coalesced, 4.3) | `working` | the allow or deny decision is `authority.md`'s (P58, P59) |
| `PermissionRequest` | `permission_request` | `PermissionRequest` | null | recorded at once; the activity turns `waiting_permission` only on `Notification:permission_prompt`, about 6 s later [D], so a quick approval does not flicker the board |
| `PermissionDenied` | `permission_denied` | `PermissionDenied` | null | payload keeps `tool_name` and `reason` [D]; evidence for P37 |
| `PostToolUse` | `tool_done` | `PostToolUse` (coalesced) | `working` | |
| `PostToolUseFailure` | `tool_done` | `PostToolUseFailure` (coalesced) | `working` | |
| `PostToolBatch` | `ignore` | - | - | `PostToolUse` already covers each call |
| `Notification` | `notify` | `Notification:<notification_type>` | per 4.4 | |
| `MessageDisplay` | `ignore` | - | - | fires while text renders; no state |
| `SubagentStart` | `subagent` | `SubagentStart` | `working` | payload keeps `agent_type` |
| `SubagentStop` | `subagent` | `SubagentStop` | null | the main agent continues |
| `TaskCreated` | `ignore` | - | - | Claude Code's own task list, not Tiller cards |
| `TaskCompleted` | `ignore` | - | - | as above |
| `Stop` | `turn_end` | `Stop` | `idle` | section 2.5 for orchestrators. When `background_tasks` or `session_crons` is non-empty [D], the payload says so and the idle alarm treats the session as waiting on its own work, not stuck |
| `StopFailure` | `turn_failed` | `StopFailure:<error>` | `errored` | `error` is one of `rate_limit`, `overloaded`, `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error`, `unknown` [D]. `rate_limit` also asks the daemon for an early quota read. The daemon never retries or resumes a session for an API error; Claude Code's own retry is the only one (decided at CP2, C07; `recovery.md`, R12) |
| `TeammateIdle` | `ignore` | - | - | fleet roles do not use agent teams |
| `InstructionsLoaded` | `instructions` | `InstructionsLoaded` | null | payload: file path relative to the project or `~`, and its sha256. T1.5 uses it to show which sessions loaded a memory revision |
| `ConfigChange` | `config` | `ConfigChange` | null | evidence that the session's `config_hash` drifted |
| `CwdChanged` | `cwd` | `CwdChanged` | null | evidence for T1.4 when a worker leaves its worktree |
| `DirectoryAdded` | `cwd` | `DirectoryAdded` | null | as above |
| `FileChanged` | `ignore` | - | - | Tiller watches no files through Claude Code |
| `WorktreeCreate` | `worktree` | `WorktreeCreate` | null | treehouse owns worker worktrees; a native worktree is evidence for T1.4 and reconcile |
| `WorktreeRemove` | `worktree` | `WorktreeRemove` | null | as above |
| `PreCompact` | `compact` | `PreCompact` | null | a breakpoint signal for handoff (T4.5): for a live orchestrator session, one `note` asking for the handoff, keyed `breakpoint:<observation id>`, written by `tiller hook` after the observation (idempotent, so a daemon pass may repeat it); none when the session wrote a handoff since, or the observation is over 30 minutes old |
| `PostCompact` | `compact` | `PostCompact` | null | |
| `PreModelSwitch` | `ignore` | - | - | `PostModelSwitch` records the result |
| `PostModelSwitch` | `model` | `PostModelSwitch` | null | the run's model drifted from its spawn; evidence for P02 |
| `Elicitation` | `elicitation` | `Elicitation` | null | debounced like permissions: `Notification:elicitation_dialog` sets `waiting_input` |
| `ElicitationResult` | `elicitation` | `ElicitationResult` | `working` | |
| `SessionEnd` | `session_end` | `SessionEnd:<reason>` | null for `clear` and `resume`; `ended` for every other reason, including ones added later | `ended` runs `session.end`. `clear` and `resume` are followed by a `SessionStart` that rebinds the same Tiller session [D: reasons `clear`, `resume`, `logout`, `prompt_input_exit`, `other`] |
| any event not listed | `unknown_event` | the event name as given | null | recorded with the full payload; T2.6 logs it once per name so the table can be extended |

Handlers marked `ignore` are not registered, so they cost nothing. `unknown_event` catches an event Tiller's settings register that this binary does not map (settings newer than the binary). A new Claude Code event is never registered by accident, so `tiller doctor` compares the installed Claude Code version with the version this table was checked against and warns when it is newer. The test `observe::hooks::every_event_has_exactly_one_handler` checks that the handler table is total over this list and the documented event names, and that no event has two entries.

### 4.3 Coalescing

`PreToolUse`, `PostToolUse` and `PostToolUseFailure` fire on every tool call. Their handlers write an observation only when the session's `activity` is not already `working` or its `activity_observed_at` is older than 60 s. The verdict stays fresh for staleness without a row per tool call.

### 4.4 Notification types

All 13 documented `notification_type` values [D], each to exactly one verdict:

| `notification_type` | Fires when [D] | Verdict | Effect |
|---|---|---|---|
| `permission_prompt` | a tool use needs approval and the prompt has waited about 6 s | `waiting_permission` | Needs you, in the permission-prompt section, never an attention row (`state.md`, Attention) |
| `idle_prompt` | about 60 s after a response with no typing | `idle` | refreshes idle; the outbox may deliver |
| `auth_success` | authentication completes | null | recorded only |
| `elicitation_dialog` | an MCP form waits about 6 s | `waiting_input` | Needs you |
| `elicitation_url_dialog` | an MCP server asks to open a URL, about 6 s | `waiting_input` | Needs you |
| `elicitation_complete` | a URL elicitation completes | `working` | |
| `elicitation_response` | an elicitation response is sent | `working` | |
| `agent_needs_input` | a background session waits on input while agent view is open | `waiting_input` | Needs you |
| `agent_completed` | a background session finishes, agent view open | null | recorded only; fleet roles do not use agent view |
| `quota_auto_resume_fired` | Claude Code continues after a usage limit | `working` | asks the daemon for an early quota read |
| `quota_auto_resume_stale` | a limit reset while the machine slept; waits for Enter | `waiting_input` | Needs you, badge "press Enter after quota reset" |
| `quota_auto_resume_disabled` | the wait for a usage limit ended without continuing | `errored` | the error badge names quota; T1.2 decides the retry |
| `plugin_notification` | a mod raised a notification | null | recorded only |
| any other value | - | null | recorded with the raw type |

This replaces plan v2's line that `permission_prompt`, `elicitation_*` and `agent_needs_input` go "to attention": they reach Needs you through the session's activity, as `state.md` decided, so a decision list never fills with dialog clicks.

### 4.5 Sources other than hooks

| Source | Detects | `observations.event` | Verdict |
|---|---|---|---|
| Transcript tailer | a user entry whose text is `[Request interrupted by user]` or `[Request interrupted by user for tool use]` [T: both strings appear in host B transcripts] | `interrupt` | `idle` |
| Transcript tailer | on a host with `intake_source = transcript`: a user entry (an ask or a wake-up line), an assistant entry ending the turn, an API error entry | `transcript:user`, `transcript:turn_end`, `transcript:api_error` | `working`, `idle`, `errored` |
| herdr poller | the pane's `agent_status` [T: `working`, `idle`, `done` seen on host B; herdr also documents `blocked`, I] | `herdr:<status>` | `working` to `working`; `idle` and `done` to `idle`; `blocked` to `waiting_input` unless the newest hook verdict is `waiting_permission` |
| herdr poller | the pane is gone | `pane_gone` | `ended` |
| Liveness check | the session's process is not running while the pane exists | `process_gone` | `unknown` |
| Worker status line (`tiller status`, 6.4) | the worker's own report | `status:<state>` | null (the line is a run fact, not an activity) |

Hook verdicts win over herdr's when both are newer than the projection: Firstmate measured herdr reading `idle` through a whole turn (`plan.md`, "How events reach the board with zero agent tokens"). The rule is the observation with the newest `observed_at`, except that a `herdr:idle` never replaces a hook `working` less than 30 s old. T2.6 pins this with `observe::merge::herdr_idle_does_not_override_fresh_hook_working`.

### 4.6 Forge sources: PRs and deploys

The daemon's forge poller (T2.5) reads GitHub through `gh-axi` only. It never writes to the forge; the only forge writes are `tiller land`'s (`authority.md`, section 6).

| Source | Reads | Every | Writes |
|---|---|---|---|
| PR poller | each open PR on a card: head SHA, state, checks, merge commit | 60 s while a run on the card is active or an approval on the PR is open, else 5 minutes | `pr.refresh` (`state.md`), which marks approvals `stale` on a moved head |
| Deploy poller | the runs of the project's `deploy_workflow` on its default branch, with `head_sha`, `status`, `conclusion`, the environment each job deployed to, and the run URL | 60 s while a deploy-watch obligation is open, else not at all | `deploy.record` (`state.md`, section 8) |
| Ruleset check | the default branch's rulesets and their bypass actors | at `tiller project add` and hourly; `tiller doctor` only reports the stored value and the last check | `project.merge_gate` (`state.md`, section 8; `authority.md`, Enforced merge gate) |

The deploy watch (decided at CP2, C02 amended by S07). A run counts for the watch only when it matches both the commit and the environment:

- **Commit.** Its `head_sha` equals the pushed SHA, or descends from it on the default branch (the superseded case). A run for an older commit never counts, however recent it is.
- **Environment.** When `projects.deploy_environment` is set, the run must have a job that deployed to that environment (`gh-axi` reads the run's deployments). A run of the same workflow that skipped the environment, or deployed only to another one, does not count. When it is null, the workflow run's own conclusion is the result.
- **Results.** `success`, `failed`, `cancelled`, `superseded`, `missing` (no matching run 10 minutes after the push) and `timed_out` (a matching run not concluded by `deploy_timeout_minutes` after the push). `state.md` section 7 says what each one records; only `success` and `superseded` close the card.
- **Idempotency.** `deploy.record` is keyed by the obligation id, the run id and the result, so a re-poll writes nothing new. `missing` and `timed_out` are written once per watch; a later matching conclusion still resolves the watch.
- Tests: `daemon::forge::deploy_success_must_match_sha_and_environment`, `daemon::forge::deploy_for_older_commit_does_not_count`, `daemon::forge::superseded_deploy_closes_landed`, `daemon::forge::missing_and_timed_out_deploys_open_recovery`.

## 5. Transcript tailing

- **Path.** `<config dir>/projects/<slug>/<claude session id>.jsonl`, where the slug is the physical working directory with every character that is not a letter or digit turned into `-` (host-capabilities [T]). The tailer derives it from the session's cwd and Claude id and stores it in `sessions.transcript_path`. Subagent transcripts under `<slug>/<session id>/subagents/` are not tailed.
- **Mechanism.** The daemon watches each live session's transcript with `notify` and reads appended lines only. It keeps the byte offset in memory and writes it into each observation's payload (`transcript_offset`), so a restarted tailer resumes from the newest recorded offset for that session and never records an entry twice. A truncated or replaced file (offset past the end) restarts from 0 and dedupes by entry uuid.
- **Lag.** Claude Code writes the transcript asynchronously [D]. Transcript observations use the entry's own timestamp as `observed_at`, so a late read never moves the projection backwards (`state.md`, out-of-order rule).
- **What it records.** Interrupts on every host (Stop does not fire on them [D]). On hosts with `intake_source = transcript`, also the rows in 4.5 that hooks would have given. It never records tool calls one by one; it coalesces like 4.3.
- **Privacy.** Transcript text is never copied into an observation payload except an ask's text into `asks.text` on reduced hosts. Raw transcripts are read only and never committed (`state.md`, section 9).
- **Retention.** The tailer stops watching a session 10 minutes after it ends.

## 6. The `tiller` command surface

### 6.1 Conventions

- **Ids.** Every response names entities by ULID (`id`) and, for cards, `display_id`. Inputs accept a ULID, a short form (its last 7 characters, from the random part; a prefix would be shared by every id minted in the same minutes), or a display id where the entity has one. Responses print the short form beside each ULID. A run is addressed as `<display id> <role>` (`alpha-42 build`) or its herdr name (`alpha-42-build`, `alpha-42-build-2`); the short role names are `scout`, `build`, `review` and `verify` for the roles `scout`, `builder`, `reviewer` and `verifier`. An ambiguous short form fails with exit 3 and lists the candidates.
- **Output.** The default is TOON, the compact form the axi tools print, for agents. `--json` prints the stable JSON contract, which insta snapshots (T2.2). Both come from one serde struct. Every response has `ok`, `data`, and, for a mutation, `events` (the `seq` values written) and `replayed` (true when an idempotency key matched). A response may end with `help` lines naming the next useful command, as gh-axi does. Diagnostics go to stderr; stdout is only the response.
- **Errors.** `{"ok":false,"error":{"code":"<code>","message":"<readable>","entity":"<entity>","id":"<id>","current":{...},"hint":"<next command>"}}`. The `conflict` message follows `state.md`: `conflict: card 01J... is at rev 7, you sent 5; title is now "<value>"`.
- **Times.** JSON carries RFC 3339 UTC. TOON shows Europe/Lisbon with the weekday (`Tue 2026-11-03 14:00`). A date input without a time (`--until 2026-11-07`) means 09:00 Lisbon that day; an input with an offset is taken as given.
- **Actor.** Inside a session Tiller launched, `TILLER_SESSION_ID` (and for workers `TILLER_RUN_ID`) are set at launch, and the actor is `session:<id>`. The daemon is `daemon`, a hook is `hook`, and anything else on the owner's shell is `owner`. T1.4 decides which actor may run which command; a refusal is exit 6.
- **Owner marker.** Every mutation refuses with exit 6 (`owner_marker`) while `~/.tiller/owner` names another system. Reads always work.

### 6.2 Exit codes

| Code | Name | Meaning | Safe to retry |
|---|---|---|---|
| 0 | `ok` | done, or replayed from an idempotency key, or already in the requested state | - |
| 1 | `internal` | a bug or an unreadable database | after a fix |
| 2 | `usage` | bad arguments | no, fix them |
| 3 | `not_found` | no such entity, or an ambiguous prefix (candidates listed) | no |
| 4 | `conflict` | `expected_rev` is stale; the current value is in `current` | after re-reading |
| 5 | `refused` | a precondition failed (`state.md`, section 8); the failed fact is named | after the fact changes |
| 6 | `forbidden` | the actor may not do this (T1.4), or the owner marker names another system | no |
| 7 | `unavailable` | database busy past `busy_timeout`, the daemon is needed and down, or herdr or the forge is unreachable; nothing was written | yes, same key |
| 8 | `uncertain` | an outside action was sent and its result could not be read: `tiller land` (the approval is `uncertain`), or a mutation forwarded to a peer (10.3, 504 `forward_uncertain`) | `land`: no; reconcile resolves it from the forge. A forwarded mutation: yes, same key, since the peer applies a key once |

Hooks always exit 0, except what the PreToolUse guard (T1.4) and the Stop hook in `block-once` mode print as their decision.

### 6.3 Idempotency

Every command is in one of three classes, named in the tables below:

- **natural:** a transition guarded by its preconditions. The command first checks whether the entity is already in the target state with the same arguments; if so it returns the current state with exit 0 and `replayed: true` and writes nothing, before any precondition runs. Otherwise the preconditions apply, and a different target is refused (exit 5). An edit whose fields all equal the current values is a no-op the same way only at the current `rev`; a stale `rev` is always a conflict (exit 4), even when the values already match, because two writers that compute the same value from one stale read must not both succeed. Only a keyed edit replays at a stale `rev`: a known key returns the stored response.
- **keyed:** a create or an append. It carries an idempotency key; a known key returns the stored response from `idempotency_keys` and writes nothing (`state.md`, invariant 10). Keys come from `--key <k>`, or, when the caller is a Tiller session and gives none, the CLI derives `auto:<session id>:<command>:<sha256 of the normalized arguments>:<turn anchor>`. The turn anchor is the id of the session's newest turn-boundary observation (`UserPromptSubmit`, `wake`, `Stop`, `StopFailure`, `interrupt`, or `transcript:turn_end` on a hook-less host), so a retry inside one turn replays and the same command after the next turn boundary is new. Hooks and the board always send a key. A shell caller (the owner, a script) that gives no key gets a fresh key minted and printed in the response; repeating the command with `--key <printed key>` replays, and repeating it without one creates again.
- **not idempotent:** named, with the reason: a keyed command run from a shell without `--key` (by design: a weekly script may send the same note twice on purpose, and the minted key makes the retry path explicit), `tiller agent interrupt`, `tiller land` once its merge call is sent, and `tiller board` (each call mints a new one-time login link). Each says so in its help.

Keys expire after 30 days (`state.md`, Bookkeeping).

### 6.4 Commands

Reads have no side effects and are all trivially idempotent; they are listed after the mutations. "Caller" is the expected caller; T1.4 enforces it.

**Asks and intake**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller ask note <project> (--text <t> \| -) [--key <k>]` | `ask.record`, `outbox.enqueue` | script, owner | keyed with `--key`; not idempotent without one, because a weekly script may send the same text twice on purpose |
| `tiller ask resolve <ask> <target>` (2.4) | `ask.resolve` (+ `card.create` for `new`) | orchestrator | natural (key `ask.resolve:<ask>`) |
| `tiller ask link <ask> <target>` | `ask.link` (+ `card.create`) | orchestrator | keyed |
| `tiller intake stats [--days 7]` | read | owner | - |

**Cards**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller card create <project> --kind K --title T [...] [--ask <ask>]` | `card.create`; without `--ask` the card has no `created_by_ask_id` and no `ask_cards` row | orchestrator, owner | keyed |
| `tiller card edit <card> --rev <n> [--key <k>] [--title] [--priority] [--body] [--size] [--acceptance] [--category] [--dispatch <model>:<effort> \| --no-dispatch]` | `card.edit`; `--dispatch` and `--no-dispatch` set or clear `cards.dispatch_override` and are owner only | orchestrator, owner | natural at the current rev (the same values are a no-op), or keyed with `--key`; a stale rev is exit 4 unless the key replays |
| `tiller card hold <card> --until <date> --reason <t>` | `card.hold` | orchestrator, owner | natural (same `until` and reason) |
| `tiller card unhold <card>` | `hold.release` (`removed`) | orchestrator, owner | natural |
| `tiller card dep add <card> <depends-on>` / `dep rm` | `dep.add` / `dep.release` (`removed`) | orchestrator, owner | natural |
| `tiller card close <card> --outcome <o> [--reason <t>]` (`--reported` is short for `--outcome reported`) | `card.close` | orchestrator, owner | natural |
| `tiller card complete <card> --evidence <t>` | `card.close` (`completed`, ops cards) | orchestrator, owner | natural |
| `tiller card cancel <card> --reason <t>` | `card.cancel` | orchestrator, owner | natural |
| `tiller card reopen <card> --reason <t>` | `card.reopen` | orchestrator, owner | natural |
| `tiller card comment <card> (--text <t> \| -)` | `outbox.enqueue` (`note`) to the project's orchestrator, recorded as an event on the card | owner, board | keyed |

**Attention and approvals**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller attention open <card> --kind K --question <t> [--evidence <json>] [--recommendation <t>] [--consequence <t>] [--options <json>]` | `attention.open`; an option may carry an owner action (`state.md`, Attention), checked against the record before the item opens | orchestrator, reconcile | keyed |
| `tiller attention answer <item> (--text <t> \| - \| --ask <ask>) [--key <k>]` | `attention.answer`; `--ask` records an answer the owner gave in the orchestrator chat, citing the hook-recorded ask, whose exact words become the answer (`answered_via = chat`), with the same checks as a chat approval (`authority.md`, G06) | owner; the orchestrator with `--ask` | keyed: the same key or the same exact words replay; a different answer is refused (`state.md`, Attention) |
| `tiller attention withdraw <item> --reason <t>` | `attention.withdraw` | orchestrator, reconcile, owner | natural |
| `tiller approval request <card> --kind merge\|land_direct [--pr <n>] [--action squash]` | `approval.request` (binds the cached head SHA) | orchestrator, daemon | natural: the same binding returns the open approval |
| `tiller approve <approval> [--reject] [--sha <head>] [--key <k>]` | `approval.decide` | owner | keyed; `--sha`, when given, must equal the bound head or exit 5 says the approval is stale |
| `tiller approve <approval> --ask <ask>` | `approval.decide` on the owner's chat word (`authority.md`, G06) | orchestrator | keyed |
| `tiller land <approval>` | `approval.execute`, `approval.finish` | daemon; run by the owner it asks the daemon to land and waits for the result (exit 7 when the daemon is down, or still not landed after 30 minutes, with the open land notices in `current`; the approval stays `accepted`), it never merges in its own process (`authority.md`, section 6) | natural up to `executing`; once the merge call is sent, a lost result is exit 8 and is never retried blind |
| `tiller grant create <project> ...` / `revoke <grant>` | `grant.create` / `grant.revoke` (conditions in `authority.md`) | owner | keyed / natural |
| `tiller away start --until <date> ...` / `end` | `away.start` / `away.end` (scope in `authority.md`) | owner | keyed / natural |
| `tiller mem note [--contradicts <memory>]` / `propose` / `approve` | `mem.note` / `mem.propose` / `mem.decide` (`memory.md`) | fleet roles; `approve` owner only | keyed |

**Runs and agents**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller orchestrator start <project> [--restart]` | `session.start` (role `orchestrator`); opens the herdr workspace and launches Claude. `--restart` first stops the live orchestrator (proven as a relaunch's stop is), ends it `restarted` and closes its pane, so the new one runs on the current binary and role prompt | owner, daemon; `--restart` owner only | natural: a live orchestrator for the project is returned, not a second one; `--restart` replaces it |
| `tiller spawn <card> <role> [--model M --effort E] [--brief-file <f>]` | `run.start` (reservation, run, session), then the worktree and pane (T4.2) | orchestrator | keyed; a second spawn of the same card and role while one is active is refused by the unique index |
| `tiller status <state> <text> [--until <date>]` (`state`: `working`, `needs-decision`, `blocked`, `paused`, `done`, `failed`) | `run.status` (`run.pause` with `--until`) and, in the same transaction, `outbox.enqueue` (`wake`) to the supervisor | worker | keyed (derived key) |
| `tiller run pause <run> --until <date> --reason <t>` | `run.pause`: the orchestrator's form of a dated pause, which silences the run's alarms until then (recovery.md, R12); the worker's form is `tiller status paused --until` | orchestrator, owner | natural (same `until` and reason) |
| `tiller pr link <url>` | `pr.link` | worker, reconcile | natural per PR url |
| `tiller pr describe --title <t> (--body <t> \| --body-file <f>)` | `pr.describe`: the title and body the daemon opens a `direct-pr` PR with; `tiller status done` from a `direct-pr` builder is refused (exit 5) until it is recorded | builder | natural (same title and body) |
| `tiller done [<run>] [--outcome <o>]` | `run.end`; refuses while commits are unlanded (P51). The daemon runs `run.clean` afterwards. `<run>` is required outside a worker session (the orchestrator ending a crashed worker's run, recovery.md R03). Without `--outcome`, a scout, reviewer or verifier run ends `reported` once its report or review row exists; a builder's run ends `landed` only through `approval.finish`; while its PR is open a builder's bare `tiller done` is refused with "PR #n is not merged; stay on the card, or end with --outcome reported once it holds your last commit", and `tiller done --outcome reported` ends the run on a ship card once its head, read on its own host at that moment, is on an open PR of the card with no uncommitted changes: the project slot is freed while the card tracks the PR to landing. With no PR it is refused with "the work is not landed; stay on the card" unless it gives `--outcome failed` or `--outcome abandoned` (recovery.md, R13) | worker, orchestrator | natural |
| `tiller run clean <run>` | `run.clean` | orchestrator, daemon | natural; refuses while `unlanded_commits` is non-zero or unknown |
| `tiller agent relaunch <run> --note <t>` | `run.relaunch`: a new session on the same run, same worktree and brief (P24); the prompt, `settings.json` and `mcp.json` are rendered again from the current binary and config, so an upgrade reaches the new session | orchestrator, owner (`authority.md`, `agent-control`) | keyed |
| `tiller agent relaunch <run> --new-attempt --brief-file <f> --note <t> [--model <m> --effort <e>]` | `run.relaunch`: ends the run (`relaunched`) and starts attempt n+1 with `predecessor_run_id`, same worktree. `--model` and `--effort` come together and only with `--new-attempt`; the new run row carries them, and they may only raise the dispatch rule unless the owner asks | orchestrator, owner (`authority.md`, `agent-control`) | keyed |
| `tiller agent resume <run>` | `session.resume`: writes `session.resume_attempted` (so recovery.md's rate limit can count attempts), then relaunches Claude with `--resume <claude session id>` in the run's worktree, which keeps the session row when the `SessionStart:resume` hook binds the same id (P25; recovery.md, "Resuming a session") | daemon (after a crash or a reboot only, `authority.md` role matrix), orchestrator, owner | natural: a live session is left alone |
| `tiller agent interrupt <run>` | none; sends Escape through herdr and records an observation (`interrupt_sent`). The response reports `cancel: confirmed` only when the tailer sees the interrupt marker or a hook reports the turn ended within 10 s, else `cancel: unconfirmed` (Firstmate's `agent-control`: busy state is never rewritten as proof) | orchestrator, owner | not idempotent: a second Escape reaches whatever the agent does next, so the command refuses when the session is not `working` |
| `tiller agent focus <run\|session>` | none; `herdr agent focus <name>`, with `--machine` for a remote host | owner, board | natural |
| `tiller send <run\|session> --kind steer\|note (--text <t> \| -)` | `outbox.enqueue` | orchestrator, owner | keyed |
| `tiller inbox [<message>]` | `outbox.ack` (3.2) | the target session | natural |

**Dispatch rules (P02; decided at CP2, C04).** The model and effort of every run come from a `[dispatch]` table in `~/.tiller/config.toml`, which fleet roles cannot read or write (`authority.md`, section 8). It holds an ordered model list (cheapest first), the effort order `low` < `medium` < `high` < `xhigh` < `max`, and rules keyed by card kind and size, each naming a model and an effort, with a default rule for anything unmatched. `run.start` resolves the rule inside its transaction and writes the resolved model, effort and the rule that matched into the `run.started` payload. `tiller spawn --model M --effort E` from the orchestrator may raise either value above the rule and is refused (exit 6, naming the rule) below it, since a downgrade needs the owner. The owner lowers or raises it for one card with `tiller card edit <card> --dispatch <model>:<effort>`, which sets `cards.dispatch_override` (owner only; `state.md`, Card); an override wins over the rule and is quoted in the payload. T4.2 enters the shipped defaults: small on haiku low, medium on sonnet medium, large on opus medium, anything else sonnet medium. A `PostModelSwitch` that leaves the resolved model is recorded as drift (4.2). Test: `agents::spawn::dispatch_resolves_model_effort_from_rules`, plus `agents::spawn::orchestrator_downgrade_refused`.

**Records**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller obligation open <card> --what <t> (--on <event filter> \| --due <date> \| --standing <event filter>)` / `cancel <id>` | `obligation.open` / `obligation.cancel` | orchestrator | keyed / natural |
| `tiller artifact add <card> --kind K --title T (--url <u> \| --path <p>)` | `artifact.add` | worker, orchestrator | keyed (natural per url) |
| `tiller review record <run> --reviewer R --coverage C --verdict V [--high n --medium n --low n] [--url <u>]` | `review.record` | worker, daemon | keyed |
| `tiller review plan <project>` | none (a read): the chunk plan of the next pass, from the project's checkout and `prs` (`docs/design/pre-launch-validation.md` section 4) | orchestrator, owner | n/a |
| `tiller review start <project>` | `review_pass.start`: fetches the default-branch head, computes the plan and stores both | orchestrator (its own project), owner | natural |
| `tiller review brief <pass> (--chunk <n> [--only <path>]... \| --summary) [--out <file>]` | none (a read): the brief of one chunk run or of the summary run, for `tiller spawn --brief-file` | orchestrator, owner | n/a |
| `tiller review finish <pass> [--report <artifact>] [--fix-card <card>]...` / `abandon <pass> --reason <r>` | `review_pass.finish` / `review_pass.abandon` | orchestrator (its own project), owner | natural |
| `tiller review due [--project <p>]` | none (a read): met weekly obligations with no pass after them, each marked `overdue` after 24 hours | orchestrator, owner | n/a |
| `tiller handoff write (--conclusions <t> --next <t> \| -)` | `handoff.write` | orchestrator | keyed |
| `tiller handoff show [--project <p>]` | none (a read): the project's newest `handoffs` row, each id resolved, with what changed since it was written | orchestrator (its own project), owner | n/a |

**Projects, hosts and config**

| Command | Mutation | Caller | Retry |
|---|---|---|---|
| `tiller project add <id> --name <n> [--repo owner/name] [--remote <url>] [--default-branch <b>] --delivery D [--checks <json>] [--capacity n]` / `edit <id> --rev <n>` with any of the same flags / `retire <id>` | project rows (`project.add`, `project.edit`, `project.retire`) | owner | natural (add with the same values returns the row) |
| `tiller project delete <id> [--yes]` | `project.delete`: the hard delete of the project and everything recorded for it on this host, its events included (state.md section 8, Deleting a project). Without `--yes` it removes nothing: it prints the rows it would remove per table and exits as a refusal, or on a terminal asks for the id to be typed. Refused while a run or a session of the project is live or one of its approvals is executing. Also withdraws every open attention item about the project or one of its sessions, in any project including `fleet`, with a reason that names the delete; the confirmation says how many. Leaves the checkout, worktrees, forge repository and `checkouts.<id>` alone, and says the key remains | owner | natural: a second delete of the same id is `not_found` |
| `tiller project stage <id> pre-launch\|launched --rev <n> [--from <sha>]` | `project.stage`; to pre-launch the CLI reads the default-branch head from the project's checkout first, unless `--from` names the commit | owner | natural |
| `tiller host edit [--lane-capacity n] [--ram-floor-mb n] [--disk-floor-mb n] --rev <n>` | host row (`host.edit`) | owner | natural |
| `tiller config set <key> <value>` | `config.set`: writes `~/.tiller/config.toml` atomically (temporary file, then rename), then commits a `config.changed` event; a crash between the two leaves a file newer than its event, which `tiller doctor` reports | owner | natural |
| `tiller peer add <label> --ssh <alias> [--herdr-machine <id>] [--read-only]` (section 10.1) | on the peer, `peer.accept` through `ssh -t <alias> tiller peer accept`; then here, `peer.pair` (`host.add` with `kind = remote`, `herdr_machine` (default: the label), `peer_scope`, `peer_host_id`) after writing the key file; the SSH alias is config (`peers.<label>.ssh`), not a column | owner, with a terminal | natural: the same label and peer pair again, revoking the old key on the peer |
| `tiller peer accept --from <label> --viewer-host-id <id> --pubkey <b64> --scope act\|read` | `peer.accept`; prints this host's `meta.host_id`, label, schema and peer protocol versions as JSON | owner, with a terminal (run by `peer add`, rarely by hand) | natural: the same viewer and key return the same row |
| `tiller peer revoke <viewer label>` | `peer.revoke`; an open link from that key closes at once | owner | natural |
| `tiller peer remove <label>` | `peer.remove`; deletes `~/.tiller/peers/<label>.key` after the commit; the peer's key row stays until `peer revoke` there, and is useless without the private half | owner | natural |
| `tiller peer list` | read: each peer's label, scope, link state, `as_of`, and on a peer, the accepted board hosts | owner | read |

**Plumbing**

| Command | Does | Caller | Retry |
|---|---|---|---|
| `tiller hook <event>` | section 4 | Claude Code | keyed (the hook's own key) |
| `tiller guard pretool --role <role>` | the PreToolUse decision (`authority.md`) plus its observation | Claude Code | natural: a decision, no state change beyond the coalesced observation |
| `tiller board` | prints a one-time login link that mints the owner cookie (7.1) | owner | not idempotent: each call mints a new link, and a used link stops working |
| `tiller owner set tiller\|firstmate` / `tiller owner show` | writes `~/.tiller/owner` (the owner marker, state.md Host; recovery.md R20) atomically and commits an `owner.changed` event when the marker names `tiller` again; `show` reads it. `authority.md` denies the whole `tiller owner` namespace to every fleet role | owner | natural: the same name is a no-op |
| `tiller migrate up\|down [--to <n>]` | the schema migration recovery.md R11 describes: `up` is what the daemon and the CLI run on open; `down` is the owner's rollback with the newer binary, after the same verified copy | owner (`down`), daemon and CLI (`up`) | natural: a database already at the target version changes nothing |
| `tiller daemon` | runs the pollers, delivery, reconcile, the quota feed and the API (T2.8); takes the single-instance lock | the service manager | natural: a second instance exits 0 naming the running pid |
| `tiller import <bundle> [--dry-run]` | `import.apply` (T2.4) | owner | natural: a second import is a no-op |
| `tiller export --jsonl <dir>` / `tiller backup` / `tiller restore <file>` | T2.3 | owner, daemon timer | export and backup natural (same content, new file name); restore refuses while the daemon runs |
| `tiller health numbers [--weeks <n>]` / `tiller health baseline <bundle>` | read only (T7.1): the avoidable-interventions-per-accepted-outcome figure with its sample size, from Tiller's own weeks (`numbers`, default 8) or computed the same way over the import bundle's history (`baseline`: `cards.jsonl` outcomes plus `baseline/touches.jsonl`, the 21 days before the manifest's `generated_at`). Rework is not observable in the baseline and the dates are day-only; the output says so in `caveats`. `baseline` feeds the T7.4 report and is never shown on the Health page | owner | read |
| `tiller doctor` | the health checks of P43 (tool versions, herdr, quota-axi, no-mistakes daemon against its CLI, hook registration) | owner, orchestrator at start | read |
| `tiller peer serve --from <label>` | the peer end of the peer link (section 10.2): reads `state.db` itself, needs no daemon, and applies verified forwarded mutations through the same core functions. Refuses unless started by sshd for the owner (`SSH_CONNECTION` set, no `TILLER_SESSION_ID`, no `CLAUDECODE`, no Claude Code process among its ancestors) | the board host's daemon, over SSH | each forwarded mutation is keyed by the board's `idempotency_key` |

**Reads**

`tiller snapshot` (the boot snapshot's JSON, P38), `tiller board [--project p] [--filter all|active|needs]`, `tiller card show <card>` (state, column, runs with activity and freshness, holds, deps, attention, PRs, approvals, artifacts; P22), `tiller card list [...]`, `tiller card timeline <card>`, `tiller ask list [--unresolved]`, `tiller ask show <ask>`, `tiller attention list [--open]`, `tiller run show <run> [--tail <n>]` (with a bounded herdr pane tail, at most 200 lines), `tiller run list`, `tiller agents`, `tiller machines [--local]` (`--local`: this host's own row only, which a peer polls), `tiller quota`, `tiller events [--since <seq>] [--follow] [--card <card>]`, `tiller config get [<key>]`, `tiller config hash`, `tiller config diff <peer>`, `tiller version`.

Every mutation the CLI exposes is a row in `state.md` section 8 (this task added `ask.link`, `session.start`, `session.end`, `project.*`, `host.*`, `config.set` and `obligation.cancel` there; T1.6 added the `tiller run pause`, `tiller done <run>`, `tiller owner set` and `tiller migrate` forms that recovery.md relies on); no command writes a table in any other way.

## 7. Board API

### 7.1 Transport and safety

- The daemon serves `http://127.0.0.1:7421` only, never another interface (AGENTS.md).
- **Host check.** Every request must carry `Host: 127.0.0.1:7421` or `localhost:7421`; any other value gets 421. This stops DNS rebinding. The owner can name extra hosts with `tiller daemon --allow-host <host>` (repeatable, exact match), such as a reverse proxy's name; those get reads only: a mutation from them gets 403 and the login link 421.
- **Credential.** `authority.md` (Board API credential) owns the requirement; this is the mechanism. Reads (`GET`, including the SSE stream) are served to any request that passes the Host check. Mutations need the owner cookie, `tiller_owner`, `HttpOnly; SameSite=Strict; Path=/`, which the browser sends with `fetch` and `EventSource` alike. It is minted only through the one-time login link `tiller board` prints (`GET /login/<one-time token>` sets it and redirects to `/`); `GET /` never sets it. It survives daemon restarts, so open tabs keep working, and expires 30 days after it was minted (`Max-Age=2592000`, and the stored hash carries the same expiry, so a copied cookie stops working too); after that, `tiller board` must be run again (decided at CP2, C15). Its hash is stored under `~/.tiller`, which every fleet role is denied. A mutation also needs `Content-Type: application/json`, `Sec-Fetch-Site: same-origin` and an `Origin` equal to the daemon's own. No CORS headers are sent, so another site's page can neither read nor write.
- **Peer forwarding.** A mutation aimed at a paired peer's record (7.4, `host`) passes every check above on this host first, then travels to the peer over the peer link (section 10.3) and is authorized again there. The owner cookie never leaves this host, and the peer never accepts a cookie from elsewhere. A request from an `--allow-host` name still gets reads only, including of peers.
- **Same-user processes.** A process running as the owner's OS user that can read `~/.tiller` could still act as the owner. `authority.md` closes this for fleet roles (no access to `~/.tiller`, no localhost from the sandbox) and lists it as an alternate path with tests.
- **Versioning.** Every path is under `/api/v1`. The OpenAPI document (`contract/openapi.json`, utoipa, T2.8) is the board's only source of types, including the SSE event schemas.

### 7.2 Responses

Mutations return the CLI's response shape (6.1) plus a receipt:

```json
{"ok": true, "replayed": false, "events": [4182, 4183],
 "receipt": {"recorded_at": "2026-11-02T14:05:09.123Z", "outbox_id": "01JC...", "approval_id": null},
 "data": { "...": "the changed entity" }}
```

The receipt's three stages (point 36) come from records, not from the response: **recorded** is the commit (this response), **delivered** is `outbox.state = delivered` or `approval.state = executing`, **done** is `acked` or `succeeded`. The board follows the later stages over SSE.

| HTTP | CLI exit | When |
|---|---|---|
| 200 | 0 | done or replayed |
| 400 | 2 | bad body |
| 403 | 6 | forbidden, owner marker, missing or bad owner cookie |
| 404 | 3 | not found |
| 409 | 4 | stale `rev`; body has `current` |
| 421 | - | wrong `Host` |
| 422 | 5 | precondition refused |
| 502 | 6 | `peer_refused`: the owning peer refused a forwarded mutation's credential (revoked key, read scope, op not allowlisted, owner marker there); nothing written there |
| 503 | 7 | unavailable; nothing written. Also `peer_unreachable`: the owning peer's link is not `live`, so nothing was sent |
| 504 | 8 | `forward_uncertain`: the frame was sent and no result came within 10 s; nothing is held for a retry. The owner may retry from the control, which reuses the same `idempotency_key`, so the peer answers from its stored response or applies it once |

A peer's own refusal (404, 409, 422 and their bodies) is passed through unchanged, with `host` added to the error.

### 7.3 Reads

| Endpoint | Returns |
|---|---|
| `GET /api/v1/snapshot` | the boot snapshot (7.6) |
| `GET /api/v1/board?project=&filter=all\|active\|needs&host=` | every open card and recent done card with its `card_column` row, plus the `asked` rows |
| `GET /api/v1/cards/{id}` | card detail: card, column, runs with sessions and effective activity, holds, deps, attention, PRs, approvals, artifacts, reviews, obligations, freshness |
| `GET /api/v1/cards/{id}/timeline?before=<seq>` | the card's events, newest first, paged |
| `GET /api/v1/attention?state=open` | the global attention queue: question, evidence, recommendation, consequence, `still_current`, `stale` (`reason`, `subject`, `at`; null while current), `subject_session_id` and `subject_project_id`; and, separately, sessions in `waiting_permission` |
| `GET /api/v1/agents?host=` | orchestrators with nested workers: role, model, effective activity, `activity_observed_at`, last status line, herdr name, pane, workspace label |
| `GET /api/v1/runs/{id}` | run detail with recent events and a bounded pane tail |
| `GET /api/v1/machines` | section 9 |
| `GET /api/v1/quota` | section 8 |
| `GET /api/v1/health` | daemon version, schema version, each poller's last success and last error, tool versions (P43, point 37). No work data |
| `GET /api/v1/projects` | projects with delivery mode, effective delivery and capacity; stage, last reviewed commit and when, next review due, `review_overdue`, and the kind of an open review pass |
| `GET /api/v1/projects/{id}/delete` | what `project.delete` would remove from this host's database: rows per table, `unlinked` counts of the references between removed rows and rows that stay that are cleared, and `refused` (the fact that blocks the delete, or null). For `fleet` or a project another host owns, `removed` and `unlinked` are empty and `refused` says why. 404 for an unknown project. Never federated |
| `GET /api/v1/signals` | the Health page's counts (T3.5): one entry per Lisbon week for the last 8, oldest first, with cards landed and reported, merged PRs, attention items opened, median minutes to answer, reverts, escaped bugs, rework, lost work, duplicate dispatches and unauthorized actions; each week also carries `answered` (the sample under the median) and `interventions` (`correction`, `unplanned_escalation`, `rework`, `planned`, from the classifier in `state.md` Intervention); the per-project counts for the newest week with `avoidable` and `answered`; the newest week's `interventions` list, each with `kind`, `classified` and `relabeled`; machines live of total. Plain counts over `state.db` (`tiller-core` `Store::signals`); no Firstmate figure |
| `GET /api/v1/memory?scope=<scope>` | the memory view: each source's lines with open proposals inline and each proposal's budget figure (`memory.md`, section 6). Board only; no fleet role and no rendered prompt reads it. `scope` is a project id or `fleet` (the default); the answer lists every document, then the chosen one with its `HEAD` blob, whether its working tree is dirty, and its recent changes. Each suggestion carries `gate`, the holdout gate's verdict on keeping it (`learning-loop.md`, section 7): `ok`, and when not, `reason` and the `hint` command that clears it |
| `GET /api/v1/memory/revisions/{id}` | one revision behind a line's provenance hint: its source file text, the approval that applied it, and the sessions that loaded it (`memory.md`, section 7). Board only |

**Federated reads** (ADR 0003). `board`, `attention`, `agents`, `projects` and `machines` return this host's rows and every paired peer's cached rows together. Every row carries `host` (label) and `host_id`; a peer row also carries `as_of` (the peer's `server_time` of the frame it came from) and `link`. `?host=` narrows to one host. The single-entity reads (`cards/{id}`, `cards/{id}/timeline`, `runs/{id}`) take `?host=<label>`; absent means this host. For a peer they are sent over the link as a `query` frame and answered from the peer's `state.db`, with a 10 s timeout; an unreachable peer gets 503 `peer_unreachable` with the cached card row in `current`. `quota`, `health`, `signals` and `memory` stay local: each host's quota and memory are its own. A display id that appears on more than one host is shown host-qualified (`host-b · fleet-1`); an id input that matches on several hosts fails with 404 listing the candidates.

### 7.4 Mutations

Every mutation body carries `idempotency_key` (a ULID the board mints when the control renders and reuses on retry; a second click repeats nothing). Field edits also carry `rev`. Transitions (hold, close, cancel, reopen, answer, decide) do not: as `state.md` principle 5 says, they are guarded by the facts they need, so a stale click is refused by its precondition, and they carry the `rev` the board showed only so a conflict message can say what changed.

| Endpoint | Mutation | Body |
|---|---|---|
| `POST /api/v1/cards` | `ask.record` + `ask.resolve new` + `card.create`, then `outbox.enqueue` | `project`, `kind`, `title`, `body?`, `priority?` |
| `PATCH /api/v1/cards/{id}` | `card.edit` (reprioritize is an edit) | `rev`, changed fields |
| `POST /api/v1/cards/{id}/comment` | `outbox.enqueue` (`note`) | `text` |
| `POST /api/v1/cards/{id}/hold` / `DELETE .../hold` | `card.hold` / `hold.release` | `until`, `reason` |
| `POST /api/v1/cards/{id}/close` / `cancel` / `reopen` | `card.close` / `card.cancel` / `card.reopen` | `outcome?`, `reason` |
| `POST /api/v1/attention/{id}/answer` | `attention.answer` | `answer` (exact words). Picking an option that carries an owner action applies it in the same transaction (`state.md`, Attention); the board turns such an option off on a peer's item, because the forwarded owner has no project commands |
| `POST /api/v1/approvals/{id}/decide` | `approval.decide` | `decision` (`accept` or `reject`), `head_sha` the board showed; a mismatch is 422 with the new SHA, shown as the stale-approval refusal (T3.3) |
| `POST /api/v1/memory/proposals/decide` | `mem.decide` (Keep or Discard), one approval row per proposal (`memory.md`, section 6) | `decisions`: a list of `{proposal, decision, base_blob}`, where `base_blob` is the blob the board showed; a mismatch is 422, shown as stale. Keep runs the holdout gate first and is 422 with its reason and hint unless it passes. A proposal made against an older text is 422, "the text moved; re-base and run the holdout again", except a member kept in the same batch after earlier members of its change set made against the same text, which is applied onto the text they left, the order the holdout run applied them in. Discard is not gated |
| `POST /api/v1/memory/edits` | the owner's own edit: a proposal and its approval written in one act, decided by `owner`, then applied (`memory.md`, section 7) | `project`, `op` (`add`, `rewrite` or `remove`), `section`, `index?` (the line in the section), `text?`, `base_blob`; a mismatch is 422, shown as stale |
| `POST /api/v1/memory/changes/{proposal}/undo` | the reverse of an applied change as a new proposal, approved and applied in one act (`memory.md`, section 7, Undo); a conflict with a later edit is 422 | `base_blob` |
| `POST /api/v1/interventions/{id}/label` | `health.relabel`; local to this host | `label` (`correction`, `unplanned_escalation`, `rework`, `planned`, or null to give the item back to the classifier) |
| `POST /api/v1/projects/{id}/stage` | `project.stage`; to pre-launch the daemon reads the default-branch head from the project's checkout (`checkouts.<id>` in `config.toml`) | `rev`, `stage` |
| `POST /api/v1/attention/{id}/withdraw` | `attention.withdraw` with the reason `withdrawn from the board by the owner` | `idempotency_key`; the board's Withdraw on an item that no longer applies; 422 `refused` while it still applies. Not forwarded: a peer's item is withdrawn on that host |
| `POST /api/v1/projects/{id}/delete` | `project.delete` | `confirm` (the project id, typed by the owner; a mismatch is 400); 422 `refused` while something is live, for `fleet` and for a project another host owns |
| `POST /api/v1/agents/{session}/focus` | none; `herdr agent focus`, with the saved machine for a remote agent, then herdr is put in front of the screen (below). `data` holds `session_id`, `opened`, `terminal` and, when `opened` is `failed`, `problem` | - |

There is no endpoint for spawn, land, run end or cleanup. Those belong to agents and the daemon.

**Open in herdr.** The focus endpoint is the board's "Open in herdr" button and O key. Besides the focus, it puts herdr in front of the person at the daemon's own screen, using the terminal app named in the `[herdr]` table of `~/.tiller/config.toml`:

```toml
[herdr]
terminal = "Ghostty"        # a macOS app name; unset means focus only, and other systems ignore it
# terminal_exec = ["-e"]    # what the app takes before a command to run; "-e" when unset
```

The daemon first asks the local herdr server whether a client is attached in front. herdr has no read-only probe, so it calls `herdr terminal title clear` and reads the reason (`cleared` or `no_foreground_client`); the side effect hands the window title back to herdr's `ui.window_title`, which Tiller never changes. The probe has 2 seconds; a herdr that cannot answer it in time, or at all, is taken as attached. Then it focuses, and `opened` says what happened:

| Client attached | `terminal` set | What the daemon does | `opened` |
|---|---|---|---|
| yes | no | focus | `focused` |
| no | no | focus; no herdr window here shows it, and the board says to open the board on the machine you are at | `unseen` |
| yes | yes | focus, then `open -a <terminal>` | `raised` |
| no | yes | focus, then `open -na <terminal> --args <terminal_exec> <herdr> [--session <s>]`, wait about 10 seconds for the client to attach, then focus again | `launched` |
| either | yes | focus, but the app does not open or its client does not attach in time; `problem` says which | `failed` |

The `<herdr>` command is the daemon's own herdr (`spawn.herdr`, or `TILLER_HERDR`), its program resolved to a full path so the app finds it. A focus herdr refuses is 503 `unavailable` and nothing else happens; `failed` is a 200, because the focus did happen. One open runs at a time, so a second click while a client is attaching waits for it and then raises the app. `[herdr] terminal` drives macOS `open`, so a daemon on another system ignores it with a warning in its log. The probe always asks the local server, because the window on this machine is the one that matters. For a remote agent the focus goes to the saved machine, but herdr's window does not follow it there, so the board tells the owner to select that machine in herdr's sidebar; and `--machine` refuses the title call, so whether a client is attached on the remote machine cannot be asked at all. A daemon with no screen of its own (a server) has no client attached and no terminal configured, so every focus answers `unseen`. Test: `front::tests` in `tiller-herdr`, and `api::tests::focus_says_what_it_did_on_this_machine`.

**Forwarding** (ADR 0003). Every mutation body above may carry `host` (a peer label; absent means this host). With a peer `host`, the daemon checks the cookie and headers as for any mutation, refuses at once with 503 `peer_unreachable` when the link is not `live`, and otherwise sends the body as a signed `mutate` frame (section 10.3) and returns the peer's response with `host` added. Project slugs are unique only per host and `fleet` exists on every host, so `POST /api/v1/cards` takes `host` like the rest; the new-card picker sends the host of the project the owner chose. The forwarding allowlist is exactly: `POST /api/v1/cards`, `PATCH /api/v1/cards/{id}`, `POST .../comment`, `POST` and `DELETE .../hold`, `POST .../close`, `.../cancel` and `.../reopen`, `POST /api/v1/attention/{id}/answer` and `POST /api/v1/approvals/{id}/decide`. Not forwarded: `attention/{id}/withdraw`, because the item is cleared on the host that holds it; `memory/proposals/decide`, `memory/edits` and `memory/changes/{proposal}/undo`, because memory reads stay local (7.3), so the board host never shows what a decision would keep; `projects/{id}/stage` and `projects/{id}/delete`, because the forwarded owner has no project commands (`authority.md` section 3); and `agents/{session}/focus`, which already runs herdr against the peer's saved machine; and `interventions/{id}/label`, because the Health numbers are this host's own. A mutation off the list with a peer `host` is 502 `peer_refused`.

### 7.5 SSE stream

One stream: `GET /api/v1/events` (`text/event-stream`).

```
id: 4182
event: change
data: {"seq":4182,"at":"2026-11-02T14:05:09.123Z","kind":"attention.answered","txn":"attention.answer","entity":"attention","entity_id":"01JC...","rev_after":3,"card_id":"01JC...","entity_state":{...},"card":{...}}
```

- **Fields.** `seq` (also the SSE `id`), `kind`, `txn`, `entity`, `entity_id`, `rev_after` (null for append-only entities), `card_id`, `entity_state` (the entity's API shape when the event is sent, with its current `rev`, so the client can `setQueryData` without a fetch; a later write may already be included, and the client keeps whichever `rev` is higher) and, when `card_id` is set, `card` (the card's recomputed `card_column` row, since a column is derived and any of the card's facts can move it).
- **Order.** One SSE message per `events` row, in `seq` order. A mutation that writes several events sends them consecutively; the client applies each and ignores any whose `rev_after` is older than what it holds.
- **Source.** The daemon reads new `events` rows (it sees CLI and hook writes through `data_version` polling) and sends them. Every pushed change is an `events` row, so every change can be resumed. Cache refreshes write `pane.changed`, `host.changed`, `worktree.changed` and `quota.changed` events only on a change (`state.md`, `cache.refresh`), and observations write `observation.recorded` only when a session's verdict changes.
- **Resume.** The browser's `EventSource` sends `Last-Event-ID`. The daemon replays every row with `seq` greater than it, then streams live. If the gap is more than 5,000 rows, or the id is greater than the newest `seq` (a restored database), the daemon sends `event: reset` with `{"seq": <newest>}` and the client refetches the snapshot silently while the UI stays put. Because events are durable, this works across a daemon restart (T2.8 acceptance).
- **Keepalive.** A comment line every 15 s, and `retry: 1000`.
- **Peers** (section 10.2). A peer's change is sent as `event: peer` with `data: {"host","host_id","as_of","change":{<the change shape above>}}` and no SSE `id`, since a peer's `seq` is not this host's. A link state change is `event: link` with `{"host","link","as_of","stale_reason"}`. When the browser reconnects, the daemon sends `event: peer_reset` per peer, with the peer's current cached `queries`, after replaying this host's rows, so a peer's changes missed during the gap are never needed.
- **Time.** Staleness is derived from age, so it changes without an event. Each snapshot and each `reset` carries `server_time`; the client computes staleness from `activity_observed_at` with the clock offset, using the thresholds the snapshot carries.

### 7.6 Boot snapshot

The daemon renders `index.html` with the snapshot inlined:

```html
<script type="application/json" id="tiller-boot">{"v":1,"server_time":"...","last_event_seq":4181,"thresholds":{...},"queries":{"/api/v1/board?filter=all":{...},"/api/v1/attention?state=open":{...},"/api/v1/agents":{...},"/api/v1/machines":{...},"/api/v1/quota":{...},"/api/v1/projects":{...}}}</script>
```

- **Peers** (section 10). The snapshot adds `"peers":[{"host","host_id","link","as_of","server_time_offset_ms","scope","last_event_seq"}]`, and every federated query entry already includes the peer rows the daemon held when it rendered.
- Each `queries` entry is exactly the response of that GET, built by the same handler, keyed by its path and query string. The board's query option factories (T3.1) map those paths to query keys, so the snapshot never drifts from the API.
- The JSON is escaped for HTML (`<`, `>`, `&` and U+2028/2029 as `\u` escapes).
- All snapshot reads run in one SQLite read transaction, and `last_event_seq` is read in it, so the client's first SSE request (`Last-Event-ID: <last_event_seq>`) neither misses nor repeats an event.
- No email, account id or credential appears in it.

## 8. Quota feed

- **Run.** Every 5 minutes the daemon runs `quota-axi --provider <p> --full --json` for each provider in the config value `quota_providers` (default `claude`; other providers matter only once another harness has an adapter, section 10), with a 60 s timeout, one at a time (a run still going when the next is due is skipped, never overlapped). `StopFailure:rate_limit` and `Notification:quota_auto_resume_fired` trigger one early run, at most once a minute. It is a script; it costs no model tokens.
- **Parse by allowlist.** The output is deserialized into a struct that has only these fields: `schemaVersion`, `generatedAt`, and per provider `provider`, `state.status`, `state.refreshedAt`, and per window `id`, `label`, `kind`, `windowSeconds`, `percentRemaining`, `resetsAt`, `pace.status`, `pace.reservePercentPoints`, `pace.burnMultiple`, `pace.elapsedPercent`, `pace.projectedExhaustedAt`. The `account` and `accountKeys` blocks, `attempts`, `source` and every other field are never deserialized, so an account id or email cannot be stored, logged or sent even if quota-axi renames a field. Raw stdout is never logged.
- **One account per provider.** quota-axi reports the one credential it selected for each provider [T: one Claude entry on host B]. If it ever returns several entries for one provider, the feed keeps the first and records `fetch_error = multiple_accounts`; it never tells them apart by account.
- **Store.** One `quota_windows` row per window, in one `cache.refresh` transaction: `label` = `<provider>:<window id>` (for example `claude:seven_day`), `title` = the window's `label`, `kind` = its `kind` (`weekly`, `model`, `credits`, ...), and `percent_remaining`, `resets_at`, `pace`, `reserve`, `burn_multiple`, `elapsed_percent`, `projected_exhaustion_at` from the fields above. A failed run keeps the previous rows and sets `fetch_error` (a short code, never quota-axi's stderr) and leaves `fetched_at` as it was.
- **Expose.** `GET /api/v1/quota` and `tiller quota` return each window with `title`, `percent_remaining`, `resets_at` (and Lisbon text with the weekday), `pace`, `reserve`, `burn_multiple`, `elapsed_percent`, `projected_exhaustion_at`, `fetched_at`, `stale` (older than 15 minutes) and a `dot`:

  The dot reads the provider's window with `kind = weekly`. `pace` values seen [T]: `ahead`, `behind`, `unknown`; any other value counts as not ahead.

  | Dot | When (on the weekly window) |
  |---|---|
  | `red` | `percent_remaining` at or under the weekly floor (3%, AGENTS.md rule 8); a projected exhaustion before `resets_at` alone is not red |
  | `amber` | `pace = ahead` |
  | `green` | otherwise |
  | `grey` | stale or never fetched; the tooltip says since when |

- **Use.** The sidebar footer and Health read it; `tiller spawn` checks it live (P04, T4.2) and fails closed when it is stale (point 35). The per-account floor and the share per host are config owned by T4.2; the feed carries no account id, so the share is configured per host.

## 9. Machine summary

One row per host the board shows: this host, and every remote host it knows through herdr's saved machines or a peer (section 10).

| Field | Local host | Remote host |
|---|---|---|
| `label`, `kind`, `view_only` | `hosts`; `view_only` is derived, false | `hosts`; `view_only` is true for a herdr-only machine and a `read` pairing, false for an `act` pairing (ADR 0003) |
| `link`, `peer_scope` | null | for a paired peer: `live`, `lagging`, `down`, `refused` or `never` (section 10.2), and `read` or `act`; null for a herdr-only machine |
| `health` | `ok`, `degraded` (a poller failing, the quota feed stale, a hook error in the last hour) or `down` (the daemon itself cannot read `state.db`) | the peer's own `health` while the link is `live`; otherwise `stale`. A herdr-only machine: `ok`, or `stale` when `last_seen_at` is older than 2 minutes |
| `running_agents` | live sessions with an active run, plus orchestrators | from the peer's own row over the link; else live panes in `panes` whose `agent_status` is not `done` |
| `capacity`, `free_slots` | the `capacity` view | from the peer's own row over the link; null without one |
| `last_seen_at` | now, while the daemon runs | the last frame from the peer link, or `hosts.last_seen_at` from the herdr poller, whichever is newer |
| `stale_reason` | null | `herdr_link_down` (the saved machine does not answer), `host_asleep` (herdr reports the remote server suspended, I; T2.5 confirms what herdr exposes), `poll_failed` (an answer that did not parse), `never_seen`, `peer_link_down`, `peer_refused`, `peer_version` |

Exposed through `GET /api/v1/machines`, `tiller machines`, the snapshot and `host.changed` events. A click on a row filters Board and Agents by `host`.

## 10. Kept Firstmate capabilities: other harnesses, remote homes, config push

The owner kept D01 (secondmates and remote homes), D04 (other harness adapters) and D11 (config push to secondmate homes) and approved the other twelve drops (the decision reached this task on Monday 2026-11-02; `parity.md` records it since T1.6). This spec gives each a protocol home; CP2 decided the two open points below (C12, C13).

- **Other harnesses (D04).** `sessions.harness` (default `claude`) records which harness runs a session. `tiller hook --harness <h> <event>` is the entry point for another harness's hooks; each harness has an adapter table, like section 4.2, mapping its events onto the same `observations.event` and verdict vocabulary, with its own totality test. A harness with no adapter yet is observed through herdr and liveness only, as a reduced host is. Claude Code is the only adapter at cutover: no other harness gets an adapter before cutover (decided at CP2, C13). Which one comes first after cutover is a later decision, and `authority.md` G35 still refuses a role on a harness without a pre-tool hook transport and a sandbox equivalent.
- **Remote homes (D01).** A remote home is another host running its own Tiller, which owns its own projects (one owner host per project). CP2 kept it view only (C12). ADR 0003, accepted Tuesday 2026-11-03, reopens C12: the board host shows every paired peer's cards live and forwards the owner's mutations to the owning host. Sections 10.1 to 10.6 are that contract. Sending an ask to a peer's orchestrator from a session (`tiller ask note --host <peer> ...`) stays off: it would be a fleet route, and forwarding is owner-only.
- **Config push (D11).** Tiller's configuration (role files, settings fragments, hook registration) ships through the Nix flake and the home-manager module on every host, so a push is a dotfiles change plus a rebuild, not a file copy. `tiller config diff <peer>` compares this host's config hash with the peer's (`ssh <alias> tiller config hash`) and lists what differs. It is read-only.

### 10.1 Pairing

`tiller peer add <label> --ssh <alias> [--read-only]` on the board host, run by the owner in a terminal:

1. Refuses a session caller (`authority.md` G22), an alias that starts with `-`, and a label still paired to a different peer (`tiller peer remove` it first).
2. Generates an Ed25519 key pair. The private half goes to `~/.tiller/peers/<label>.key` (mode 0600, written atomically), never to `state.db` or a log.
3. Runs `ssh -t <alias> tiller peer accept --from <this label> --viewer-host-id <this meta.host_id> --pubkey <public half> --scope act|read`. `-t` gives the peer a terminal, so the peer sees its owner principal (`authority.md` section 3) and asks for confirmation; it refuses when its config has `federation.accept = false`. It writes `peer_keys` and prints its `host_id`, label and versions.
4. Commits `peer.pair` here (`hosts` row with `peer_scope` and `peer_host_id`) and config `peers.<label>.ssh`, then the daemon opens the link. A failure at step 3 deletes the key file; nothing else was written. A failure at step 4 also deletes the key file, which leaves the peer's new `peer_keys` row with no private half; running `tiller peer add` again replaces it, since a new accept from the same board host revokes the old row.

Adding a host stays one command. A peer whose database was replaced reports a new `host_id` and is `refused`; `tiller peer remove <label>` then `tiller peer add` pairs it again. Removing one is `tiller peer remove <label>` here and, to make the old key useless at once, `tiller peer revoke <label>` there.

### 10.2 Peer link, freshness and resume

One SSH session per paired peer, opened by the board host's daemon: `ssh -T -o BatchMode=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=2 <alias> tiller peer serve --from <label>`. Frames are JSON lines, protocol `tiller-peer/1`:

| Frame | Direction | Carries |
|---|---|---|
| `hello` | board host to peer | `v`, `from`, `key_id`, `viewer_host_id`, `nonce`, `ts`, `sig` (Ed25519 over all of them and the peer's expected `host_id`), `last_seq` (the peer `seq` the board host last applied, or null) |
| `welcome` / `refused` | peer | `host_id`, `label`, `schema_version`, `tiller_version`, `server_time`; or `reason` (`revoked`, `unknown_key`, `bad_sig`, `clock_skew`, `version`, `accept_off`) |
| `snapshot` | peer | `server_time`, `last_event_seq`, and `queries`: the peer's own `board?filter=all`, `attention?state=open`, `agents`, `projects` and its local `machines` row, built by the same handlers as 7.6, in one read transaction |
| `event` | peer | one `events` row in the SSE change shape (7.5), in `seq` order |
| `beat` | peer | `server_time`, `last_event_seq`, every 10 s |
| `refresh` | board host | nothing: asks for a fresh `snapshot` (Fresh, below) |
| `query` / `result` | board host / peer | `id`, an allowlisted single-entity GET (`cards/{id}`, `cards/{id}/timeline`, `runs/{id}`); `id`, `http_status`, `body` |
| `mutate` / `result` | board host / peer | section 10.3 |

- **Hello checks.** The peer answers `hello` only when config `federation.accept` is not `false`, the key row is unrevoked, the signature is valid and `ts` is within 120 s of its clock; otherwise `refused` with `accept_off`, `revoked` or `unknown_key`, `bad_sig` or `clock_skew`. It re-reads `federation.accept` and the key row on every `mutate` and `query`, and closes the link when either changes, so setting `federation.accept = false` cuts a live link at once.
- **Fetch.** After `welcome`, the peer sends `snapshot` when `last_seq` is null, older than 5,000 rows, or greater than its newest `seq` (a restored database); otherwise it replays every row after `last_seq`. Then it streams `event` frames as it sees new rows (`data_version` polling every 200 ms, like the daemon) and a `beat` every 10 s.
- **Fresh.** The board host applies each `event` to its in-memory copy at once and pushes it to browsers as `event: peer`. Derived columns are re-read: while events arrive, it asks for a fresh `snapshot` at most once every 2 s (coalesced), so a column a peer derives never drifts from the peer.
- **Link state.** `live`: a frame within the last 25 s. `lagging`: the last frame 25 s to 2 min ago. `down`: no frame for 2 min, or reconnect failing. `refused`: the last `hello` was refused, or the peer's `host_id` differs from `hosts.peer_host_id`. `never`: no `welcome` yet. Every change is an `event: link` and a `host.changed` event here.
- **Reconnect.** When the SSH child exits, the connection is refused, or the link reaches `down` (no frame for 2 min, which also catches a frozen `peer serve` behind a live TCP session), the daemon kills the SSH child and retries after 5 s, 15 s, 30 s, then every 60 s. `refused` waits for the owner (`tiller peer add` again) except `clock_skew` and `version`, which retry every 60 s.
- **Clock.** Each `welcome` and `beat` carries the peer's `server_time`; the board host keeps the offset and serves it in `peers[].server_time_offset_ms`, so a peer agent's activity staleness is computed against the peer's clock, not this one.
- **Isolation.** Each link runs in its own task with its own timeouts. Nothing on this host waits on it: no local write, no SSE push of a local event, no poller.

### 10.3 Forwarded mutations

1. The board sends a 7.4 mutation with `host = <peer label>`. This daemon checks the cookie and headers (7.1), that the link is `live` (else 503 `peer_unreachable`), that the pairing is `act` (else 502 `peer_refused`) and that the op is on the allowlist (7.4).
2. It sends `mutate{id, op, target, body, idempotency_key, ts, viewer_host_id, peer_host_id, key_id, sig}`. `op` names the 7.4 endpoint. The signature covers every other field.
3. The peer's `peer serve` verifies, in this order, which is the one canonical order (`authority.md` section 7 and G41 refer to it): config `federation.accept` is not `false`; the key row exists and is unrevoked; the signature; `ts` within 120 s of its clock; `peer_host_id` is its own; the row's scope is `act`; `op` is allowlisted; the owner marker (`authority.md` G26) still names `tiller`; and the target is in a project this host owns (G25). A failure is `result{http_status: 502, code: "peer_refused", reason}` and writes nothing.
4. It then runs the same core function its own board would run (protocol principle 1), with every precondition, `rev` and `head_sha` check of its own, with `actor = board@<viewer label>` and `via_peer` in the event payload. Its `idempotency_key` table makes a repeat return the stored response.
5. Its response comes back as `result`, and the board host returns it to the browser with `host` added. The peer's own SSE and its `event` frame carry the change, so the board updates the same way it does for a local write.
6. No result within 10 s is 504 `forward_uncertain`. The board shows "sent, not confirmed" and keeps the control's `idempotency_key`. Neither daemon holds or replays anything. When the link is `live` again the owner may retry from the control; the board host signs a new frame with a fresh `ts` and the same key, and the peer either returns the stored response or applies it once.

The peer never forwards a frame on. A board host that is also some other host's peer serves only its own records over its own link.

### 10.4 An unreachable peer

- The board host keeps working. Its own cards, agents and mutations do not wait on any link (10.2, Isolation).
- The peer keeps working. Its orchestrators, workers, delivery, landing and its own localhost board need nothing from the board host.
- The board host keeps the last snapshot and events it applied, in memory. A daemon restart drops them, and the peer's cards reappear when the link is `live` again.
- A mutation for the peer is refused at once with 503 `peer_unreachable` and nothing is queued (ADR 0003, owner choice 3). The board says so on the control; the owner retries when the link is back.
- No attention item is opened for a link: an outage is not a question for the owner. `GET /api/v1/health` lists each link's last success and last error; the Machines row shows the state; the existing 24 h Safety signal (`recovery.md` R05) still applies to a peer that stays down.

### 10.5 Staleness on the board

- Every card, attention item and agent from a peer carries a host chip with the peer's label. Local rows carry one too when any peer is paired.
- While the link is `live`, a peer's rows look like local rows.
- Otherwise its rows are dimmed and say "as of <time>" in Europe/Lisbon with the weekday, and the age ("as of Tue 14:02, 6 min ago"). Their controls are disabled, with the reason as the tooltip ("host-b is unreachable since Tue 14:02", "host-b refused the link: key revoked").
- A `read` pairing shows its rows live with controls disabled ("view only pairing").
- The new-card project picker groups projects by owner host and disables a group whose link is not `live`.

### 10.6 Security

`authority.md` owns it: section 3 (the forwarded owner principal), section 7 (Peer link), G25 and G41 to G44, and the alternate paths in section 10.

## 11. Checks

| Check | How it holds |
|---|---|
| Every command is idempotent or says why not | Every mutating command in 6.4 is `natural` or `keyed`. Four cases are not, each with its reason: a keyed command from a shell without `--key` (a script may send the same text twice on purpose; the minted key is printed for a deliberate retry), `tiller agent interrupt` (a second Escape reaches the agent's next action, so it refuses unless the session is `working`), `tiller land` after the merge call is sent (exit 8, resolved by reconcile, never retried blind), and `tiller board` (a used login link stops working, so a second call must mint a new one). Test: `cli::idempotency::same_key_twice_has_no_second_effect`, run over every keyed command |
| Every hook event maps to exactly one handler | Section 4.2 lists all 33 documented events plus `unknown_event`, and section 4.4 all 13 notification types plus the fallback. Tests: `observe::hooks::every_event_has_exactly_one_handler`, `observe::hooks::every_notification_type_has_one_verdict` |
| Interrupts, `StopFailure`, permission prompts and session end are detected | 4.2, 4.4, 4.5. Tests (T2.6): `observe::transcript::interrupt_marker_sets_idle`, `observe::hooks::stop_failure_sets_errored`, `observe::hooks::permission_prompt_sets_waiting_permission`, `observe::hooks::session_end_logout_ends_session` |
| A wake-up line never creates an ask | 3.2. Test: `agents::intake::wake_line_is_acked_not_recorded_as_ask` |
| A board-created card is not created twice | 2.2, 3.2. Test: `agents::intake::board_card_wake_does_not_create_second_card` |
| No hook stores a reply, a tool input or a secret | 4.1. Test: `observe::hooks::payload_allowlist_drops_reply_and_tool_fields` |
| No email or account id leaves the daemon | 8. Test: `daemon::quota::account_fields_never_deserialized` with a fixture that has them |
| SSE resumes with no missed event | 7.5. Test: `daemon::sse::resume_after_restart_replays_from_last_event_id` |
| A peer's cards appear live and resume after a gap | 10.2. Tests (F3): `daemon::peer::snapshot_then_events_fill_cache`, `daemon::peer::resume_from_last_seq_replays_gap`, `daemon::peer::gap_over_5000_sends_snapshot`, `daemon::peer::frozen_serve_reaches_down_and_reconnects` |
| A frozen peer never stalls this host | 10.2, 10.4. Test (F6): `e2e::peer::sigstop_peer_local_board_and_writes_unaffected` |
| A forwarded mutation is authorized on the owning host | 10.3. Tests (F4): `daemon::peer::mutate_bad_sig_refused`, `daemon::peer::mutate_replayed_ts_refused`, `daemon::peer::mutate_wrong_host_id_refused`, `daemon::peer::revoked_key_refused`, `daemon::peer::read_scope_cannot_mutate`, `daemon::peer::spawn_not_forwardable`, `daemon::peer::forwarded_precondition_failure_passes_through`, `daemon::peer::same_key_twice_applies_once` |
| A mutation for an unreachable peer writes nothing anywhere | 10.4. Test (F4): `daemon::peer::unreachable_returns_503_and_queues_nothing` |

## 12. Coverage

### Second opinion points this spec covers

| Point | How |
|---|---|
| 5 | `intake_source = transcript` and the herdr and liveness sources give the reduced behavior on a host where hooks are blocked |
| 11 | Each hook event and notification type handled on its own; `StopFailure` and transcript interrupts; `unknown` and stale explicit; hooks merged with herdr and liveness by `observed_at` |
| 12 | `tiller ask resolve` with four dispositions; tags for readability only; the Stop hook observes first and blocks once only after its false-block rate is measured; unresolved asks stay visible |
| 13 | Wake-up line grammar, store then type, ack through the hook or `tiller inbox`, target check, batching, re-ring |
| 18 | Data and control flows separated per host (ADR 0003, accepted): peers are read through a read-only link, and owner mutations are forwarded only on an allowlist, signed, and re-authorized on the owning host; the fleet ask route to a peer stays off |
| 35 | The quota feed fails closed when stale and carries no account id |
| 36 | Attention reads with evidence and `still_current`; permission prompts in their own list; receipts recorded, delivered, done |
| 37 | Each poller's last success in `/health`; one stream with freshness |
| 38 | AXI output, stable ids, exit codes, idempotency classes; no model calls for rendering, feeds or delivery |

Points 7 and 16 (rev conflicts, bound approvals) are `state.md`'s; this spec carries them to the CLI and API (409 with `current`, `--sha` and `head_sha` on approve).

### Firstmate contracts matched or departed from

| Contract | Matches | Departs, and why |
|---|---|---|
| `supervision-host` | Scripts report facts and the orchestrator reports judgement: a worker's status line is published by the daemon, not by the model remembering to write it | The wake is an outbox row with a typed `tiller inbox <id>` line and an ack, not a watcher note drained by a cursor |
| `supervision-host`, turn-end guard part (P26; Firstmate also has a separate `turnend-guard` note outside the plan's contract list) | The turn end is checked mechanically | Observe first; block once only after measurement, and never on workers. Idle orchestrators are woken by the outbox, not kept awake by a block |
| `agent-control` | Relaunch keeps the worktree and needs a note; resume keeps the Claude session | `--new-attempt` makes a new run row instead of a journal entry |
| `captain-hold-lifecycle` | Keyed answers: an exact replay is idempotent, a drifted answer is refused | Answers arrive through one endpoint and one command, both on the same mutation |
| `herdr-backend` | Typing into a pane through herdr, pane identity in metadata | Only one fixed-grammar line is ever typed; the payload never crosses the pane |
| `fleet-ledger` | Readers ignore unknown fields | Hook input structs ignore unknown fields the same way |

### Parity rows this spec gives a home

P02 (dispatch rules, 6.4), P04 (quota feed), P18 and P20 (status lines and one wake per batch), P22 (`card show`, `run show` with a bounded tail), P23 (re-ring, polling pickup), P24 (`agent relaunch`, `interrupt`), P25 (`agent resume`), P26 (idle orchestrator woken by the outbox), P27 (every prompt is an ask, including in worker panes), P36 (receipts and attention reads; the notify rule is T2.5's), P38 (`tiller snapshot` and the boot snapshot), P43 (`tiller doctor`, `/health`), P47 (review-page answers, 3.5), P60 (`tiller ask note`), P62 (machines, focus on the saved machine), D01, D04 and D11 (section 10).

## Decided at CP2

Monday 2026-11-02, Lisbon time. The owner's picks from `cp2-open.md`:

1. **Remote homes** (C12): stay read-only for v1; `tiller ask note --host <peer>` is not a control route (section 10). Reopened Tuesday 2026-11-03 by ADR 0003 (accepted Tuesday 2026-11-03): owner mutations forward to the owning host; the fleet route stays off.
2. **Other harnesses** (C13): none before cutover; Claude Code is the only adapter (section 10).
3. **The Stop hook switching rule** (C14, confirmed by S06): 200 orchestrator stops over 7 days with a false-block rate under 2%, as 2.5 defines it (the false-block test, its denominator and the owner-only switch).
4. **The owner cookie** (C15): survives daemon restarts and expires after 30 days (7.1).
5. **Output** (C16): TOON by default for agents, `--json` on request (6.1).
6. **Model and effort rules** (C04): a `[dispatch]` table in config, resolved in `run.start`, with an owner-only per-card override (6.4).
7. **Review-page answers** (C05): a v1 poller that steers the owning worker (3.5).
8. **`StopFailure`** (C07): never retried by the daemon (4.2; `recovery.md`, R12).
9. **Deploy watch** (C02 amended by S07): matched to the pushed commit and environment (4.6).
