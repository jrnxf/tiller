# Tiller herdr layout

Written Sunday 2026-11-01, late evening Lisbon time. It is based on a read-only pass over herdr 0.9.3 (installed), the herdr 0.9.5 docs, Firstmate's herdr backend and the dotfiles' herdr config. Tags: [T] tested locally, [D] documented, [I] inferred.

**Decision (the developer, Sunday 2026-11-01): Firstmate-style nested workspaces, not tabs.**

## The rule

Each project is laid out like this:

- **The project's orchestrator gets its own workspace**, labelled with the project id.
- **Each worker gets its own workspace**, labelled with a tree glyph and placed directly under its project's orchestrator, as Firstmate does today.
- **Every machine uses the same layout** under its own section of the sidebar.

```
LOCAL
● gamma
  ├ dot-7 build
  └ dot-8 review
● alpha
  ├ alpha-42 build
  ├ alpha-42 review
  └ alpha-43 scout
○ beta
▾ remote host (saved machine)
● beta
  └ beta-17 build
```

The indentation is drawn by the label, not by herdr. Workspaces are flat rows in herdr, so the glyph and the ordering carry the nesting.

## What changes from Firstmate's version

| Firstmate | Tiller | Why |
|---|---|---|
| `└ <task-id> · p:<22-char token>` | `├ alpha-42 build` / `└ alpha-43 scout` | Pane identity lives in state.db and in herdr pane metadata, not in the visible label, so names are no longer truncated. `├` for middle children and `└` for the last one, so the result reads as a tree. |
| One global `firstmate` workspace on top | One orchestrator workspace per project, each heading its own block | One orchestrator per project. |
| A Python socket helper moves workspaces | The daemon's herdr adapter calls the socket API directly (Bun speaks unix sockets) | One mechanism, no Python dependency. The Firstmate contract (herdr-backend.md, MIT) is the reference. |
| Remote secondmates in a separate `fm-remote` session | The same layout under the remote machine's sidebar section | Same on every machine. |

## Mechanics

| What | How |
|---|---|
| Orchestrator workspace | `herdr workspace create --label <project>`, made once per project per machine. Agent name `<project>-orch`. It stays open while the project exists |
| Worker workspace | `herdr workspace create --label "├ <card> <role>"`, then a socket `workspace.move` to put it directly after the last row in its project's block. Agent name `<card>-<role>`, e.g. `alpha-42-build`, so `herdr agent focus` targets it |
| Tree glyphs | When a block's membership changes, the daemon relabels it: the last child gets `└`, all others `├`. Ordering inside a block: by card, then by role (scout, build, review, verify) |
| Order upkeep | The reconcile loop checks each block's order and glyphs after every spawn, cleanup and herdr reconnect, and repairs drift with `workspace.move` and relabels. This is idempotent |
| Row tokens | `herdr pane report-metadata` sets `$project`, `$card`, `$role` and `$model`, shown through `[ui.sidebar.spaces] rows` with an `fg` colour per project. herdr loses these on a server restart [D], so the adapter reports them again after every reconnect |
| Agents panel | `agent_panel_sort = "spaces"`, so the panel follows the same block order |
| Jump | `herdr agent focus <card>-<role>` (the board's jump button calls this too) |
| Cleanup | Close the worker's pane only after its run is recorded as ended and its worktree has no unlanded work. herdr closes the emptied workspace. Then relabel the block |
| Orphans | A worker workspace whose run is ended in state.db and whose pane process is dead gets closed by reconcile. If the pane process is alive, it never gets closed; reconcile raises an attention item instead |
| Zero workers | The orchestrator workspace stands alone |
| Remote lanes | The same calls with `--machine <saved machine id>`. Use the id; the display name failed [T] |

## Known limits

- **There is no real collapse.** The rows are flat, so a busy project takes one sidebar row per worker. At 3 projects with 4 workers each, that is 15 rows. The board's wall view is the compact all-projects summary.
- **The orchestrator row shows only its own status**, not a roll-up of its workers, because there is no parent row. Each worker row shows its own status, and the board shows each project's total.
- **Socket API moves depend on the herdr version.** The adapter pins a tested herdr version, and the capability probe and CI check `workspace.move`. If a herdr update breaks it, the layout degrades to "correct labels, wrong order", which is still usable, and an attention item is raised.
- **Upgrade herdr** from 0.9.3 to 0.9.5 through the dotfiles before cutover.

## Considered and not chosen

- **Tabs per agent inside a project workspace.** The developer dislikes them.
- **Native herdr worktree groups** (`herdr worktree create|open`). They give real indentation and collapse, but treehouse owns worker worktrees, and `herdr worktree remove` runs `git worktree remove`, which could destroy unlanded work. Whether a collapsed parent rolls up its children's status is also unproven. Revisit if herdr adds parent/child workspaces that are not tied to git (upstream ask 1 below).

## Firstmate in the meantime

With Firstmate's current per-worker workspaces, the nesting already appears, but under one global first mate and with the token suffix. To get per-project blocks before cutover, run one secondmate per project. That fits the optional alpha secondmate trial and needs the developer's go, because it changes Firstmate's setup.

## Candidate herdr upstream asks (described here, not posted)

1. Parent/child workspaces that do not need git, with collapse and a documented child-to-parent status roll-up. This would replace the glyph labels with real nesting.
2. CLI commands for `workspace.move_block` and `pane focus --pane <id>` (related to discussion #5582).
3. Row tokens and agent views that survive a server restart.
4. A colour per workspace.
5. A project view across machines.

## A quick demo before building

About 3 minutes:
- create a separate herdr session (`--session tiller-demo`) with dummy panes in this layout for two projects;
- look at it;
- close the session.

It needs the developer's go, because it creates herdr state, even though that state is isolated from the live fleet.
