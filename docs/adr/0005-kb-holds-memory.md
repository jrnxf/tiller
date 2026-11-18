# 0005. kb holds the memory, and tiller keeps a gate for agent notes

- **Status:** accepted (Wednesday 2026-11-18, Lisbon time). The owner chose each decision below on the review page of the same day.
- **Date:** Wednesday 2026-11-18 (Lisbon time)
- **Reopens:** [`docs/spec/memory.md`](../spec/memory.md) as approved at CP2, and [`docs/design/learning-loop.md`](../design/learning-loop.md) (the holdout gate, change sets and the measuring window)
- **Sources:** kb, the owner's note store (`$HOME/.kb`, or `KB_HOME`), and the leak fix of the same day (`crates/agents/src/spawn/plugins.rs`)

## Context

The owner writes notes in two places. kb holds the notes of their own sessions: one markdown file per note, under git, loaded at session start by the kb plugin. tiller holds a second store for fleet roles: memory documents in `~/.tiller/memory`, with revisions, memories, evidence, proposals, change sets, a holdout eval and a measuring window in `state.db`.

The second store carried 1 memory document, 3 revisions and 2 proposals (both the owner's) after its first 4 days. It had no evidence rows, no eval runs and no change sets. It costs about 7,800 lines of Rust and 5,600 lines of board code.

The CP2 contract still holds one rule that matters: text that an unwatched agent wrote loads into no fleet role until the owner approves it. kb cannot hold that gate:

- A fleet pane may not write `$HOME/.kb`: its sandbox denies the path.
- A flag inside a kb note (author, status) is one that an agent with write access could set.
- kb commits with `git add -A` and short retries, so many writers at once can lose a write.

Also, fleet panes loaded the kb plugin's `SessionStart` hook through the developer's own settings, so kb notes already reached fleet roles with no check. The leak fix switches off each plugin with hooks in every tiller launch.

## Decision

1. **kb is where the owner writes.** tiller has no memory documents of its own. At each launch the daemon reads the kb store, read-only, and renders the notes that apply into the role prompt (L3).
2. **Which notes load.** A note loads when it is in `notes/` (not the trash), is `indexed`, is not `open-work`, and its scope is in the host's `memory.kb_scopes`. The default scopes are the project's repo scope and `global`. Notes load in a fixed order until the role's startup budget is full. The rest are named as skipped.
3. **Agent notes wait in a pending queue in `state.db`.** `tiller mem note` from a fleet session or the curator adds a row. Only the daemon writes the queue, and nothing in it loads anywhere. Keep, by the owner only, adds the note to kb with the kb CLI, in the project's repo scope. Discard deletes the row. Edit changes the text before Keep.
4. **Each host names its kb scopes.** `memory.kb_scopes` in `config.toml` is the boundary rule. A work host lists only work scopes. Keep never writes to `global`.
5. **Project files keep their path.** `CLAUDE.md`, `AGENTS.md` and `.claude/rules` change only by PR, as before. This ADR does not change L4, L5 or `claudeMdExcludes`.
6. **The holdout gate, change sets and the measuring window go.** The owner's Keep is the review. `evals/` stays for role prompt changes.
7. **Evidence rows, independence keys, validity and review intervals go.** A pending note keeps its source: role, card and run. After Keep, kb's own `stale`, `verify` and audit handle freshness.
8. **The curator runs only for incidents.** A full retro runs for an abandoned card, a failed attempt or a revert. It suggests one note at most, into the pending queue. Sampled and surprising-win retros go.
9. **The old store is exported, then dropped.** One migration copies each line of each approved tiller memory document into the pending queue, with source `migration`. Then it drops the memory, revision, evidence, proposal, change set and eval tables. The nightly backup keeps the old rows.

## Consequences

- One place to write and search notes. kb's freshness checks, audit and sync apply to fleet notes too.
- tiller depends on kb's file format for reads and on the kb CLI for Keep. A fixture test pins the format. A host without a kb store loads no kb notes, and Health says so.
- No automatic signal says when a kept note hurts. `session.memory_loaded` records which notes loaded and their hash, so a measuring window can come back later.
- Text the owner keeps is trusted. Keep is the only review, so the pending card shows the source run and card.
- Dropping the tables is a one-way door. The export runs first, in the same migration.

## Options weighed

| Option | One place to write | Agent text gated | Work boundary | Code kept |
|---|---|---|---|---|
| Keep both stores | no | yes | yes | all |
| kb only, agents write kb with a pending flag | yes | no: the flag can be forged, and the sandbox denies the write | weak | least |
| kb for the owner, a tiller queue for agents (chosen) | yes | yes | `memory.kb_scopes` | loader, queue, blob rendering |
