# Capability probe

`tiller-probe.sh` finds out what a host lets Tiller do. It is read-only: everything it creates lives under one temp directory, and it proves that with a before and after diff. It writes `report.json` and `report.md`.

## Run it

Run it from inside a herdr pane. The `memory_live` check answers whether `CLAUDE_CODE_DISABLE_AUTO_MEMORY` is honored in a herdr-launched session only then; from a plain terminal it reports `warn` and says the herdr question is unanswered.

From a checkout:

```sh
bash probe/tiller-probe.sh --host-role personal-mac
```

On a machine without a checkout (the repo is private, so this needs a `gh` login that can read it):

```sh
gh api -H 'Accept: application/vnd.github.raw' 'repos/jrnxf/tiller/contents/probe/tiller-probe.sh?ref=main' | bash -s -- --host-role work-mac
```

If `gh` cannot read the repo there, copy the file over and run `bash tiller-probe.sh --host-role work-mac`.

It takes about 20 seconds and prints the paths of the two reports. Send back `report.md`; it is already redacted (the home directory shows as `~`, the user name in a home or managed-preferences path as `<user>`, the temp root as `$PROBE_ROOT`).

| Option | Effect |
|---|---|
| `--host-role ROLE` | Label for the report: `personal-mac`, `remote-linux` or `work-mac` |
| `--out DIR` | Write the reports and proof files to `DIR` instead of the temp root |
| `--offline` | Skip the live tier and the plugin eval: no login and no model calls on a host whose login is the normal one (see `hooks_offline` below) |
| `--no-service-trial` | Do not load a throwaway launchd agent or systemd unit |
| `--tiller PATH` | Also run this `tiller` binary (the T0.1 build) |
| `--nix-hello` | Build and run `nixpkgs#hello`. It adds to the Nix store, so it is off by default |
| `--keep-work` | Keep the scratch directory for debugging |

## What it checks

| Check | How |
|---|---|
| `managed_settings` | Reads only `allowManagedHooksOnly` and `disableAllHooks` from the managed settings files, the MDM plist on macOS, and any cached server-managed settings |
| `hooks_offline.*` | Runs `claude -p` in a throwaway project with `CLAUDE_CONFIG_DIR` inside the temp root and no provider credentials in the environment. If managed settings authenticate it anyway (an `env` block or `apiKeyHelper`), `authenticated` is not `false` in the report, and each run is capped at USD 0.02. Hooks in user, project, local, plugin (`--plugin-dir`) and flag (`--settings`) scope only touch marker files. Claude Code runs `SessionStart` and `UserPromptSubmit` hooks before it checks the login, so without a login this tier makes no model calls. Three cases: baseline, user-level `disableAllHooks`, and user-level `allowManagedHooksOnly` |
| `memory_offline` | Whether the default auto-memory directory is created, with and without `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` |
| `hooks_live` | The same marker hooks under the real login, so server-managed policy applies too. `--setting-sources project,local` keeps the user's own hooks and plugins from running. Covers `PreToolUse`, `PostToolUse`, `Stop` and `SessionEnd` |
| `append_system_prompt_file` | A canary passed with `--append-system-prompt-file` must reach the model |
| `memory_live` | A canary in `MEMORY.md` must reach the model, and must not with `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`. It passes only when the probe runs inside a herdr pane (`HERDR_PANE_ID` set); otherwise it warns. The report also lists which hooks fired in that second run. The memory directory is moved into the temp root with `autoMemoryDirectory` |
| `transcript_live` | Where the live session's transcript lands and which record types it holds; the probe then removes it |
| `transcripts` | Counts of existing transcripts and their layout; it never reads their contents |
| `plugin_eval` | `claude plugin eval --no-publish` on a one-case throwaway plugin with a free regex grader. Each case runs in the eval's own sandbox home; on the remote Linux host an strace audit saw no writes from it outside the temp root |
| `user_service` | Linux: `systemd-run --user --collect` runs a transient unit that touches a marker. macOS: a launchd agent loaded from the temp root with `launchctl bootstrap`, then `bootout` |
| `localhost_port` | Binds an ephemeral port on 127.0.0.1 and connects to it |
| `herdr` | `herdr status`, `herdr api snapshot` and `herdr machine list` only. It never creates or closes a pane or workspace |
| `sqlite3` | WAL mode, `busy_timeout` and `BEGIN IMMEDIATE` on a database in the temp root |
| `nix_store_binary`, `tiller_binary` | Runs a binary that resolves into `/nix/store`, and the `tiller` build when given |
| `proof` | See below |

The live tier makes two small haiku calls and the plugin eval one more, about USD 0.06 in all.

Interrupting a run (Ctrl-C, or a TERM or HUP signal) still unloads the throwaway launchd agent, removes what the live sessions created, and deletes the scratch directory and any unredacted proof files. No report is written then. The exit code is 130, 143 or 129 by signal.

## What it touches, and the proof

One carve-out, accepted by the owner: the live tier and the plugin eval run a real `claude` session under the real login, so Claude Code itself writes a few files outside the temp root (below). `--offline` skips them and leaves only the temp root.

Every file the probe writes is under its temp root, including Claude Code's own temp files (`TMPDIR` points there for every child `claude`). Live sessions use `--strict-mcp-config`, so no MCP server or connector starts.

Live sessions still make Claude Code write a few things under the real config dir: the session transcript and empty `session-env/<session id>` directories. Their names carry the run id or one of the run's random session ids, and the probe removes exactly those.

The `proof` check then holds when both of these are true:

- `manifest-before.txt` and `manifest-after.txt` match. They hash the Claude user settings, `CLAUDE.md`, keybindings and plugin registries, the managed settings, git and herdr config, LaunchAgents, systemd user units and the crontab, and count probe launchd jobs, probe systemd units and references to the run in `.claude.json`.
- `leftovers-after.txt` is empty: nothing under the Claude config or cache dir carries the run id or a run session id. `leftovers-pre-cleanup.txt` shows what the scan saw before cleanup, so an empty result is not a blind spot.

Not tracked: the files Claude Code rewrites on every start whoever runs it (the body of `.claude.json` and its backups, telemetry, plugin in-use markers), the system journal or unified log, and API usage.

On the remote Linux host a run under `strace -f` confirmed this independently: the only file outside the temp root that changed and still existed afterwards was `.claude.json`, with no reference to the run.

## Reports

- [Host B, remote Linux](reports/remote-linux.md) ([JSON](reports/remote-linux.json)), redacted as the probe writes it.
