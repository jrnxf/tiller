# Orchestrator chat: build plan

Written Mon 16 Nov 2026, about 22:00 Lisbon, at main `79aced66`, with a board mock kept outside the repo. PR 1 builds the thread API, the context use and `tiller thread`.

## Goal

The owner talks with an orchestrator in one thread. They see what they sent, whether it arrived, what the orchestrator did, its answer, and any decision it needs. The Inbox stops being the only place to read answers.

## What exists already

- Every owner message is an `asks` row. Every answer is a `replies` row (`tiller reply`). `AskDelivery.state` gives Sending, Delivered and Read.
- The `events` log records each mutation with `actor` (`session:<id>`), `kind` (`card.created`, `run.started`, `pr.linked`, `card.hold` ...), `card_id` and `payload`. This is the source for the activity steps and the tool blocks.
- `approvals` (merge, land_direct ...) and `attention` items are the decisions. `approve.tsx` and `answer.tsx` already act on them.
- SSE on `/api/v1/events`, peer reads with `?host=`, the ask editor with `#` card mentions (`ask-editor.tsx`, `ask-mentions.ts`, `ask-send.ts`).

## What does not exist (cut from v1)

- **Reasoning ("Thought for 6 seconds")**: Claude Code saves thinking blocks without text. Cut.
- **Token streaming**: replies arrive whole. The "Working" state and live steps come from events, not tokens.

## PR 1 - thread API and CLI (Rust), about 3-4 h

1. `GET /api/v1/projects/{id}/thread?before=<seq>&limit=50` returns one ordered list of items:
   - `ask`: text, sent time, delivery state, linked cards.
   - `activity`: the orchestrator session's events between an ask and its reply, grouped, each with kind, label, card or PR ref and payload. Duration = first to last event.
   - `reply`: body, card link, read state.
   - `decision`: open or resolved approvals and attention items for the project, with the owner's choice and time.
   - `checkpoint`: orchestrator session started or restarted.
2. Same endpoint with `?host=` for a peer, like `replies`.
3. Opening the thread marks its replies read (one call, not one per reply).
4. CLI parity: `tiller thread <project> [--json]`, and a row in `docs/design/cli-board-parity.md`.
5. Context use: the newest transcript `usage` row of the orchestrator session, stored on the session, returned as `context: {used, window}`.
6. Tests: insta snapshot of the JSON, one `World` integration test (ask, events, reply, approval in one thread), the pagination edge.

## PR 2 - board chat view, about 4-6 h

1. Route `/chat/$project`, plus the same view as a drawer over the board (one component, two shells).
2. `board/src/components/chat/`:
   - Sidebar: orchestrators grouped Needs you, Working, Idle, with letter avatar, last line, time, decision count, search with `/`.
   - Messages: owner turns as a contained block on the right with the receipt line; orchestrator turns flat, Copy and Reply on hover.
   - Activity: the collapsible step list with a connector line and card, PR and run refs ("Worked for 12 s").
   - Tool block: one per daemon mutation (`card.create`, `land` ...), status, then Command and Result.
   - Confirmation: approvals and attention questions inline, actions on the right. Reuse the approve and answer logic.
   - Queue chip in the input toolbar with a popover (running, up next, done).
   - Prompt input: reuse the ask editor (`#` cards), model chip read-only from the project's orchestrator settings, send, spinner, Stop (Stop = interrupt the orchestrator session).
   - Follow-up suggestions under the latest reply: a fixed set by project state in v1.
3. Live updates: invalidate the thread on SSE `ask.*`, `reply.*`, `card.*`, `run.*`, `attention.*`, `approval.*` for the project.
4. Open the chat from the sidebar project row, the Inbox reply, the ask tile and the command palette.
5. Board rules: actions on the right, no ghost buttons (`no-ghost-buttons.test.ts`), no words from other tools, board tokens only.
6. Tests: vitest for grouping, receipts, the confirmation flow and the queue. Screenshots in fixture mode, both themes, 1440 px and 390 px.

## PR 3 - Inbox follow-up, about 1 h (only after decision 2)

Replies read in the chat leave the Inbox. The Inbox keeps decisions and unread replies.

## Total

About 8-11 h of agent wall-clock time, CI included. That is more than my first estimate of 6-9 h, because the sidebar, queue, search and CLI parity are now in scope.

## Owner decisions (Tue 17 Nov 2026)

1. **Page and drawer:** both, from one component.
2. **Inbox after the chat ships:** decisions and unread replies only (PR 3).
3. **Context ring:** in PR 1. The daemon reads the newest assistant `usage` row of the orchestrator's Claude Code transcript (input + cache read + cache creation tokens), stores it on the session and returns it in the thread API with the model's window. About 1.5 h more. **Reasoning row:** cut. Claude Code writes thinking blocks with empty text (105 of 109 checked), so only the duration exists, and "Worked for N s" shows it.
4. **Suggestions:** a fixed set by project state in v1.

Builders run on Opus at high effort. Record the weekly quota before and after PR 1. Stop and ask before PR 2 if PR 1 used more than 12%.

## Delivery

- One branch, one worktree, one PR per phase. PR 2 builds on PR 1.
- Reviews and `no-mistakes` are paused (Tiller fate pause). Log each merged commit in issue #67.
- Merge only when the owner says so.
