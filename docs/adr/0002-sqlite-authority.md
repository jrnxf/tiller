# 0002. One SQLite database per host is the authority for workflow records

- **Status:** accepted
- **Date:** Sunday 2026-11-01 (Lisbon time)
- **Sources:** [`docs/design/plan-v2.md`](../design/plan-v2.md) (points 6, 7, 8, 24 and 35, and the fact-ownership table), [`docs/plan/build-plan.md`](../plan/build-plan.md) (T1.1, T2.1, T2.3)

## Context

The orchestrator, hooks, scripts and the board all write workflow records. The second opinion found that concurrent writes were unspecified (point 7): calling the same CLI does not by itself give locking, atomic updates or protection from stale edits. It also found that saved to disk is not the same as durable (point 8), and that one tracker can still have a database (point 6). Firstmate keeps its backlog in markdown files, which give none of these guarantees.

## Decision

Each host keeps one SQLite database, `state.db`, as the single authority for workflow records: asks, cards, runs, links, dispositions, approvals, reservations, the outbox, observations and proposals.

- **Transactions.** Every mutation goes through `tiller` in a `BEGIN IMMEDIATE` transaction, in WAL mode with a `busy_timeout`. The record change and its event row commit in the same transaction, so there is no half-written state.
- **Stale edits.** Editable records carry a `rev`. A stale board edit is refused with a readable conflict.
- **Hooks write directly.** `tiller hook <event>` writes observation rows straight to SQLite, so hooks still work while the daemon is down.
- **One owner per fact.** A fact-ownership table names the record that owns each fact. Everything else is a rebuildable projection: the board columns are derived, never stored; PR state stays owned by GitHub, panes by herdr, and worktrees by treehouse and git, each cached with a fetch time.
- **Memory stays in files.** Approved memory and rules stay in their canonical files under git (CLAUDE.md, AGENTS.md, memory dirs, the dotfiles source). Unapproved evidence and proposals live in `state.db` and are never loaded into a fleet role's context.
- **Durability.** Every record is classed as authoritative (`state.db`, memory files), rebuildable (projections, observations) or disposable (logs). A nightly `sqlite3 .backup` stays inside each host's boundary, schema migrations are versioned, and a restore drill is a day-one acceptance test.

## Consequences

- Concurrency and recovery become testable: property tests with concurrent writer processes, a write killed mid-transaction, and migrations run both ways (T2.1); a restore drill that recovers an acknowledged card with its pending decision (T2.3).
- Each host has its own authority. A project has exactly one owner host in v1, and host C's data never leaves host C.
- Raw transcripts and quotes stay in `state.db` or Claude Code's own files, never in git.
- Rust's rusqlite with bundled SQLite is the access layer, so the database ships inside the one `tiller` binary.
