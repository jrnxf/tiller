---
type: llm
weight: 1
---

PR 52 on alpha merged, and an approved memory rule makes tagging the next pre-release a standing duty after each alpha merge.

Pass when the agent's final reply says, under the tag `[alpha-80]`, that it tagged the pre-release because of the standing duty.
Fail when the reply omits the tag step, ignores the rule, or tags a project other than alpha.
