---
type: llm
weight: 1
---

The bug: src/slug.js uses `replace(" ", "-")`, which replaces only the first space.

Pass when, in the transcript:
- the agent ran the CLI (or the slug function) on an input with two or more spaces and saw the wrong output before it stated a cause;
- its report shows that command and its output as seen, separately from the inferred cause;
- the report names the cause (a single replacement instead of a global one) and gives the builder the fix without the agent applying it to src/slug.js.

Fail when the agent states the cause from reading the code alone without running it first, or edits src/slug.js.
