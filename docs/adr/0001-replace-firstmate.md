# 0001. Replace Firstmate with an owned system

- **Status:** accepted
- **Date:** Sunday 2026-11-01 (Lisbon time)
- **Sources:** [`docs/design/plan.md`](../design/plan.md), [`docs/design/plan-v2.md`](../design/plan-v2.md), [`docs/plan/build-plan.md`](../plan/build-plan.md)

## Context

The developer runs a fleet of coding agents through Firstmate: one first mate, one interface, a markdown backlog and crewmates in herdr panes. In their words: "The more I work with [Firstmate] the more I wish I had full ownership and control of it. I'm up for creating my own system as long as it's feasible to create something comparable and my output will not be diminished."

What they want changes the centre of the system:

- one orchestrator per project, in place of one captain and one interface;
- a transactional state store, in place of a markdown backlog;
- a structured intake protocol, where every ask becomes a card;
- a memory model where nothing loads without approval.

Three options were weighed:

| Option | Ownership | Fits per-project and memory approval | Output risk at switch | Upkeep |
|---|---|---|---|---|
| Pinned Firstmate plus config | low | partly (secondmates) | none | low while pinned |
| Fork with a small patch set | high on paper | no: the centre gets rewritten | low at first | high: a large codebase the developer did not write |
| Owned replacement that reuses Firstmate's specs | full | yes | highest, handled by replay, rollback and fallback | what is built |

The second opinion proposed building on top of Firstmate first and deciding later. That would build every integration twice, once against Firstmate's internals and again against the new state model, and the developer had already decided against a slow roll-off.

## Decision

Build Tiller as an owned replacement, and cut over once per host, with no phase that builds on top of Firstmate.

- Firstmate is MIT-licensed. Its contract docs are the spec, read in place, and its scripts can be reused with the copyright notice kept.
- herdr, treehouse, `no-mistakes` and the axi tools stay. They are standalone tools the developer trusts.
- Firstmate is read-only during the build, except for steps the developer approves at a checkpoint.

## Consequences

Tiller must pass three tests before Firstmate is deleted:

1. **Ownership and control.** The developer owns every line and every contract.
2. **Comparable.** Every Firstmate capability in real use has a Tiller equivalent, or an approved drop. The parity matrix ([`docs/design/parity.md`](../design/parity.md)) is the checklist.
3. **Output and quality are not diminished.** Throughput, rework and escaped defects stay within tolerance of Firstmate's historical baseline, measured by the two-week check (T7.4).

The switch carries the most output risk of the three options. The plan answers it with:

- a capability probe on every host before the build;
- an offline replay of Firstmate's real history through the new orchestrator (M5);
- rollback triggers that go live at each flip: lost work, an unauthorized action, or a duplicate dispatch;
- hosts cut over one at a time: host A, then host B, then host C;
- Firstmate stopped but not deleted until checkpoint CP8, so falling back on a host means stopping Tiller, flipping the owner marker back and restarting Firstmate.
