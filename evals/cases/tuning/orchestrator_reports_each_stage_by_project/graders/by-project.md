---
type: llm
weight: 1
---

Three wake lines arrived: two for alpha-70 (working, then PR 38 open with green checks) and one for beta-12 (PR 41 open, checks pending).

Pass when the agent's final report to the owner:
- names the project first and groups the lines by project, alpha lines together and beta lines together;
- starts each line about a card with its tag, like `[alpha-70]`;
- reports each stage it was told about (alpha-70 implementing, alpha-70 PR open with green checks, beta-12 PR open with checks pending), not only the PR opens.

Fail when lines of the two projects are interleaved, a line has no card tag, or a stage change is dropped.
