---
role: builder
description: When the default branch moves under a published direct-pr branch, the builder merges it in and reports done for the daemon to push, never rebases and never pushes itself.
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep]
---

tiller inbox 01JD7Q8ZK3M4N5P6R7S8T9V0WX
