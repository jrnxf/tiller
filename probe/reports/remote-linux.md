# Tiller host probe: remote-linux

Run `tiller-probe-W9IUYe`, Sun 2026-11-01 21:35 WET (Lisbon). Probe version 1.

| Check | Status | Summary |
|---|---|---|
| `env` | info | Debian GNU/Linux 13 (trixie) x86_64, claude 2.1.289 (Claude Code) |
| `managed_settings` | info | 0 file source(s); allowManagedHooksOnly=unset disableAllHooks=unset |
| `hooks_offline.baseline` | pass | scopes fired: user,project,local,plugin,flag |
| `hooks_offline.user-disableAllHooks` | info | scopes fired: none |
| `hooks_offline.user-allowManagedHooksOnly` | info | scopes fired: user,project,local,plugin,flag |
| `memory_offline` | pass | default memory dir created=true, with CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 created=false |
| `transcripts` | info | 2000 session transcripts across 326 project dirs |
| `hooks_live` | pass | project: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; local: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; flag: SessionStart,Stop |
| `append_system_prompt_file` | pass | canary from --append-system-prompt-file seen=true |
| `memory_live` | pass | MEMORY.md loaded when enabled=true, when CLAUDE_CODE_DISABLE_AUTO_MEMORY=1=false |
| `tool_read` | pass | Read tool used in -p mode=true |
| `transcript_live` | pass | transcript written at <config dir>/projects/<slug>/<session id>.jsonl |
| `plugin_eval` | pass | claude plugin eval --no-publish exit=0 score=1 |
| `user_service` | pass | systemd --user running; transient unit ran=true; linger=yes |
| `localhost_port` | pass | bind and connect on 127.0.0.1: ok 34981 |
| `herdr` | pass | herdr 0.9.3; status exit=0; api snapshot exit=0 workspaces=8 panes=8 agents=8 protocol=22 |
| `sqlite3` | warn | sqlite3 CLI not on PATH; Tiller bundles SQLite through rusqlite, so only manual inspection needs it |
| `nix_store_binary` | pass | ran a Nix store binary (claude): 2.1.289 (Claude Code) |
| `tiller_binary` | skip | no --tiller given (T0.1 build not supplied) |
| `proof` | pass | before and after manifests match; nothing named after this run remains outside the temp root |

## Details

### `env`

- host_role: `remote-linux`
- platform: `linux`
- os: `Debian GNU/Linux 13 (trixie)`
- arch: `x86_64`
- bash: `5.2.37(1)-release`
- claude: `2.1.289 (Claude Code)`
- nix: `nix (Nix) 2.35.2`
- jq: `true`
- inside_herdr_pane: `true`
- claude_config_dir_set: `false`

### `managed_settings`

- file_sources: `0`
- sources: `none`
- server_managed_cache: `absent`
- allowManagedHooksOnly: `unset`
- disableAllHooks: `unset`

### `hooks_offline.baseline`

- scopes_fired: `user,project,local,plugin,flag`
- markers: `flag-SessionStart,flag-UserPromptSubmit,local-SessionStart,local-UserPromptSubmit,plugin-SessionStart,plugin-UserPromptSubmit,project-SessionStart,project-UserPromptSubmit,user-SessionStart,user-UserPromptSubmit`
- authenticated: `false`
- claude_output: `Not logged in · Please run /login `

### `hooks_offline.user-disableAllHooks`

- scopes_fired: `none`
- markers: `none`
- authenticated: `false`
- claude_output: `Not logged in · Please run /login `

### `hooks_offline.user-allowManagedHooksOnly`

- scopes_fired: `user,project,local,plugin,flag`
- markers: `flag-SessionStart,flag-UserPromptSubmit,local-SessionStart,local-UserPromptSubmit,plugin-SessionStart,plugin-UserPromptSubmit,project-SessionStart,project-UserPromptSubmit,user-SessionStart,user-UserPromptSubmit`
- authenticated: `false`
- claude_output: `Not logged in · Please run /login `

### `memory_offline`

- default_path: `<config dir>/projects/<slug of cwd>/memory/`
- slug_rule: `every non-alphanumeric character of the physical cwd becomes -`
- created_when_enabled: `true`
- created_when_disabled: `false`

### `transcripts`

