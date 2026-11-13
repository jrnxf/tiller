# 0003. One federated board across hosts

- **Status:** accepted (Tuesday 2026-11-03, Lisbon time), with the owner's three recommended choices
- **Date:** Tuesday 2026-11-03 (Lisbon time)
- **Reopens:** CP2 decision C12 (remote homes stay read-only in v1), and the build plan's "Remote lanes" default
- **Sources:** [`docs/spec/state.md`](../spec/state.md) (principle 1), [`docs/spec/protocol.md`](../spec/protocol.md) (sections 7, 9 and 10), [`docs/spec/authority.md`](../spec/authority.md) (sections 3, 4, 7 and G25), [`docs/spec/recovery.md`](../spec/recovery.md) (R05), [`docs/spec/cp2-open.md`](../spec/cp2-open.md) (item 12), ADR 0002

## Context

The owner wants one Tiller board, behind one login, that shows all work on every host and lets them answer, steer and act on any card from it. That means the personal Mac (host A), the remote Linux host (host B) and any host added later.

The approved specs say the opposite. Each host has its own `state.db` and owns its own projects (ADR 0002). A peer added with `tiller peer add` appears only as a summary row on Machines (`protocol.md` section 10, D01). A peer's cards never enter this board (`state.md` principle 1), and remote lanes are read-only in v1 (CP2, C12; `authority.md` G25).

Two things from that design must stay. A host keeps working when another is down: the remote host froze twice in one week, and a frozen host must not stall the others. And there is still exactly one authority per fact: one owner host per project and one `state.db` per host.

Three shapes were weighed:

| Option | One board, all cards | Each host works alone | New attack surface | Fits ADR 0002 |
|---|---|---|---|---|
| Replicate every host's `state.db` into one (multi-writer, or a sync log) | yes | partly: a split brain needs merge rules | a sync channel that writes authoritative rows | no: two writers per fact |
| Move every project to one hub host | yes | no: the hub is a single point of failure | none new | yes, but defeats per-host ownership |
| Federate the board: each host stays the authority for its projects; the board host reads peers live and forwards the owner's mutations to the owning host, which re-checks them | yes | yes | one owner-only, signed forwarding path over SSH | yes |

## Decision

Federate the board. Keep one owner host per project and one `state.db` per host. The board host shows every paired peer's cards live, labelled by host, and routes the owner's mutations to the host that owns the record.

1. **Peers are read live, never stored.** The board host opens one SSH session per peer (`ssh <alias> tiller peer serve`), receives the peer's board snapshot and then its event stream, and keeps a disposable in-memory copy. A peer's cards never become rows in this host's `state.db` (`state.md` principle 1, amended).
2. **Mutations are forwarded, and authorized where the record lives.** A board mutation on a peer's record travels as a signed frame over the same SSH session. The owning host verifies the signature against a key it accepted at pairing, checks the op against a fixed allowlist, and then runs the same core function its own board would run, with every precondition, `rev` and head SHA check of its own. The board host's checks are only for a quick refusal; the owning host's checks are the gate.
3. **An unreachable peer degrades the view, never the host.** Its cards stay on the board as they were last seen, dimmed, with "as of" and the age. Their controls are disabled. A mutation aimed at it is refused at once with nothing written and nothing queued. No local write, SSE push, orchestrator or worker ever waits on a peer.
4. **The forwarding path is owner-only.** Pairing runs `tiller peer accept` on the peer as the owner principal, over `ssh -t`, which mints an Ed25519 key pair whose private half lives only under the board host's `~/.tiller`, which every fleet role is denied. No fleet role gains a cross-host route: orchestrators and workers still act only on their own host, and `tiller ask note --host` stays off.
5. **Adding a host stays one command.** `tiller peer add <label> --ssh <alias>` on the board host pairs and connects. `--read-only` pairs for viewing only.
6. **No new listening port.** Daemons still bind localhost only (AGENTS.md). The transport is the owner's existing SSH, with host-key checking, so no TLS, port or certificate is added.

The contracts are in `state.md` (principle 1, Host, Peer key, sections 8, 9 and 11), `protocol.md` (sections 7, 9 and 10), `authority.md` (sections 3, 4 and 7, section 10, G25 and G41 to G44) and `recovery.md` (R05).

## Choices the owner made

The owner approved the ADR on Tuesday 2026-11-03 (Lisbon time) and took the recommended option for each choice.

1. **Login and board host.** One board host, the personal Mac, whose localhost board already has the owner cookie; every other host is its peer, starting with the remote Linux host. The rejected alternatives were a mesh (each host also pairs back, one login per board you open) and a shared owner credential trusted by every daemon.
2. **The work Mac.** It stays out. It sets `federation.accept = false`, so a pairing to it is refused, and "work data never leaves the work Mac" holds as before. It still runs its own Tiller, separately; the setting only refuses pairing.
3. **A mutation for an unreachable peer.** Refused at once, with nothing queued. The rejected alternative was a short queue that replays on reconnect.

## Consequences

- C12 is reopened. Remote homes become actionable from the owner's board, and only from it. Point 18 of the second opinion ("specify data flows and control flows separately") is answered by the allowlist and the per-pairing scope, not by read-only.
- G25 changes meaning. A host still mutates only its own projects, and a peer's record changes only through a forwarded owner mutation that the owning host verified. Four guarantees are added (G41 to G44).
- Blast radius grows. Anyone who can read the board host's `~/.tiller` can act as the owner on every peer paired with `act` scope. That attacker is the deliberate adversary with the owner's shell, which `authority.md` section 1 already leaves out of scope. Fleet roles cannot read `~/.tiller` (G20), cannot reach a peer over SSH (G43), and cannot sign a frame. `--read-only` pairings and keeping the work Mac out limit the reach.
- Six build tasks (F1 to F6), about 13 to 17.5 agent-hours summed, about 10.5 to 14 hours on their own critical path. They run beside the replay gate and finish before the remote host's cutover (T6.3), so the personal Mac's cutover (T6.1) does not move. T6.3 grows by about 0.5 h for pairing and its live check. A new owner checkpoint, CP-F, comes before T6.3.
- The board's screens stay as they are. It gains a host chip, an "as of" age on a stale peer's cards, and disabled controls with a reason. CP4 can go ahead now.

## Amendment: peer agent parity (D20)

- **Date:** Friday 2026-11-13 (Lisbon time)
- **Decision:** the owner's decision D20: from the board, the owner can interrupt, answer prompts for, steer, stop, or do anything else for any peer's agent, not just the board host's own.
- **What changes:** the forwarded owner keeps the board's full row for agents and their records. The tail and the prompt read go as a `query` on the peer link. The prompt answer, interrupt, steer, stop, pause, unpause, resume, relaunch and an attention item's withdraw go as a forwarded `mutate`. Keys and Fit stay on the pane link (HR1). The routes, frames and error codes are in `protocol.md` 7.4 (Agent verbs on a peer) and 10.7, and the rows in `authority.md` section 3, section 4 and G46.
- **What stays:** the owner only, on the board host, with the `act` scope (the tail excepted), each action signed and checked again by the owning host. A `read` pairing and every fleet role stay refused. No host forwards to a third (G42). The owner-CLI-only powers (grants, away mode, pairing, landing, `tiller spawn`) and the host and project admin (config, projects, host edit, backups, pollers, memory decisions) stay local.
- **Build task:** HR2 in `docs/design/terminal-holder.md` section 13, after H11 and before H12.
