# Memory

This is the contract for memory in tiller: what a fleet role loads at startup, where that text comes from, and how a note that an agent suggests reaches the owner's store. It builds on `state.md` and `schema.sql`.

Rewritten Wednesday 2026-11-18, Lisbon time, by [ADR 0005](../adr/0005-kb-holds-memory.md). It replaces the contract approved at CP2 on Monday 2026-11-02 (memory documents, revisions, evidence, proposals, change sets and the holdout gate). A contract change from here on needs a stop-and-ask.

## 1. Principles

1. **The owner writes notes in one place: kb.** kb is the owner's note store (`$HOME/.kb`, or `KB_HOME`). tiller reads it and never writes it, except through Keep (section 4).
2. **Nothing an agent wrote loads before the owner keeps it.** A note from a fleet session, the curator or the migration waits in the pending queue in `state.db`. Nothing reads the queue into a session, a prompt, a hook output or a card.
3. **A fleet role loads a rendered snapshot.** The daemon renders the role prompt (L3) at launch. Claude Code's own memory loading is off or excluded (L1, L2, L4, L5), so a file edited during a run changes nothing until the next launch renders it.
4. **Each host has one boundary.** `memory.kb_scopes` names the kb scopes a host may load. A work host lists only work scopes, so a personal note never reaches a fleet role there.
5. **Every load is recorded.** `session.memory_loaded` names each note that loaded, with its hash, and each note that was skipped, with the reason.

Fleet roles only. The owner's own interactive sessions load kb through its plugin hook, as the owner set it up.

## 2. What a fleet role loads

Every channel by which text can enter a fleet role's context, and the rule for it.

