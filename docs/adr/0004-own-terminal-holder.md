# 0004. Tiller owns its terminals

- **Status:** accepted (Wednesday 2026-11-11, Lisbon time), with the owner's direction choices below, made the same day. It was accepted in task H01, with the spec amendments. Each amended spec marks the rules that hold after the cutover with [H].
- **Date:** Wednesday 2026-11-11 (Lisbon time)
- **Reopens:** [`herdr-layout.md`](../design/herdr-layout.md) (herdr owns every agent pane), P23 (herdr as a home-manager service, now withdrawn), and the "herdr status works" minimum in [`host-capabilities.md`](../design/host-capabilities.md)
- **Sources:** [`docs/design/terminal-holder.md`](../design/terminal-holder.md), [`docs/spec/protocol.md`](../spec/protocol.md) (sections 3, 4.5, 7.4 and 10.7), [`docs/spec/recovery.md`](../spec/recovery.md) (R04, R18 and the liveness ladder), ADR 0003

## Context

herdr owns every agent pane. Tiller reaches it through `crates/herdr`: a CLI client, a poller, a layout repairer and a terminal mirror. That costs Tiller four things:

- Tiller has to work around herdr's ideas: workspaces, tabs, row tokens that a herdr restart loses (R18), pane ids that come back, and an agent status that reads `idle` through a whole turn.
- herdr gives no exit code, so the liveness ladder guesses death from a signal-0 probe.
- A remote host reached as a saved herdr machine has no live view and no input.
- A herdr upgrade kills every pane.

Three shapes were weighed:

| Option | Daemon restart keeps agents | Holder upgrade keeps agents | Exit codes | One protocol for board, CLI and peers |
|---|---|---|---|---|
| Keep herdr, and harden the client | yes | no | no | no |
| Keep herdr behind a trait, and add a Tiller holder as a second backend | yes | only on the new backend | only on the new backend | no: two code paths for years |
| A Tiller holder only, and delete herdr at cutover | yes | yes, by re-exec | yes | yes |

## Decision

Tiller ships its own terminal holder and drops herdr. The design is in [`terminal-holder.md`](../design/terminal-holder.md).

1. **One holder per host** (`tiller holder`), its own user service, owns every PTY, the child processes, a screen model per pane and the pane metadata. The daemon is a client of it, so a daemon restart touches no agent.
2. **A versioned, framed protocol on a `0600` unix socket**, with a peer uid check and no network listener.
3. **`alacritty_terminal` 0.26 is the screen model**, picked over `vt100` because it handles synchronized output, answers terminal queries, tracks the kitty keyboard mode and reflows. Both gave the same composer verdicts on today's fixtures.
4. **The holder upgrades live by re-exec**, after a preflight and a dry-run handover. It keeps its PID, its children and every PTY.
5. **The board is the main pane UI. `tiller attach` gives a real terminal**: one pane at a time with a switcher, and no sidebar.
6. **Owner notifications run a configured command**, which the owner points at their own desktop notification app.
7. **No herdr backend and no fallback trait.** `crates/herdr` and the herdr parts of the Nix module are deleted at cutover. Saved herdr machines go, and a remote host must be a paired peer.

## Choices the owner made

On Wednesday 2026-11-11 (Lisbon time), the owner chose:

- A separate holder that owns the PTYs, with the daemon as its client.
- No herdr backend and no fallback. Development cost does not matter.
- `tiller attach` layout A: one pane plus a switcher overlay.
- The recommended upgrade path: a live re-exec at any time, after the preflight passes.
- Owner notifications through the owner's desktop notification app.
- The recommended fixture plan: a builder records raw PTY fixtures once the owner gives the go.

## Consequences

- The cutover changes state in one way that cannot be undone without a backup restore: `hosts.herdr_machine` and the old `panes` rows are dropped.
- Tiller now maintains a terminal emulator integration and a repaint renderer. The `alacritty_terminal` version is pinned, and a bump must pass the fixture gate.
- One holder per host means one crash can end every pane on that host. Recovery row R21 resumes them, and the holder is kept small for this reason.
- The click handler and the forwarded env list of the owner's notification app, in the owner's dotfiles, must change at cutover, with the owner's approval.
