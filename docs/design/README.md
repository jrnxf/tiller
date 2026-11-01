# Design docs

These are the design inputs Tiller is built from, imported by task T0.2 on Sunday 2026-11-01 (Lisbon time). They were scrubbed for the public-by-default rule: machine names became roles, local paths and private project names were removed, and the meaning was kept. `scripts/scrub-check.sh` checks the whole repo in CI.

## Hosts

Every doc names hosts by role:

- **Host A**: the personal Mac.
- **Host B**: the remote Linux host, reached from host A through herdr's saved machine.
- **Host C**: the work Mac. It is managed, and its data never leaves it.

Project names in examples (`alpha`, `beta`, `gamma`, card ids like `alpha-42`) are placeholders.

## Index

| Doc | What it is |
|---|---|
| [`plan.md`](plan.md) | The original plan, with Revisions 1 and 2 |
| [`second-opinion.md`](second-opinion.md) | An independent review of the original plan, in 43 points |
| [`plan-v2.md`](plan-v2.md) | The response to the second opinion: the current plan. When it and `plan.md` conflict, it wins |
| [`herdr-layout.md`](herdr-layout.md) | How orchestrators and workers are laid out in herdr |
| [`host-capabilities.md`](host-capabilities.md) | The minimum common behavior across hosts, from the capability probe (T0.3) |
| [`parity.md`](parity.md) | The Firstmate parity matrix: the checklist for "comparable" (T0.6) |
| [`../plan/build-plan.md`](../plan/build-plan.md) | The executable build plan, task by task |
| [`../plan/checklist.md`](../plan/checklist.md) | Build progress, ticked as work lands |
| [`../firstmate-export.md`](../firstmate-export.md) | The Firstmate export prompt: the import contract |
| [`../adr/`](../adr/) | Architecture decision records |

## Pending

Two inputs are Lavish design files that live only on host A. They are not imported yet:

- `design-basis.html`: the design basis (Linear tokens and a working mock).
- `mocks.html`: the UI mocks the developer approved at CP-UI.

Each must be copied here and scrubbed the same way before the board work (M3) starts.
