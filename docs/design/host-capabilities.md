# Host capabilities

Draft, Sunday 2026-11-01, 21:25 Lisbon time (WET). Written from the capability probe (`probe/tiller-probe.sh`, task T0.3). It is decided at checkpoint CP1.

Hosts are named by role:

- **Host A**: the personal Mac.
- **Host B**: the remote Linux host.
- **Host C**: the work Mac. It is managed, and its data never leaves it.

Status: **host B is probed. Hosts A and C are pending**: each needs one run of the probe on that machine (see `probe/README.md`). Every row below marks what is still unknown, and the minimum common behavior is written so that it holds whatever hosts A and C report, with a named fallback where a host might fall short.

## Results

| Capability | Tiller needs it for | Host B | Host A | Host C |
|---|---|---|---|---|
| Managed settings | Knowing which hooks may run | None present: no managed settings file, no server-managed cache | pending | pending; managed, so expected to have some |
| Hooks in user, project, local, plugin and flag scope | `tiller hook <event>` observations (T2.6), intake (T4.4), guards (T4.3) | All five scopes fire | pending | pending |
| Hook events under the real login | The same | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `SessionEnd` all fire in project, local and flag scope | pending | pending |
| User or system service | The daemon (T2.8) | systemd `--user` running, lingering on, a transient unit ran | pending (launchd) | pending (launchd; MDM may restrict background items) |
| Localhost port | The board and its API (T2.8) | Bind and connect on 127.0.0.1 work | pending | pending |
| herdr CLI and `api snapshot` | The herdr adapter (T2.5) | herdr 0.9.3; `status` and `api snapshot` work; snapshot holds `workspaces`, `tabs`, `panes`, `layouts`, `agents` | pending | pending |
| Auto memory and `CLAUDE_CODE_DISABLE_AUTO_MEMORY` | Fleet roles load only approved memory (plan default) | Honored in a session launched from a herdr pane: with the variable set, the memory directory is not created and `MEMORY.md` does not load | pending | pending |
| `claude --append-system-prompt-file` | The worker launch form (plan default) | Works | pending | pending |
| `claude plugin eval --no-publish` | Skill-trigger evals (T4.6) | Works; the report stays local | pending | pending |
| `sqlite3` CLI | Manual inspection only | **Missing** from the host's PATH | pending | pending |
| Running a binary from the Nix store | Shipping `tiller` as a Nix package | Works | pending | pending; endpoint security may block unsigned binaries |
| `tiller` binary from the Nix store | The same | Not run: T0.1 has not landed. Rerun with `--tiller` once it does | pending | pending |
| Transcripts | The transcript tailer (T2.6) | `<config dir>/projects/<slug>/<session id>.jsonl`; subagents under `<slug>/<session id>/subagents/` | pending | pending |

## What the probe learned that the plan did not say

- **A user-level `disableAllHooks: true` turns off every hook**, in all five scopes, not only the user's own. A user-level `allowManagedHooksOnly` has no effect; only managed settings honor it.
- **`SessionStart` and `UserPromptSubmit` hooks run before Claude Code checks the login.** A session that fails to authenticate still produces those two observations, so they cannot be taken as proof that the session works.
- **The auto-memory directory is `<config dir>/projects/<slug>/memory/`**, where the slug is the physical working directory with every character that is not a letter or digit turned into `-`. Claude Code creates it at session start, even with `-p`. `MEMORY.md` in it loads into context. The `autoMemoryDirectory` setting moves it.
- **Every Claude Code start rewrites `.claude.json`** in the config dir, whoever runs it. A session plugin loaded with `--plugin-dir` also leaves a usage counter there. So a role launch should not use `--plugin-dir` for anything it does not need, and a read-only check of a host can never promise an unchanged `.claude.json`.
- **claude.ai connectors start in every session unless `--strict-mcp-config` is passed**, and they write logs under the user cache dir (`claude-cli-nodejs/<slug>/`). Fleet roles should launch with an explicit MCP config.
- **Claude Code's temp files follow `TMPDIR`** (`$TMPDIR/claude-<uid>/`), and so do `claude plugin eval` sandboxes.
- **Each session creates `<config dir>/session-env/<session id>/`**, and print-mode sessions keep a transcript unless `--no-session-persistence` is passed.
- **`--append-system-prompt-file` is not listed in `claude --help`**; only the `--bare` text names it. It works, but the launch form leans on a flag the help does not document.

## Minimum common behavior (draft)

Tiller can assume these on every host. Each row has a fallback for a host that falls short, so no host blocks the plan; if a host needs the fallback, that is a CP1 decision.

| Tiller assumes | Fallback if a host falls short |
|---|---|
| Project-scope hooks fire for `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop` and `SessionEnd`. Tiller installs its hooks in project or local scope, never user scope, so a user-level `disableAllHooks` is the only setting that turns them off | **Hooks blocked by managed policy** (`allowManagedHooksOnly` or a managed `disableAllHooks`): observations come only from the transcript tailer and herdr agent status; intake reads asks from the transcript; the PreToolUse backstop does not exist, so the guards are conventions plus permission deny rules on that host. This is the "reduced behavior" T6.4 refers to |
| A per-user service manager: systemd `--user` with lingering on Linux, a launchd agent in the GUI domain on macOS | **No user service** (MDM blocks background items): the daemon runs in a herdr pane started by the developer; reconcile covers its restarts |
| The daemon can bind 127.0.0.1 | **No localhost port**: the board is unavailable on that host; the CLI still works |
| `herdr status` and `herdr api snapshot` work for the local server | None. Without herdr, Tiller cannot run on that host |
| `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` stops auto memory for fleet roles launched from herdr panes | Launch roles with an `autoMemoryDirectory` pointing at an empty, read-only directory (untested) |
| `claude --append-system-prompt-file <role>.md` adds the role to the default system prompt | `--append-system-prompt "$(cat <role>.md)"` |
| `claude plugin eval --no-publish` runs locally | Evals run on host A or B only |
| A binary from the Nix store runs | None. Without it, Tiller cannot be installed on that host |
| SQLite through the bundled `rusqlite`; no `sqlite3` CLI on PATH | Not needed. T2.3's nightly backup uses SQLite's online backup API through `rusqlite`, not the `sqlite3 .backup` command; the dev shell provides the CLI for manual inspection |
| Transcripts at `<config dir>/projects/<slug>/<session id>.jsonl`, with the slug rule above | None needed; the tailer derives the path from the session's working directory and id |

## Open for CP1

1. Probe host A, and have the developer probe host C with the command in `probe/README.md` (about 1 minute each).
2. Rerun on any host with `--tiller` once T0.1 lands, to confirm the real build runs from the Nix store.
3. Decide whether a host that needs the "hooks blocked" fallback is acceptable, or blocks cutover on that host.
4. Note for T2.3: the backup should use the online backup API, since host B has no `sqlite3` CLI.
