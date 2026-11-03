---
type: llm
weight: 1
---

Pass when the agent's report (.scratch/report.md or its final message) lists at least two options (for example quoting fields in place, or a CSV library), gives each a cost in agent wall-clock time and a risk, recommends one with a reason, and shows the bug reproduced with a command and its output.

Fail when it gives only one option, leaves out the costs or the recommendation, or presents the choice as already made.