- root: `~/.claude/projects`
- project_dirs: `326`
- session_transcripts: `2000`
- subagent_transcripts: `48`
- layout: `<root>/<slug>/<session id>.jsonl, subagents under <root>/<slug>/<session id>/subagents/`

### `hooks_live`

- fired: `project: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; local: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; flag: SessionStart,Stop`
- exit: `0`
- note: `user settings files are left out with --setting-sources project,local so the user's own hooks do not run; user and plugin scopes are covered offline (a session plugin would add a usage counter to .claude.json)`

### `append_system_prompt_file`

- exit: `0`

### `memory_live`

- loaded_enabled: `true`
- loaded_disabled: `false`
- exit_disabled_run: `0`
- hooks_fired_memory_disabled: `local-SessionEnd,local-SessionStart,local-Stop,local-UserPromptSubmit,project-SessionEnd,project-SessionStart,project-Stop,project-UserPromptSubmit`
- launched_from_herdr_pane: `true`
- mechanism: `MEMORY.md in the memory dir is loaded into context at session start; autoMemoryDirectory relocates it`

### `tool_read`


### `transcript_live`

- path_pattern: `<config dir>/projects/<slug of cwd>/<session id>.jsonl`
- lines: `28`
- record_types: `assistant,atis-latch,attachment,cost-state,last-prompt,queue-operation,system,user`
- record_keys: `apiBlockIndex,atis,attachment,content,cwd,entrypoint,gitBranch,hasOutput,hasUnknownModelCost,hookAdditionalContext,hookCount,hookErrors,hookInfos,isSidechain,lastPrompt,leafUuid,level,message,modelUsage,operation,parentUuid,perTurnEffort,permissionMode,preventedContinuation,promptId,promptSource,rendered,renderedRole,requestId,sessionId,sourceToolAssistantUUID,startTime,stopReason,subtype,thinkingDurationMs,timestamp,toolUseID,toolUseResult,totalAPIDuration,totalAPIDurationWithoutRetries,totalCostUSD,totalDuration,totalLinesAdded,totalLinesRemoved,totalToolDuration,turnOrigin,turnPosition,type,userType,uuid,version,wireToolInputs`

### `plugin_eval`

- exit: `0`
- score: `1`
- report_local: `true`
- output_tail: `Wrote $PROBE_ROOT/work/eval.json Report: $PROBE_ROOT/work/eval-plugin/evals/results/2026-11-01T21-35-44-843Z/report.html `

### `user_service`

- manager: `running`
- linger: `yes`
- transient_unit_ran: `true`
- exit: `0`
- output: ``

### `localhost_port`

- result: `ok 34981`
- method: `perl`

### `herdr`

- version: `herdr 0.9.3`
- status_exit: `0`
- snapshot_exit: `0`
- snapshot: `workspaces=8 panes=8 agents=8 protocol=22`
- machine_list_lines: `0`
- inside_herdr_pane: `true`
- note: `read-only: status, api snapshot and machine list only; no pane or workspace is created`

### `sqlite3`

- path: `missing`

### `nix_store_binary`

- binary: `claude`
- exit: `0`

### `tiller_binary`


### `proof`

- manifest_identical: `true`
- leftovers: `0`
- artifacts_seen_before_cleanup: `4`
- new_eval_sandboxes_in_system_tmp: `0`
- scope: `tracked: Claude user settings, CLAUDE.md, keybindings, plugin registries, managed settings, git and herdr config, LaunchAgents, systemd user units, crontab, probe launchd jobs and systemd units, references to this run in .claude.json; scanned: the Claude config and cache dirs for anything named after this run; not tracked: files Claude Code rewrites on every start (the body of .claude.json and its backups, telemetry, plugin in-use markers), the system journal or unified log, API usage`

## Proof

Manifest diff (empty means identical):

```

```

Named after this run before cleanup (shows the scan sees them):

```
~/.claude/projects/-tmp-tiller-probe-W9IUYe-work-live
~/.claude/projects/-tmp-tiller-probe-W9IUYe-work-live/56bbd415-b604-4d8b-8ce2-8f8130adf49b.jsonl
~/.claude/session-env/12cbdb2b-6d6c-4da1-aefa-23b82546d663
~/.claude/session-env/56bbd415-b604-4d8b-8ce2-8f8130adf49b
```

Leftovers named after this run (empty means none):

```

```
