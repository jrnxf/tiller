# Fleet roles

The five roles Tiller launches: one `orchestrator` per project and four worker roles (`scout`, `builder`, `reviewer`, `verifier`) that run on one card each. Each role is one prompt file and one settings fragment template here; `crates/agents/src/roles.rs` embeds them so a launch reads nothing from disk, and `tiller spawn` (T4.2) renders them per run.

`curator.md` and `curator.settings.json` are not a fleet role: the curator is the headless session `tiller retro run` launches for one full retro (`docs/design/learning-loop.md`, section 4). It has no run, no pane and no hooks, reads what fleet roles never see, and is embedded by `crates/agents/src/curator.rs`. It sits here so the evals cover it like the rest.

Specs: `docs/spec/authority.md` (role matrix, launch form, deny rules), `docs/spec/memory.md` (what a role loads), `docs/spec/protocol.md` (status lines, `tiller` commands), `docs/spec/recovery.md` (what the orchestrator does when a worker goes quiet, blocks or reports done). The role standard is `docs/design/plan.md` section 12, amended by `docs/design/plan-v2.md` points 24, 30 and 31.

## Files

| File | What it is |
|---|---|
| `<role>.md` | The role: YAML frontmatter (`name`, `description`, `model`, `effort`, `permissionMode`) and the prompt body. Spawn appends the body to the default system prompt with `--append-system-prompt-file`, followed by the approved memory it renders (`memory.md` L3). The frontmatter is metadata for spawn and never reaches the model |
| `<role>.settings.json` | The settings fragment template: permission mode, deny and allow rules, `claudeMdExcludes`, hooks, sandbox. Placeholders in angle brackets are filled per run (below). Passed with `--settings`, never written into a project's `.claude/` |
| `mcp.json` | The one MCP config every role launches with: no servers. Passed with `--strict-mcp-config --mcp-config` |
| `check-launch.sh` | The acceptance check: launches each role through the real launch form and verifies instructions, tools, model and memory at startup and on resume |

## The launch form

Every fleet role starts as (`authority.md` section 7):

```
claude --append-system-prompt-file <body> --settings <fragment> --permission-mode <mode> \
       --strict-mcp-config --mcp-config roles/mcp.json --model <model> --effort <effort>
```

