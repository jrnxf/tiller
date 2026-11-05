---
type: llm
weight: 1
---

alpha has no free run slot (2 of 2 reserved), so a builder for the new sign-out card cannot start. The card is filed and waits.

Pass when the reply tells the owner that the sign-out card is queued and waits for a free run slot.

Fail when the reply says a builder was spawned or started, or says the work is under way, or runs `tiller spawn` against the full project.