| # | Channel | Rule |
|---|---|---|
| L1 | Claude Code auto memory | Off. Every fleet launch sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` |
| L2 | Subagent persistent `memory:` | No role or agent file tiller launches has a `memory` field |
| L3 | The role prompt appended with `--append-system-prompt-file` | The daemon renders it per launch: the role file, then the kb notes that apply (section 3) |
| L4 | User `CLAUDE.md` and its `@` imports | Unchanged by ADR 0005. See `authority.md` |
| L5 | Project `CLAUDE.md`, `AGENTS.md`, `.claude/rules/*.md` and their imports | Unchanged by ADR 0005. They change only by PR. A builder may edit them in its worktree, and the edit rides its PR |
| L6 | Skills, agent definitions, plugins, output styles, hook output injected as context, MCP server instructions | Harness configuration, not memory text. It changes only through the dotfiles or the project repo and enters `sessions.config_hash`. Fleet roles launch with `--strict-mcp-config`. Every launch, the curator's included, names each installed developer plugin that has hooks (or might) as `false` under `enabledPlugins` (`crates/agents/src/spawn/plugins.rs`), so a plugin's `SessionStart` hook cannot print notes into a fleet role. A plugin with only skills or commands stays on (P67) |
| L7 | Task content: card bodies, attention answers, outbox bodies | Not memory. No pending row is rendered into any of them |

## 3. The kb loader

At each launch (`run.start`, `run.relaunch`, a promotion, the orchestrator launch) the daemon reads the kb store and renders the notes that apply into L3.

### Where the store is

`KB_HOME` in the daemon's environment, else `$HOME/.kb`. A note is one file, `notes/<scope>/<id>.md`, where `<scope>` is `global` or a repo id such as `github.com/example/app`. The file is YAML front matter between `---` lines, then the body. The loader reads these fields: `id`, `title`, `scope`, `kind`, `indexed`, `last_verified`, and the body. It ignores other fields. `trash/` and `archive/` are never read.

A fixture test pins this format (`kb_store_fixture_parses`). A note that does not parse is skipped with the reason `unreadable` and the file name, and the launch goes on.

### Which notes load

A note loads when all of these hold:

- its `scope` is one of the host's kb scopes for this project (below);
- its `kind` is not `open-work`;
- `indexed` is not `false`.

The host's kb scopes for a project come from `memory.kb_scopes` in `config.toml`, a list of strings. Each entry is `global`, `repo` (the project's own repo scope) or an exact repo id. The default is `["repo", "global"]`. The project's repo scope is its `repo` URL as kb writes it: host and path, with no scheme, no user and no `.git`, such as `github.com/example/app`. A project with no repo has no repo scope.

### Order and budget

Notes load in this order, and the first rule that differs decides:

1. scope: the project's repo scope, then exact repo ids in config order, then `global`;
2. kind: `convention`, `decision`, `gotcha`, `how-to`;
3. `last_verified`, newest first;
4. `id`.

The role's startup budget (7,500 tokens, P42) covers the role file and the notes. The loader adds notes in order while the total stays under the budget. A note that does not fit is skipped with the reason `over budget`, and later smaller notes may still fit. Tokens are counted with the same counter as the role file (`memory_budget.rs`).

### Render form

```
# Memory

These notes come from the owner's kb store. Follow them. To suggest a new note, run `tiller mem note`; it waits for the owner and loads nowhere until kept.

## <title>
<kind> · <scope> · kb:<id>

<body>
```

One `##` section per note, in load order. No notes renders nothing, not even the heading.

### When the store is missing

A host with no kb store, or a store tiller cannot read, loads no notes. The launch goes on. `session.memory_loaded` records `kb: "missing"` or `kb: "unreadable"`, and Health shows a warning until a launch reads the store again.

### The load record

`mem.load` writes `session_notes` (one row per note that loaded: `session_id`, `note_id`, `sha256` of the rendered section) and the memory part of `sessions.config_hash`, from the rendered L3 text. It emits `session.memory_loaded` with `trigger`, `kb` (`ok`, `missing` or `unreadable`), `notes` (each `id` and `sha256`), `skipped` (each `id` and `reason`: `over budget`, `unreadable`) and `hash` (SHA-256 of the whole rendered memory section). On a `SessionStart` for `resume`, `clear` or `compact`, the daemon copies the newest set for the session, because Claude Code reads the appended prompt file again.

## 4. The pending queue

Notes that agents suggest wait here for the owner.

### Table

```sql
CREATE TABLE memory_pending (
  id          TEXT PRIMARY KEY,               -- ULID
  project_id  TEXT NOT NULL REFERENCES projects(id),
  kind        TEXT NOT NULL CHECK (kind IN ('convention','decision','gotcha','how-to')),
  title       TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 120),
  body        TEXT NOT NULL CHECK (length(body) BETWEEN 1 AND 2000),
  source      TEXT NOT NULL CHECK (source IN ('fleet','curator','migration')),
  role        TEXT,                           -- the session's role, for source = fleet
  card_id     TEXT REFERENCES cards(id),
  run_id      TEXT REFERENCES runs(id),
  session_id  TEXT,
  retro_id    TEXT,
  created_at  TEXT NOT NULL,
  edited_at   TEXT,                           -- set when the owner edits it
  rev         INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX memory_pending_project ON memory_pending(project_id, created_at);
```

A kept or discarded row is deleted. Its event keeps the record.

### Limits

- `open-work` is not a pending kind: an agent cannot suggest a note that kb would hide from fleet roles anyway.
- A run adds at most 3 notes. A project holds at most 50 pending notes. Past either limit, `mem.note` is refused with the numbers.
- A note never names a scope. Keep writes it to the project's repo scope.

### Mutations

Each follows `state.md` section 8: one `BEGIN IMMEDIATE` transaction with its event rows.

| Mutation | Caller | Preconditions | Writes | Events |
|---|---|---|---|---|
| `mem.note` | fleet session (`tiller mem note`), the curator (retro finish), the migration | project exists; for a fleet session, the session is live and the project is its own; limits hold | `memory_pending` | `memory.noted` with `pending_id`, `project_id`, `source` |
| `mem.edit` | owner (board Edit, `tiller mem edit`) | row exists; `rev` matches | `title`, `body`, `kind`, `edited_at`, `rev` | `memory.edited` |
| `mem.keep` | owner (board Keep, `tiller mem keep`) | row exists; `rev` matches; the project has a repo scope; kb accepted the note (below) | deletes the row | `memory.kept` with `pending_id`, `note_id`, `scope` |
| `mem.discard` | owner (board Discard, `tiller mem discard`) | row exists; `rev` matches | deletes the row | `memory.discarded` with `pending_id` |
| `mem.load` | the launcher; the daemon on a `SessionStart` for `resume`, `clear` or `compact` | the render succeeded | `session_notes`, `sessions.config_hash` | `session.memory_loaded` (section 3) |

### How Keep writes to kb

Keep runs the kb CLI's `add --json` with this JSON on stdin: `{"title", "kind", "body", "scope": <the project's repo scope>, "confidence": "medium"}`. The body gets one last line: `Source: tiller <source>, card <card>, run <run>`, with the parts that exist.

The command is `memory.kb_command` in `config.toml` (a list: program, then arguments, such as `["kb"]`). When it is not set, the daemon finds the kb plugin in the Claude Code plugin list (`plugins/installed_plugins.json` under the Claude config home, the entry whose key starts with `kb@`) and runs `bun <installPath>/cli/main.ts`. With neither, Keep is refused with "tiller cannot find the kb command: set memory.kb_command", and Health shows the same warning.

The daemon runs `add` before the transaction and deletes the row only when `add` exits 0 with a JSON result that names an `id`. A result of `{"status": "exists", "id"}` is a success: the note is already there. Any other result refuses the Keep with kb's message, and the row stays.

### API

All routes are owner-only (`authority.md`). Memory is local: there is no `host` parameter.

| Route | Body | Result |
|---|---|---|
| `GET /api/v1/memory?project=<id>&role=<role>` | | `{project_id, role, kb: {state, home, scopes}, budget: {used, limit}, live: [{id, title, kind, scope, tokens, loaded, reason}], pending: [PendingNote]}`. `live` is what a launch of `role` (default `builder`) would load now, with skipped notes marked `loaded: false` and the reason. Without `project`, `live` is empty and `pending` lists every project |
| `PATCH /api/v1/memory/pending/{id}` | `{idempotency_key, rev, title?, body?, kind?}` | the row |
| `POST /api/v1/memory/pending/{id}/keep` | `{idempotency_key, rev}` | `{note_id, scope}` |
| `POST /api/v1/memory/pending/{id}/discard` | `{idempotency_key, rev}` | `{}` |

`PendingNote` is `{id, project_id, kind, title, body, source, role, card_id, run_id, retro_id, created_at, edited_at, rev}`.

### CLI

| Command | Who | Does |
|---|---|---|
| `tiller mem note --kind <kind> --title <title> [--body <text>]` | fleet session, curator | `mem.note`. The body comes from `--body` or stdin. The project, card and run come from the session |
| `tiller mem pending [--project <id>]` | owner | lists pending notes |
| `tiller mem edit <id> [--title] [--body] [--kind]` | owner | `mem.edit` |
| `tiller mem keep <id>` | owner | `mem.keep`, prints the kb note id |
| `tiller mem discard <id>` | owner | `mem.discard` |
| `tiller mem live [--project <id>] [--role <role>]` | owner | what a launch would load now, with the budget |

`pending`, `edit`, `keep`, `discard` and `live` are owner-only commands: the fleet guard denies them (`guard_denies_every_cli_owner_command`).

## 5. Retros and the curator

A full retro runs only for an incident: a card closed as abandoned, a run that failed an attempt, or a landed change that was reverted. Sampled retros and surprising-win retros are gone. A light retro (the record only, no curator) still closes every card.

The curator finishes a full retro with one of two results: `nothing`, or one note, which `tiller retro finish` adds to the pending queue with `source = curator` and the `retro_id`.

## 6. The migration from the old store

One migration runs once, on the first daemon start of a build with ADR 0005:

1. For each tiller memory document that has an approved revision, read the approved blob from the memory repo. Each non-empty line under a section heading becomes one pending row: `source = migration`, `title` = the line cut to 120 characters, `body` = the whole line. The kind comes from the section: "Watch out for" is `gotcha`; every other section is `convention`.
2. In the same transaction, drop the old memory tables (sources, revisions, memories, evidence, proposals, session loads, change sets and eval runs, with their link tables), and create `memory_pending` and `session_notes`.
3. A blob the repo cannot give is skipped and named in the migration's event. The migration does not stop for it.

The memory repo stays on disk. The migration does not delete it. The nightly backup from before the migration keeps the old rows.

Old config keys (`memory.remote`, `memory.review_days.*`, `learning.*`) are ignored. `tiller doctor` names each one that is still set.

## 7. Guarantees

| Guarantee | Label | Test |
|---|---|---|
| A pending note never reaches a fleet role: no rendered prompt, hook output, card body or fleet CLI output contains its text | enforced | a canary string in a pending row, asserted absent from every rendered surface |
| Only notes in the host's kb scopes load, and never `open-work` or an unindexed note | enforced | the loader's filter test |
| The render follows the order and stays under the budget, and names each note it skipped | enforced | the loader's order and budget tests |
| A missing or unreadable store loads nothing and does not stop the launch | enforced | the loader's missing-store test |
| The kb file format tiller reads is pinned | enforced | `kb_store_fixture_parses` |
| A fleet session cannot keep, edit, discard or list pending notes | enforced | `guard_denies_every_cli_owner_command`, the API authority test for the memory routes |
| A fleet session can add notes only to its own project, within the limits | enforced | the `mem.note` limit and project tests |
| Keep deletes the row only after kb accepted the note | enforced | the failed-add test |
| Each installed plugin with hooks is switched off in every launch | enforced | `spawn::tests::launch_switches_off_the_developers_plugins_with_hooks` |
| Auto memory is off in every fleet launch | enforced | the spawn test for the environment |
| No role file tiller launches has a `memory` field | enforced | the roles test |
| The migration exports before it drops, and runs once | enforced | the migration test |

What this does not cover: the owner's interactive sessions, and text the owner types into a fleet session. Both are the owner's own input.

## 8. History

The CP2 contract (Monday 2026-11-02) had memory documents in a tiller memory repo, revisions, per-line memories with validity and review intervals, evidence, proposals bound to a diff, a holdout eval and change sets measured over a window of cards. ADR 0005 removed them. The rule they served, that agent text loads nowhere before the owner approves it, holds through the pending queue. The git history of this file keeps the old text.