with `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, `TILLER_SESSION_ID`, `TILLER_RUN_ID` and `TILLER_ROLE` in the environment, no `--plugin-dir`, no mods, and never `--agent`, which would replace the default system prompt. Workers run in `dontAsk`; the orchestrator runs in `auto`, so routine actions do not wait on the owner while the deny rules, the backstop and the sandbox keep the hard limits (it falls back to `default`, saying so, when Claude Code does not offer `auto`). A resume is the same command with `--resume <claude session id>`: the role body and the fragment are passed again, which is why the check verifies both halves.

## Defaults

| Role | Model | Effort | Mode | Write set | Ends with |
|---|---|---|---|---|---|
| orchestrator | `opus` | medium | `default` | its home, its scratch directory | never; it supervises until the owner clears it |
| scout | `opus` | high | `dontAsk` | its worktree, its scratch directory | a report artifact, `tiller done` (`reported`) |
| builder | `opus` | high | `dontAsk` | its worktree, its scratch directory | the merge (`landed`), or `tiller done --outcome failed\|abandoned` |
| reviewer | `opus` | high | `dontAsk` | its scratch directory only | a `code-review` review row, `tiller done` (`reported`) |
| verifier | `opus` | high | `dontAsk` | its worktree (build output), its scratch directory | a `spec-review` review row, `tiller done` (`reported`) |

Model and effort are the role's defaults; the `[dispatch]` rules in the config decide per run and the orchestrator may only raise them (`protocol.md` 6.4).

## Placeholders

| Placeholder | Filled with |
|---|---|
| `<tiller>` | the packaged `tiller` binary, by its absolute Nix store path |
| `<worktree>` | the session's working directory: the run's worktree for a worker; for the orchestrator the directory `tiller orchestrator start` (T4.2) launches it in, which the specs name twice (`recovery.md` R02: the project checkout; `authority.md` section 8 and P58: its home). The fragment covers either, since the checkout is denied for edits and the orchestrator's cwd gets the same `.env` and `.claude` sandbox entries as a worktree |
| `<scratch>` | the run's scratch directory, outside the repository and outside `~/.tiller` |
| `<home>`, `<pool>`, `<checkouts>` | the orchestrator's home, the treehouse pool root, the root of the project checkouts (orchestrator only) |
| `<default>` | the project's default branch |
| `<network_allow>` | a list entry replaced by the hosts the sandbox may reach (the forge, the registries the project's checks need) |
| `<no_mistakes_socket>` | a list entry replaced by the `no-mistakes` daemon socket, or removed when the project has none |

Rule paths are written `/<worktree>/**` in the template so the filled rule is `Edit(//home/user/pool/3/repo/**)`, the absolute form Claude Code's permission rules use. Sandbox entries use the plain path. `render_settings` in `crates/agents` does the filling and refuses a fragment that still carries a placeholder.

## What the fragments hold

- The common deny list of `authority.md` section 8, "Every fleet role", in order, then the role's extra deny rules and its allow rules from the "Per role" table. One addition: `Agent(isolation:*)`, because no fleet role uses native worktree isolation (treehouse owns worktrees) and a deny rule is cheaper than the backstop for the canonical form.
- `claudeMdExcludes` for every memory file kind (`**/CLAUDE.md`, `**/CLAUDE.local.md`, `**/AGENTS.md`, `**/.claude/rules/**`), so nothing loads natively and spawn renders the approved revisions instead (`memory.md` section 2). Spawn appends per-run paths when the launch check finds one these globs miss.
- `autoMemoryEnabled: false` and the `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` variable, `disableClaudeAiConnectors`, `remoteControlAtStartup: false`, and `disableRemoteControl` for workers (P68; the orchestrator keeps it available because the owner may want to reach it from the Claude app).
- Hooks: `tiller hook <event>` for every event `protocol.md` 4.2 maps to a handler, and `tiller guard pretool --role <role>` on `PreToolUse` for every tool.
- The sandbox: on, failing closed, with no unsandboxed retry; sandboxed Bash auto-approved for workers (they hold a bare `Bash` allow rule anyway) and not for the orchestrator, whose `default` mode must still prompt the owner for a command outside `tiller` and the `gh-axi` reads; the role's write set; `~/.tiller` and `~/.claude` unwritable and `~/.tiller` unreadable from the shell; `tiller` the only excluded command; the network allow list per project and no localhost binding.

What the fragments do not hold, by design: the Read deny globs (`**/*.pem`, `**/*.key`) have no sandbox equivalent because sandbox paths are paths, not globs, so the sandbox denies the concrete `.env` and `.envrc` of the worktree and the Read rules carry the patterns for the file tools; the forge credential stores are not denied anywhere, because denying them would stop `git push` and `gh-axi` inside the sandbox too (`authority.md` section 8, notes).

## Tests

`cargo nextest run -p tiller-agents` checks, from the embedded files: the frontmatter defaults, no `memory` field (`memory.md` section 9), the description as a trigger, the size and token budget (P42), the browser CLI and `gh-axi` in the builder's tools (P63, P64), the status contract in every worker, the common deny list and the per-role table of `authority.md` section 8, the launch guards, the hook registration, the MCP config, and the rendering of placeholders in both path syntaxes.

## The launch check

```
roles/check-launch.sh [--role NAME]... [--out DIR] [--model M] [--no-sandbox] [--keep]
```

For each role it builds a throwaway project with canaries that must not load (a `CLAUDE.md`, a `.claude/rules` file, an auto-memory `MEMORY.md` for that project), renders the body and the fragment, launches `claude -p` with the launch form above, and asks for: the first heading of the appended instructions, every canary visible, the tool list, the result of one `Write` and of one denied Bash command (`curl --version`). It then resumes the same session with `--resume` and asks again. The JSON result gives the model charged, the session id and the permission denials; a stub `tiller` logs which hook events the fragment fired. The report (`report.md`, `report.json`, raw outputs) goes to `--out`. It uses the real login and costs a few cents per role. Run it at T4.1, after each Claude Code upgrade, and before each host's cutover, next to T4.3's guard tests; a host whose sandbox cannot start runs it with `--no-sandbox` and says so in the report.

The guards themselves (the backstop, the alternate paths, the sandbox rows) are T4.3's tests, not this check's.
