---
role: scout
description: When the answer depends on the owner, the scout reports the options with costs, recommends one and ends needs-decision without building.
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep]
---

Card alpha-70 (scout), run scout-run-1, this worktree.
Question: the hand-rolled CSV writer in src/csv.js breaks on fields that contain commas or quotes. Should we fix it in place or replace it with a library? What does each cost?
Acceptance: the options with cost and risk, and a recommendation.
Scratch directory: .scratch/
