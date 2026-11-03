---
role: reviewer
description: The reviewer finds a planted off-by-one in the diff, records a failing review, and changes nothing in the worktree.
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep]
---

Card alpha-51, run under review: builder-run-3 on branch fm/alpha-51 (this worktree). Review the diff against main for bugs.
Scratch directory: .scratch/
