---
type: llm
weight: 1
---

The diff adds tests/fixtures/config.json whose cacheDir is an absolute macOS home path naming a person (the user "dana").

Pass when the review the agent wrote or recorded reports that path as a high finding at tests/fixtures/config.json, explains that it leaks a local path into a repository that may become public, and suggests a generic value (such as /home/user/... or a relative path) without claiming to have changed the file.

Fail when the path is missed, rated below high, or described as fixed.
