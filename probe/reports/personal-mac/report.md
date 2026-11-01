# Tiller host probe: personal-mac

Run `tiller-probe-1nCzs0`, Sun 2026-11-01 23:16 WET (Lisbon). Probe version 1.

| Check | Status | Summary |
|---|---|---|
| `env` | info | macOS 26.6.2 arm64, claude 2.1.310 (Claude Code) |
| `managed_settings` | info | 0 file source(s); allowManagedHooksOnly=unset disableAllHooks=unset |
| `hooks_offline.baseline` | pass | scopes fired: user,project,local,plugin,flag |
| `hooks_offline.user-disableAllHooks` | info | scopes fired: none |
| `hooks_offline.user-allowManagedHooksOnly` | info | scopes fired: user,project,local,plugin,flag |
| `memory_offline` | pass | default memory dir created=true, with CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 created=false |
| `transcripts` | info | 1302 session transcripts across 240 project dirs |
| `hooks_live` | pass | project: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; local: SessionStart,UserPromptSubmit,PreToolUse,PostToolUse,Stop,SessionEnd; flag: SessionStart,Stop |
| `append_system_prompt_file` | pass | canary from --append-system-prompt-file seen=true |
| `memory_live` | warn | MEMORY.md loaded when enabled=true, when CLAUDE_CODE_DISABLE_AUTO_MEMORY=1=false; not run from a herdr pane, so the herdr-launched question is unanswered |
| `tool_read` | pass | Read tool used in -p mode=true |
| `transcript_live` | pass | transcript written at <config dir>/projects/<slug>/<session id>.jsonl |
| `plugin_eval` | pass | claude plugin eval --no-publish exit=0 score=1 |
| `user_service` | pass | launchd agent loaded from the temp dir ran=true |
| `localhost_port` | pass | bind and connect on 127.0.0.1: ok 64895 |
| `herdr` | pass | herdr 0.9.3; status exit=0; api snapshot exit=0 workspaces=4 panes=4 agents=4 protocol=22 |
| `sqlite3` | pass | sqlite3 3.51.0, WAL and BEGIN IMMEDIATE: wal 5000 42  |
| `nix_store_binary` | warn | no Nix store binary found on PATH among claude, herdr, jq |
| `tiller_binary` | skip | no --tiller given (T0.1 build not supplied) |
| `proof` | pass | before and after manifests match; nothing named after this run remains outside the temp root |

## Details

### `env`

- host_role: `personal-mac`
- platform: `macos`
- os: `macOS 26.6.2`
- arch: `arm64`
- bash: `5.3.9(1)-release`
- claude: `2.1.310 (Claude Code)`
- nix: `nix (Determinate Nix 3.13.0) 2.32.3`
- jq: `true`
- inside_herdr_pane: `false`
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
- project_dirs: `240`
- session_transcripts: `1302`
- subagent_transcripts: `46`
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
- launched_from_herdr_pane: `false`
- mechanism: `MEMORY.md in the memory dir is loaded into context at session start; autoMemoryDirectory relocates it`

### `tool_read`


### `transcript_live`

- path_pattern: `<config dir>/projects/<slug of cwd>/<session id>.jsonl`
- lines: `32`
- record_types: `api-request,api-request-blob,api-request-shape,assistant,atis-latch,attachment,cost-state,last-prompt,queue-operation,system,user`
- record_keys: `advisorModel,apiBlockIndex,atis,attachment,base,cacheControl,content,cwd,effort,entrypoint,gitBranch,hasOutput,hasUnknownModelCost,hash,hookAdditionalContext,hookCount,hookErrors,hookInfos,id,isSidechain,keep,lastPrompt,leafUuid,level,message,modelUsage,operation,params,parentUuid,perTurnEffort,permissionDecision,permissionMode,preventedContinuation,promptId,promptSource,querySource,rendered,renderedRole,requestDigest,requestId,requestRef,requestedModel,sessionId,shape,shapeHash,sourceToolAssistantUUID,startTime,stopReason,subtype,tail,thinkingDisplay,thinkingDurationMs,timestamp,toolUseID,toolUseResult,totalAPIDuration,totalAPIDurationWithoutRetries,totalCostUSD,totalDuration,totalLinesAdded,totalLinesRemoved,totalToolDuration,turnOrigin,turnPosition,type,userType,uuid,version,wireToolInputs`

### `plugin_eval`

- exit: `0`
- score: `1`
- report_local: `true`
- output_tail: `Wrote $PROBE_ROOT/work/eval.json Report: $PROBE_ROOT/work/eval-plugin/evals/results/2026-11-01T23-16-30-168Z/report.html `

### `user_service`

- domain_reachable: `true`
- bootstrap_exit: `0`
- agent_ran: `true`
- launchagents_writable: `true`
- managed_servicemanagement_policy: `false`
- output: ``

### `localhost_port`

- result: `ok 64895`
- method: `perl`

### `herdr`

- version: `herdr 0.9.3`
- status_exit: `0`
- snapshot_exit: `0`
- snapshot: `workspaces=4 panes=4 agents=4 protocol=22`
- machine_list_lines: `1`
- inside_herdr_pane: `false`
- note: `read-only: status, api snapshot and machine list only; no pane or workspace is created`

### `sqlite3`

- version: `3.51.0`
- path: `/usr/bin/sqlite3`
- result: `wal 5000 42 `

### `nix_store_binary`


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
~/.claude/projects/-private-var-folders-<tmp>-T-tiller-probe-<run>-work-live
~/.claude/projects/-private-var-folders-<tmp>-T-tiller-probe-<run>-work-live/c24dcf3f-1e52-415f-9f94-4ad99beffcd3.jsonl
~/.claude/session-env/c24dcf3f-1e52-415f-9f94-4ad99beffcd3
~/.claude/session-env/de1019cc-c6c1-46fd-b704-5ef27e603bdb
```

Leftovers named after this run (empty means none):

```

```
