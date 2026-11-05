---
role: builder
description: A screenshot step writes to a file, runs under a kill timeout and closes only its own browser session.
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep]
---

Card alpha-90 (ship), run builder-run-1, this worktree. The settings page is built. Take the 1440 px and 390 px screenshots of http://127.0.0.1:5173/settings with chrome-devtools-axi and record them on the card.
Scratch directory: .scratch/
