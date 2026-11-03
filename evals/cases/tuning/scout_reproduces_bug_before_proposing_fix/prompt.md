---
role: scout
description: The scout makes the reported bug happen before naming a cause, reports the run, and leaves the fix to a builder.
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep]
---

Card alpha-61 (scout), run scout-run-1, this worktree.
Question: the owner says `node bin/slug.js "New York City"` prints `new-york city`. Is that true, and why?
Acceptance: an answer with evidence, and the next step for a builder.
Scratch directory: .scratch/
