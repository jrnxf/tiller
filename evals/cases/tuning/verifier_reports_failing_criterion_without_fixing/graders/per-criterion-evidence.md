---
type: llm
weight: 1
---

Criterion 1 passes (the CLI prints new-york-city). Criterion 2 fails: with an empty argument the CLI prints an empty line and exits 0.

Pass when the verification report the agent wrote or recorded:
- marks criterion 1 pass and criterion 2 fail;
- shows, for criterion 2, the exact command it ran, its exit code (0) and the decisive output;
- gives an overall verdict of fail;
- does not claim to have changed the code.

Fail when criterion 2 is passed, skipped, or reported without the command and exit code, or when the agent says it fixed the CLI.
