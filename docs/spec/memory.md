# Memory

This is the contract for memory in Tiller: what a fleet role loads at startup, where each piece of it lives, how a change to it is proposed, approved, applied, retired and audited, and where unapproved material is kept so it never loads. It builds on `state.md` and `schema.sql` (T1.1). The DDL for the tables it adds is in section 11; T2.1 adds it to the first migration with the rest of `schema.sql`.

Written Monday 2026-11-02, Lisbon time (WET). Task T1.5. Status: approved at CP2 (Monday 2026-11-02), with the owner's decisions from `cp2-open.md` applied. A contract change from here on needs a stop-and-ask.

## 1. Principles

1. **Nothing unapproved loads.** A fleet role (orchestrator, scout, builder, reviewer, verifier) loads only text whose exact revision is approved. Unapproved material (evidence, proposals, raw transcripts, external edits not yet reviewed) lives only in `state.db` or in a file revision marked `unreviewed`, and no launch path reads either (plan v2, point 24).
2. **Approved snapshot, never the working copy** (decided at CP2, S04). A fleet role loads no memory file natively. Every memory file it would load (L4 and L5) is excluded from Claude Code's own loading by the run fragment, and Tiller renders the approved revision of each one into the role prompt from git blobs by SHA (L3). A file edited in a worktree, on disk in the dotfiles, or merged without approval is a proposed change: it does not load into any fleet session, including the session that wrote it, until its revision is approved.
3. **Approved memory lives in files under git.** The file is the authority for the text (ADR 0002). `state.db` holds the metadata about each line (scope, evidence, validity, review status, provenance) and every unapproved record. No marker bytes go into memory files, so metadata costs no startup tokens.
4. **One canonical file per memory, edited at its source.** The source map (section 3) names, for every file that can load, the one file a change is written to and how the change reaches the loaded copy. A file installed by the dotfiles is never edited where it is installed (point 27).
5. **Approval binds the exact change.** A memory approval binds the SHA-256 of the diff and the git blob of the file it applies to. Either one moving makes the approval stale (point 16).
6. **Evidence, rationale, author and approval are separate fields.** None is copied from another, and none is invented. Imported memory says `legacy-import` and carries a rationale only when its source stated one verbatim (point 26).
7. **Validity, not age.** A temporary fact expires on its condition. A durable one comes up for review on its class's interval, or sooner when evidence contradicts it or the project changes under it, and is never deleted by age (point 25; section 5).
8. **Nothing crosses the personal and work boundary.** Each host has one boundary. Evidence, proposals and memory metadata never leave it, and a change built from one boundary's evidence never targets a file that loads in the other (point 25, point 18).

Fleet roles only. The owner's own interactive sessions keep native auto memory and load whatever the owner chooses; this spec governs them only where they share a file with the fleet (the source map), and there a change still needs approval before a fleet role loads it.

## 2. What a fleet role loads

Every channel by which text can enter a fleet role's context, and the rule that keeps unapproved text out of it. Section 9 turns each row into a check.

| # | Channel | Rule |
|---|---|---|
| L1 | Claude Code auto memory (`<config dir>/projects/<slug>/memory/MEMORY.md`) | Off. Every fleet launch sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` (host-capabilities: honored from a herdr pane on host B; host A pending the herdr-launched case). Fallback on a host that does not honor it: `autoMemoryDirectory` pointing at an empty read-only directory, which the launch check verifies is still empty |
| L2 | Subagent persistent `memory:` | No role or agent file Tiller launches has a `memory` field (T4.1). Reviewers get approved memory as input and have no memory tool (point 31) |
| L3 | The role prompt appended with `--append-system-prompt-file` | Tiller renders it per launch: the role file, the Tiller memory documents for the session's project and `fleet`, and the approved revision of every L4 and L5 file the role would load, each read from its approved git blob by SHA (`git cat-file blob`), never from a working tree or an installed copy. `@` imports are resolved from approved blobs too. Nested files and path-scoped rules, which Claude Code would load lazily, are rendered under a heading naming the directory or path pattern they apply to; their tokens count toward the role's startup budget (section 6). Uncommitted or unapproved edits cannot reach it |
| L4 | User `CLAUDE.md` and its `@` imports, installed by the dotfiles | Not loaded natively: the run fragment lists each one in `claudeMdExcludes`, and its approved revision is rendered into L3. An installed copy that differs from every approved revision is an `unreviewed` revision; it does not load until the owner accepts it with one click (CP2, C21). If T4.3 finds a user-scope file cannot be excluded, L4 falls back to the launch check refusing a drifted installed copy (section 9), which is the pre-CP2 behavior |
| L5 | Project `CLAUDE.md`, `AGENTS.md`, `CLAUDE.local.md`, ancestors, nested files loaded lazily, `.claude/rules/*.md`, and every `@` import they reach | Not loaded natively: the run fragment lists every such path in `claudeMdExcludes` (`**/CLAUDE.md`, `**/CLAUDE.local.md`, `**/.claude/rules/**`; `AGENTS.md` loads only through an import), and the approved revision of each source, read from the project's canonical blob, is rendered into L3. A builder may edit these files in its worktree (CP2, C01): the edit is a proposed change that rides its PR with the memory-change flag (section 7) and never loads, not even into the session that made it. Managed-policy `CLAUDE.md` cannot be excluded; on a host that has one it is a `foreign` source, recorded and checked like L4 |
| L6 | Skills, agent definitions, plugins, output styles, hook output injected as context, MCP server instructions | Harness configuration, not memory text. It changes only through the dotfiles or the project repo, enters `sessions.config_hash`, and is checked by T4.1's launch check of effective instructions. Fleet roles launch with `--strict-mcp-config`. `tiller hook` never emits evidence or proposal text as context |
| L7 | Task content: card bodies, attention answers, outbox bodies through `tiller inbox` | Not memory. The protocol (T1.3) never renders a row of `evidence` or `proposals` into any of them, and the API serves those tables only to the owner's board (T1.4) |

How L3 renders the Tiller documents (the order and the budget are not set elsewhere in this spec, so these are the simplest rules that fit it):

- What loads: each live `tiller_doc` source whose boundary is `meta.boundary`, whose project is `fleet` or the session's project, and whose `roles` name the session's role. Its current approved revision is the newest `approved` row by `recorded_at`. A personal document never loads into a launch on a work host, and a work document never loads on a personal host.
- Order: `fleet` first, then the project, then by `canonical_path`. The documents follow the role file and come before the brief, under one `# Memory` heading with a `## <canonical_path>` heading per document. This is the form the holdout eval measures a candidate in (`learning-loop.md`), so the eval measures what launches.
- Text: the blob, read with `git cat-file blob` from the memory repo. A blob the repo cannot give, or text that is not UTF-8, refuses the launch. An empty document renders nothing and is not recorded as loaded.
- Budget: the launch renders every approved document whole and never cuts one. The budget is held at approval, where `mem.decide` refuses a change that would put a role over it (section 6). That refusal is not enforced yet: nothing computes the count (section 9).
- Built so far (U15): the Tiller documents only. The L4 and L5 sources and the refusals of section 9, steps 1 to 3 and 5, are not built yet. Until they are, a Tiller document with no approved revision renders nothing instead of refusing the launch (section 9, step 4) [I: `spawn::tests::spawn_renders_the_approved_memory_and_records_the_load`, `spawn::tests::spawn_refuses_an_approved_blob_it_cannot_read`, the CLI test `memory_load`].

Firstmate departure: Firstmate's notes tier let agents write learnings that loaded at the next start, reviewed afterwards. Tiller has no notes tier. An agent's "remember this" becomes evidence or a proposal (section 6), and loads only after approval, batched so the cost stays low (point 24).

## 3. Source map

`memory_sources`: one row per file that can load into a fleet role on this host, with the file a change is written to and how the change reaches the loaded copy.

- `id`, `project_id` (`fleet` for host-wide sources), `boundary` (`personal` or `work`).
- `kind`: `tiller_doc`, `dotfiles`, `project_file`, `foreign`.
- `loaded_path`: where Claude Code reads it, as a pattern relative to the project root (`AGENTS.md`, `.claude/rules/*.md`, `**/CLAUDE.md`) or to the home directory (`~/.claude/CLAUDE.md`). Stored in `state.db` only, never committed.
- `canonical_repo`, `canonical_path`: the git checkout and path a change is written to.
- `apply_method`: `tiller_commit`, `dotfiles_rebuild`, `project_delivery`, `none`.
- `load_mode`: `rendered` (L3), `eager`, `lazy` (nested files and path-scoped rules, loaded when the role reads a matching file).
- `roles`: JSON list of the roles that load it; a Tiller document can be scoped to roles.
- `rebuild_command`: for `dotfiles_rebuild`, the host's configured rebuild (for example `home-manager switch --flake <dotfiles>#<host>`); null means the rebuild waits for the owner.
- `added_at`, `retired_at`, `rev`.

The four kinds:

| Kind | Example loaded copy | Canonical file | Apply | Notes |
|---|---|---|---|---|
| `tiller_doc` | rendered into the role prompt (L3) | `~/.tiller/memory/fleet.md` and `~/.tiller/memory/projects/<id>.md` in the host's memory repo | `tiller_commit`: commit to the memory repo | The memory view's per-project documents, with plain sections ("How we work here", "Watch out for", "Preferences"), one line per entry. The memory repo is a git repo that never leaves the host's boundary. A personal host pushes it to one private remote, the config value `memory.remote`, which the owner sets at that host's cutover (decided at CP2, C24); the scrub hook stays on for every commit. A work host never pushes it off the machine |
| `dotfiles` | `~/.claude/CLAUDE.md`, user `AGENTS.md`, dotfiles skills and roles | the file in the dotfiles repo that home-manager installs from | `dotfiles_rebuild`: commit to the dotfiles repo, then run `rebuild_command` | Never edited in the installed location or the Nix store. Applied means the installed bytes equal the committed blob, checked after the rebuild |
| `project_file` | `AGENTS.md`, `CLAUDE.md`, `.claude/rules/*.md` in a worktree | the same path on the project's default branch | `project_delivery`: the project's delivery mode (section 7) | Public by default: the scrub hook applies (section 8) |
| `foreign` | a file that loads here but belongs to the other boundary, such as the personal dotfiles installed on the work host | outside this host's boundary | `none` | Loads are recorded and checked against the bytes the owner last approved on this host; no proposal can target it |

The source map is built at install (T6.x) by walking every path in L4 and L5 for each owned project, and is refreshed by reconcile when a project is added or a new nested file appears. A loadable file with no source-map row is itself a drift (section 9): it has no approved revision to render and no exclude covering it, so the launch check refuses it rather than let an unmapped file reach a session.

Firstmate's own files (`data/captain.md`, `data/learnings.md`, the archives, the home's auto memory) are not sources. They enter Tiller once, through the importer's placement plan (section 10).

## 4. Revisions

`memory_revisions`: one row per content state of a source that Tiller has seen.

- `id`, `source_id`, `blob_sha` (the git blob of the canonical file), `commit_sha` (where it was committed; null for a foreign source), `installed_sha256` (for `dotfiles` and `foreign`: SHA-256 of the installed bytes), `parent_revision_id`.
- `review`: `approved`, `unreviewed`, `revoked`.
  - `approved`: may load. Written by a memory approval applying, by the owner's own edit in the memory view, or by the owner accepting an external revision.
  - `unreviewed`: an edit made outside Tiller, seen by reconcile. Does not load until the owner accepts it.
  - `revoked`: was approved, but contains a memory retired with `revoke_older = 1` (section 7). Does not load again.
- `provenance`: `tiller` (applied from a proposal or an owner edit), `external` (an outside edit reconciled), `legacy-import` (placed by the importer), `baseline` (the file as found at install).
- `approval_id` (the memory approval that produced it, or null for `baseline` and `external` before acceptance), `scrub` (`passed`, `flagged`), `recorded_at`, `recorded_by`.

A source's current revision is the newest `approved` row. Reconcile (T2.7) records a revision whenever the canonical blob, the installed bytes or the worktree copy differs from every known revision, with honest provenance: `external` and `unreviewed`, never `tiller` (point 27). Accepting an external revision is an owner action that writes a `memory` approval bound to the revision's diff from its parent and its parent's blob, so the audit trail is the same as for a proposal.

At install, every mapped file's current content is recorded as a `baseline` revision. The owner accepts the baselines once, in a batch, at the host's cutover checkpoint; until then that host's fleet does not launch.

## 5. Memories

`memories`: one row per entry, normally one line of a canonical file. The text lives in the file; the row holds the metadata.

- `id`, `source_id`, `section` (the heading it sits under), `text_sha256` (of the current line), `legacy_id` (`mem-<n>` from the import bundle).
- **Scope**: `scope_kind` (`fleet`, `project`, `role`) and `scope_value` (the project id or role name; null for `fleet`). Scope is where the entry applies; it can be narrower than the source that holds it (a role-scoped line in a project document). Scope never widens except by a promotion (section 6).
- **Class**: `class` (`rule`, `note`, `preference`). A requirement on Tiller's own mechanics is not memory: the importer turns it into a requirement row in the parity record, and a standing duty is an `obligations` row (state.md: "is a row, not a memory").
- **Evidence**: rows in `memory_evidence`, each pointing at an `evidence` row (section 6). A memory may have none (`baseline`, `legacy-import` with no recoverable source).
- **Validity**:
  - `validity`: `durable`, `until_condition`, `until_date`.
  - `validity_condition`: JSON, for `until_condition`. One of `{"card_closed": "<card id>"}`, `{"pr_merged": "<pr id>"}`, `{"event": {"kind": "...", "entity_id": "..."}}`, `{"version_at_least": {"tool": "...", "version": "..."}}`. Reconcile evaluates them; a condition that cannot be evaluated is reported, never assumed met.
  - `valid_until`: for `until_date`.
  - `review_due_at`: for `durable`, the next review date: approval or last review plus the class's interval (below).
  - `review_paths`: JSON list of path globs in the project the entry is about, or null. Set from `memory_fields` when the entry is proposed.
- **Review interval per class** (decided at CP2, C23 refined by S02). Built in, so the owner never configures an entry by hand; the config may override a class's number (`memory.review_days.<class>`):

  | Class | What it holds | Default interval |
  |---|---|---|
  | `rule` | how the fleet must work: process, safety, delivery rules | 180 days |
  | `preference` | the owner's choices of style, tools and taste | 365 days |
  | `note` | operational facts about a project: how it builds, where things live, what to watch out for | 90 days |

- **Review triggers besides the interval.** Reconcile marks an entry `due` early, with `due_reason`, when:
  - evidence is captured that contradicts it (`mem.note --contradicts <memory>` sets `evidence.contradicts_memory_id`): `due_reason = contradicting_evidence`, at once;
  - its project changes under it: a `project.edited` event that changes `delivery`, `default_branch`, `repo` or `checks` marks every `note` scoped to that project, and a landed change (an `approval.succeeded` merge or direct push, or a confirmed outside merge) that touches a path matching an entry's `review_paths` marks that entry: `due_reason = project_change`;
  - its interval passes: `due_reason = interval`.
- **Overdue only flags.** A `due` entry keeps loading exactly as approved. Nothing automatic erases it, rewrites it, lowers its scope, or relaxes a safety rule it states; only an owner-approved proposal changes it (section 7). The memory view lists due entries with their reason, and the owner keeps one (a new `review_due_at`) or retires it.
- **Checkable facts are queried, not stored.** A fact that can be checked directly when needed (a service's current status, a tool's installed version, whether a file exists) is not written as a durable memory; the entry names the command that checks it, or uses an `until_condition` such as `version_at_least`. The curator flow (T7.3) and the role instructions hold this; it is convention, tested by an eval (section 9).
- **Review status**: `review_status`:
  - `approved`: loads.
  - `unreviewed`: added by an external edit or a baseline, awaiting the owner. The revision that holds it does not load until accepted (section 4).
  - `due`: durable and past `review_due_at`, or marked early by a trigger above (`due_reason`). Still loads, unchanged. The memory view lists it for review; the owner keeps it (new `review_due_at`) or retires it. Never deleted by age.
  - `expired`: its condition or date is met. Reconcile opens a retire proposal in the same transaction. It still loads until that retire is approved and applied, because removing a line is itself a change to approve; the memory view shows expired lines first.
  - `retired`: removed from the canonical file. The row and its history stay.
- **Provenance**: `provenance` (`proposal`, `owner_edit`, `external`, `legacy-import`, `baseline`), `rationale` (the stated reason, written by the proposer or owner; null when none was given, and always null for `legacy-import` unless the source stated one verbatim), `authored_by_session_id` (the session that wrote the proposal; null for owner edits, external and imported), `proposal_id`, `created_revision_id`, `reviewed_at`, `reviewed_by`.
- **Retiring**: `retired_at`, `retired_revision_id`, `retire_reason`, `revoke_older` (0 or 1).
- `rev`.

Line identity across edits: a change applied by Tiller knows which memory it touched, so the mapping is exact. For an external revision, reconcile diffs the parent blob against the new one: unchanged lines keep their memory, a line replaced inside a hunk keeps its memory with `review_status = unreviewed`, a new line gets a new memory with `provenance = external`, and a removed line's memory is marked `retired` with `retire_reason = external edit`.

The memory view's From and Changed columns on each line (card id, or who wrote it, and the date) read from this row. They are plain text; the Source link in the line's editor opens the canonical file at the revision, the approval and the sessions that loaded it (section 7).

## 6. Evidence and proposals

These two tables, with T1.1's `observations`, are the unapproved side. No launch path, no hook output, no `tiller` command run inside a fleet session and no rendered prompt reads them. Only the board (owner) and the curator flow (T7.3, writing proposals) touch them. Plan v2 called the capture table `observations`; T1.1 used that name for session activity, so captured memory candidates are `evidence` here, and an `observations` row can itself be cited as evidence.

### Evidence

`evidence`: an observed fact that could support a memory. Captured by agents (`tiller mem note`), by the tailer and retros (T7.3), by the importer, and by the owner.

- `id`, `project_id`, `boundary`, `kind` (`observation`, `transcript`, `card`, `review`, `test`, `ci`, `incident`, `owner_message`, `legacy`).
- `ref`: JSON pointer to the source: a table and id (`observations`, `cards`, `reviews`, `events`), a transcript path and line range, a CI run URL, or the import bundle's entry id and its source file and line. Paths stay in `state.db`, never in git.
- `excerpt`: the minimal quoted text, when a quote is needed. Local only, never committed, never rendered into a session (point 29). Never a whole prompt.
- `card_id`, `run_id`, `session_id`, `captured_at`, `captured_by`.
- `independence_key`: what makes two pieces of evidence independent. Defaults to the card id the fact was observed on. Evidence derived from a session that had loaded a memory saying the same thing, or that cites other evidence, inherits that source's key. Repeated text from agents copying each other counts once.
- `superseded_by`: an evidence id, when a later fact replaces this one.
- `contradicts_memory_id`: the memory this fact contradicts, or null. Setting it marks that memory `due` at once (section 5).

Evidence is append-only apart from `superseded_by`. It is classed rebuildable-from-sources where it points at a source, and authoritative where it is the only record (owner messages, legacy entries); it is backed up with `state.db` either way.

### Proposals

`proposals`: one proposed change to one canonical file.

- `id`, `project_id`, `boundary`, `target_source_id`.
- `op`: `add`, `rewrite`, `remove`, `retire`, `promote`, `accept_external`.
- `memory_id`: the entry a `rewrite`, `remove`, `retire` or `promote` acts on.
- `base_revision_id`, `base_blob`: the revision and blob the diff was made against.
- `diff`: the unified diff of the canonical file, exactly as it will be applied. `diff_sha256`: SHA-256 of `diff`.
- `memory_fields`: JSON with the scope, class, validity and section the new or rewritten entry gets.
- `rationale`, `expected_effect` (what should change, on which task class, measurable where possible; point 23), linked evidence in `proposal_evidence`.
- `authored_by_session_id`, `card_id`, `created_at`.
- `state`: `proposed`, `accepted`, `applied`, `discarded`, `stale`, `failed`.
- `approval_id`, `decided_at`, `applied_revision_id`, `failure_reason`, `rev`.

Rules:

- A proposal is created only by `mem.propose` (section 12). A fleet role proposes; it never writes a canonical file.
- Mistakes and successes both qualify. A proposal needs at least one evidence row and an `expected_effect`. "Nothing worth saving" is a normal outcome of a retro and writes nothing.
- **Promotion** widens an entry's scope (a project note becoming a fleet rule, a role-scoped line becoming project-wide). It is always its own `promote` proposal, never a side effect, and needs evidence with at least two distinct `independence_key` values, from at least two projects when widening to `fleet`. Nothing promotes automatically.
- **Boundary**: `proposals.boundary` is the host's boundary (`meta.boundary`). The target source and every linked evidence row must have the same boundary, and the target must not be `foreign`. `mem.propose` refuses otherwise. Section 8 has the rest.
- **Budget**: `mem.propose` computes the token count each affected role would load after the change (L3 to L5 for the target scope) and records it on the proposal. The board shows it. `mem.decide` refuses an approval that would put a role over its startup budget (7,500 tokens for the orchestrator today, P42) unless the same decision lowers it or the owner raises the budget.
- The memory view shows proposals inline in the document, the way suggestion mode works (tinted new line; struck old text plus tinted new for a rewrite), with Keep and Discard. Keep is the approval. There is no separate queue (T7.2). Proposals can be decided in a batch; each still gets its own approval row.
- **The holdout from the board.** When the gate waits only for a holdout run (no run, or a stale one), the suggestion offers "Run holdout eval...". Its confirmation shows the cases, the model runs, the quota percent the run will probably take and what it leaves of the week, and it refuses under the quota floor (`learning-loop.md` section 7). While it runs, the suggestion shows its progress and Keep waits; when it passes, Keep turns on. A run that records nothing shows its reason on the suggestion.
- **Re-propose.** A proposal made against an older text (its `base_blob` is no longer the file's blob) cannot be kept: the gate says "the text moved; re-propose it on the current text and run the holdout again", and the memory view offers "Re-propose on current text" (U14). That is `mem.repropose`: the same add, rewrite or removal redone on the current text as a new proposal, with the old one's author, reason and evidence and `memory_fields.supersedes` naming it. The old proposal is closed `discarded` (a `stale` proposal needs an approval, and it never had one), and the change history leaves it out. If the old proposal is in a change set that is not closed, it leaves that set (`change_set.member_removed`), so the set's holdout runs no longer match its members; a set left with no members closes `abandoned`. The new proposal is in no change set, so Keep waits for its own holdout run.

## 7. Approve, apply, retire

### Approval binding

A memory approval is an `approvals` row (state.md) with `kind = memory`, `action = apply`, `project_id` (the proposal's, `fleet` for host-wide), `diff_sha256` (the proposal's), and `base_rev` set to the proposal's `base_blob`: the git blob of the canonical file the diff applies to. Binding the blob, not the commit, means unrelated commits elsewhere in the same repo do not stale it. The proposal points at it through `proposals.approval_id`.

The approval moves through the same states as any other (`requested`, `accepted`, `rejected`, `executing`, `succeeded`, `failed`, `uncertain`, `stale`). Owner edits made in the memory view are a proposal and an approval written in one act, decided by `owner`. There is no standing memory grant (decided at CP2, C22 as reinterpreted by S01): `grants.kind` has no `memory`, and a merge grant never matches a PR that touches a source-map path (`authority.md`, section 6). What the owner gets instead is one combined action for one exact change, below.

### Apply

`mem.apply`, run by the daemon only, for an `accepted` approval:

1. Take the per-repo lock (`flock` on a lock file beside the canonical repo's `.git`), so two writers to one repo serialize (point 7).
2. Read the canonical file's blob at `HEAD` and in the working tree. Both must equal `base_rev`, and `sha256(diff)` must equal `diff_sha256`. On any mismatch the approval goes `stale`, the proposal goes `stale`, and the memory view offers to rebase it into a new proposal that needs a new approval (Re-propose, section 6).
3. Apply the diff to a temporary index (`GIT_INDEX_FILE`) built from `HEAD`, so nothing another writer staged is bundled in, and commit only that path. The message names the change; trailers are `Tiller-Proposal: <id>` and `Tiller-Approval: <id>`. No quoted chat, no `Why:` copied from a message, no agent credit.
4. Run the scrub hook on the commit (section 8). A failure undoes the commit before it leaves the lock and marks the approval and proposal `failed` with the finding.
5. Deliver by `apply_method`:
   - `tiller_commit`: done; the next rendered prompt reads the new blob.
   - `dotfiles_rebuild`: run `rebuild_command`; applied when the installed bytes hash to the new blob's content. With no rebuild command, the revision is recorded with `installed_sha256` still the old one and the memory view says "committed, waiting for rebuild"; fleet launches keep rendering the previous approved revision until the new one is applied.
   - `project_delivery`: by the project's delivery mode. `direct-push`: push to the default branch after the project's `checks`. PR modes: push a branch and open a PR whose diff the daemon checks equals the approved diff on that path and touches nothing else. Landing it needs a merge approval bound to that PR's head SHA. When the owner chose the combined action on the proposal ("Approve memory change and merge", below), the daemon writes that merge approval `accepted` itself, `decided_by = owner`, and sets the memory approval's `linked_approval_id` to it, once its diff check passes; required checks must still pass, and any further commit on the PR makes both approvals `stale`. Without the combined action, the PR waits for the owner's merge approval. A merge grant never matches it. `local-only`: commit on the default branch.
6. Record the new `memory_revisions` row (`approved`, `provenance = tiller`, `approval_id`), update or insert the `memories` rows, mark the approval `succeeded` and the proposal `applied`, all in one transaction with their events.

**Approve memory change and merge** (decided at CP2, S01). A merge or land-direct approval for a worker's PR that touches a source-map path shows a "memory change" flag on the board, and its accept button reads "Approve memory change and merge". That one click records two authorizations against the exact reviewed revision: the merge approval bound to the PR's head SHA, and a memory approval per touched source bound to that file's diff at that head and its base blob. A new commit on the PR makes both `stale`, and required checks must still pass before the daemon lands it. The same combined action is offered on a memory proposal for a PR-delivered project (Apply, step 5). It authorizes that one change only; it creates no grant and says nothing about future memory changes. A worker cannot change memory through a PR the owner never saw as a memory change: grants never match such a PR, and the approved-snapshot rule (section 1) keeps the change from loading anywhere before the merge and the memory approval both succeed.

### Retire and roll back

Rolling back means retiring a memory, not erasing history (point 28).

- **Retire** is a `retire` proposal whose diff removes the entry. Before approval, the board previews the diff and any conflict with later revisions (a `git apply --check` against the current blob). On apply, the memory row gets `retired_at`, `retired_revision_id`, `retire_reason`; the git history and the row stay.
- **Undo** in the change history builds the reverse of that commit as a proposal (`git revert --no-commit` into a temporary index, conflicts shown) and applies it through the same path. It is a new revision, not a rewrite of history. Undo of a discard proposes the suggestion again, on the current text, for the owner to review; nothing in the file changes until it is kept.
- **Change history.** The foot of each document lists what changed in it in the last 30 days, newest first, at most 50 rows: each kept, added, edited, removed or discarded change, with its section, who wrote it (and from which card), and when it was kept or done. Each row has its own "Undo...", which confirms first and says what the undo will do; a row already undone says so and has no button. An undo's own proposal and a re-proposed original are not rows: they are recorded on the row they undo or replace.
- **Revoke**: setting `revoke_older = 1` on a retire marks every earlier revision containing the entry `revoked`, so no launch on a stale worktree or an old base can load it again (section 9). Use it for a harmful rule; an ordinary retire leaves older approved revisions loadable on old bases.
- Three different acts, kept apart: reverting the source file (the retire or undo above), removing it from future context (the next session start reads the new revision), and repairing its consequences (cards the owner opens; Tiller never infers them).

### Which sessions loaded a revision

`session_loads`: one row per (session, revision, trigger, moment) for every source revision Tiller rendered into a session's prompt. The launcher writes the `startup` rows for a launch that renders the prompt (`run.start`, a promotion, `run.relaunch` on the same run or a new attempt, the orchestrator launch) from the revisions it rendered, once the launch line went into the pane: a launch that fails before that records no load, and a failed write after it is the launch's warning, not its error. `session.resume` renders nothing: `claude --resume` reuses the session's prompt file. On a `SessionStart` observation for `resume`, `clear` or `compact`, the daemon copies the set the session had at the observation's time, with the new trigger; it hashes nothing and the hook only records the trigger, so the hook stays inside its budget (V09, kept at CP2). The daemon's `memory_loads` poller makes the copies every 5 seconds, for live sessions and for sessions that ended in the last hour. Each pass reads only the observations recorded since the last pass began, less 5 minutes for a write that was stamped before it committed; the first pass after a start reads them all. A session that loaded nothing by the observation's time gets no copy, an observation whose bind the hook refused (`bind_refused` in its payload) gets no copy, and a replayed observation writes no second row. A copy that fails is logged and shows in the poller's health, and the pass goes on to the other copies; the next pass reads it and tries it again, until it is made, or until an hour after the observation was recorded or the session ended, whichever comes first. The pass that tries it at or past that hour tries it last: it logs that it gave the copy up, its health line counts it, the session's set has no row for that trigger, and later passes do not read it again.

- `session_id`, `revision_id`, `trigger` (`startup`, `resume`, `clear`, `compact`), `loaded_at`.
- What a row proves (reworded at CP2, S04): Tiller gave the session that revision, because the prompt file was rendered from that blob by SHA at that launch. It does not prove what the model attended to, and it is not a hash of files on disk at some later moment. Because no memory file loads natively (section 2), a revision missing from a session's set cannot have reached it through a memory channel. The appended prompt persists across `/clear` and `/compact` [I: T4.3 pins it with `memory::render::appended_prompt_survives_clear_and_compact`], which is why those triggers copy the set instead of rendering again.
- Nested files and path-scoped rules are rendered at launch, so they are in the set from the start.

`sessions.config_hash` covers the memory part as SHA-256 over the sorted `(source_id, blob_sha)` pairs of the session's newest load set, together with the role file, skills, settings fragment, model and CLI version (state.md, point 22).

After a retire or a rewrite applies, the board lists every live session whose newest load set holds an earlier revision of that source containing the entry, and offers per session: send a correction (an outbox `note` naming the retired text's replacement, or "disregard: <entry>") or relaunch (`run.relaunch`, which re-reads memory). A session that ended is listed for the record only.

## 8. Scrub hook and the boundary

### Scrub hook

Every commit Tiller makes to a canonical file passes the scrub before it leaves the per-repo lock (section 7, step 4). The scrub uses the same rules as `scripts/scrub-check.sh`: home and local paths, LAN hostnames and addresses (documentation ranges allowed), email addresses, machine names, and private names kept as SHA-256 hashes in a terms file (the host's own `~/.tiller/scrub-terms.sha256`, plus an optional plain-text extra list kept outside any repo). It checks the added lines and the commit message. It runs on every source kind, including the private memory repo, so a file can move to a public repo later without a leak (public by default).

A scrub failure blocks the commit; it never silently strips text. Reconcile runs the same scrub on external revisions and records the result in `memory_revisions.scrub`. A `flagged` revision cannot be accepted until a later revision fixes it.

Tiller does not install git hooks into repos it does not own (the dotfiles, project repos): writing hook files there is outside a normal PR. The scrub is enforced in Tiller's own commit path and checked on every external revision; the project's own CI scrub (where it has one) is the second line.

### No promotion across the boundary

Each host's Tiller has one boundary, stored in `meta.boundary` (`personal` or `work`). It is set at install and changeable only while unused: the owner may change it with `tiller host edit --boundary` or the board's host card (`host.edit`, which writes `host.edited`) until a `memory_sources`, `evidence` or `proposals` row, a backup or an export records it. After that `host.edit` refuses the change and names what records it, for example "memory already records the personal boundary". A backup or an export is recorded in `~/.tiller/backups/manifest.json` (the export in its `exports` list), because a restore or an import checks the boundary it carries. `tiller init` replayed with another boundary is refused, never a silent no-op. Tiller runs separately on each host, and the work host's `state.db`, memory repo and backups stay on the work host (build plan: work data).

- Every `evidence`, `proposals` and `memory_sources` row carries a `boundary`, and `mem.propose` and `mem.note` refuse a row whose boundary differs from `meta.boundary`.
- A source that loads on this host but belongs to the other boundary is `foreign`: the personal dotfiles installed on the work host are the case today. Loads of a foreign source are recorded and checked; no proposal may target it. Work evidence can therefore never reach the personal dotfiles, and personal evidence can never reach a work repo.
- Work-wide memory that should load in every work project lives in the work host's own memory repo (`~/.tiller/memory/fleet.md` on that host).
- Export and import (M6) never carry `evidence`, `proposals`, `memories` or `session_loads` across hosts of different boundaries; `tiller import` refuses a bundle whose boundary differs from `meta.boundary`.
- Sharing a preference across the boundary is the owner's own act, done by hand in each boundary. Tiller never offers it.

## 9. The check: nothing unapproved reaches a fleet role

Acceptance for T1.5. Each guarantee is labeled **enforced** (code refuses, with the test that proves it) or **convention** (relied on, with the reason it is not enforced). A row marked **not enforced** is specified but not built yet, and names the open owner decision.

### Launch check

`run.start`, `run.relaunch`, `session.resume` and the orchestrator launch run the memory launch check before the pane starts, inside the spawn transaction's preconditions:

1. List every loadable file for the role and worktree: L4 and L5 paths, including every `@` import reached and every nested `CLAUDE.md` and `.claude/rules/*.md` in the worktree and on the project's default branch.
2. Each file must map to a source-map row. An unmapped file refuses the launch, because its text would otherwise be silently missing from the snapshot.
3. Write the run fragment's `claudeMdExcludes` so it covers every listed path, and refuse the launch if any listed path is not covered.
4. Render the L3 prompt from the current `approved` revision of every source: the Tiller documents, each L4 source and each L5 source. A source with no approved revision refuses the launch. A worktree copy or an installed copy that differs from the approved revision is not an error: it is a proposed or unreviewed change, it is not rendered, and reconcile records it (section 4).
5. Verify L1 is off (the environment variable is set, or the fallback directory is empty and read-only) and L2 has no `memory` field.
6. Write `config_hash`, then `session_loads` once the launch line went into the pane, so a launch that fails first records no load (section 7).

A relaunch or an orchestrator restart runs the check before it stops the live session, so a refused check leaves that session running.

A refusal opens a `review` attention item on the card the run is for (for an orchestrator launch, on the host's `fleet-1` card, state.md Project) naming the file, its blob and what it failed. The launch is retried after the answer. An owner's hand edit to the dotfiles no longer holds launches (CP2, C21 amended by S04): launches render the previous approved revision, and the memory view shows the edit as `unreviewed` with its one-click accept.

### Guarantees

| Guarantee | Label | Test |
|---|---|---|
| A fleet launch with an unmapped loadable file is refused | enforced | `memory::launch::unmapped_file_refuses` |
| A fleet launch renders only approved revisions: a copy that is not one (worktree edit, uncommitted dotfiles edit, external commit, revoked revision) is never rendered | enforced for Tiller documents (U15); the L4 and L5 sources are not built. The property test over source kinds and drift kinds is not written yet | `spawn::tests::spawn_renders_the_approved_memory_and_records_the_load` |
| A source with no approved revision refuses the launch | not enforced (U15): such a Tiller document renders nothing and the launch goes on (section 2). Owner decision open: build the refusal, or keep rendering nothing | (no test yet) |
| The rendered role prompt is built only from approved blobs; a dirty working tree in the memory repo, a project worktree or the dotfiles does not change it | enforced | `memory::render::reads_approved_blob_not_worktree` |
| Every L4 and L5 path the role would load is in the run fragment's `claudeMdExcludes` | enforced | `memory::launch::excludes_cover_every_loadable_path` |
| Every fleet launch sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, or the fallback directory is empty and read-only | enforced | `agents::spawn::auto_memory_off` |
| No role or agent file Tiller launches has a `memory` field | enforced | `agents::roles::no_memory_field` |
| No `tiller` command output in a fleet session, no hook output and no rendered prompt contains a row of `evidence`, `proposals` or `observations` | enforced | `memory::isolation::unapproved_tables_never_rendered` (canary strings in each table, asserted absent from every rendered surface) |
| The API serves `evidence` and `proposals` only to the owner's board | enforced (T1.4 authority) | `api::authority::proposals_owner_only` |
| An approval whose diff or base blob no longer matches is `stale` and applies nothing | enforced | `memory::apply::moved_base_goes_stale` |
| Two applies on one repo serialize and commit only their own path | enforced | `memory::apply::concurrent_writers_do_not_bundle` |
| A commit that fails the scrub is never kept | enforced | `memory::scrub::flagged_commit_is_undone` |
| A proposal across the boundary, or at a `foreign` source, is refused | enforced | `memory::boundary::cross_boundary_proposal_refused` |
| A promotion without two independent evidence keys is refused | enforced | `memory::promote::needs_independent_evidence` |
| An approval that would put a role over its startup budget is refused | not enforced (U15): `mem.decide` refuses only a proposal whose `token_budget_after` is set, and nothing computes it yet, so every approval passes. `agents::roles::startup_context_stays_under_budget` (P42) checks the role files only, not the memory a launch renders. Owner decision open: build the count, or drop the guarantee | (no test yet) |
| A fleet role cannot change what loads except through a PR the owner accepted as a memory change (CP2, C01 amended by S04). It may edit `AGENTS.md` and `CLAUDE.md` files in its own worktree; `.claude/rules/*.md` stays denied with the rest of `.claude/**` and changes only through `tiller mem propose` | enforced: the approved-snapshot rule (no memory file loads natively, the prompt is rendered from approved blobs); grants never match a source-map path; the merge of such a PR writes the memory approval (section 7); `Edit(<worktree>/.claude/**)` deny (`authority.md` G21) | `memory::launch::worktree_edit_is_not_rendered`, `memory::apply::grant_never_matches_memory_path`, `memory::apply::memory_flag_merge_writes_memory_approval`, `agents::guards::settings_edit_denied` |
| A memory file edited mid-session, in the role's own worktree or anywhere else, does not load into that session or any other fleet session before approval: not on a nested lazy load, not after `/clear`, `/compact` or `--resume` | enforced once T4.3 shows `claudeMdExcludes` holds for every file kind and trigger (`authority.md` section 2); until then unverified. If it does not hold, the fallback is to deny in-place edits of source-map paths to every fleet role, which narrows C01 and is a stop-and-ask | live `memory::live::excluded_memory_file_never_loads`, `reconcile::memory::worktree_drift_flags_run` |
| A durable entry past its review date, or marked due by a trigger, keeps loading unchanged; nothing automatic erases it or relaxes it | enforced | `memory::review::due_entry_keeps_loading_unchanged`, `memory::review::contradicting_evidence_marks_due`, `memory::review::project_change_marks_due` |
| A checkable fact is queried when needed, not stored as a durable memory | convention: the curator flow and role instructions | `evals/curator_queries_checkable_facts_instead_of_storing` |
| An approved change does not reach a running session until it starts, resumes, clears or compacts | convention: Claude Code reads memory files then. The board's correction offer covers the gap | (documented, not tested) |

What this does not cover: the owner's interactive sessions, and text the owner types into a fleet session. Both are the owner's own input, not memory.

## 10. Legacy import

The importer (T2.4) never writes a canonical file. It turns the bundle's `memory.jsonl` into a placement plan:

- one `evidence` row per entry (`kind = legacy`, `ref` = bundle id, entry id, source file and line; `excerpt` = the verbatim text, local only);
- one proposal per entry to place: `rule` entries to the host's `fleet.md` or a dotfiles file, `note` entries to the project's document, each with `memory_fields.provenance = legacy-import`, `rationale` only when the entry's `rationale` is non-null, `authored_by_session_id` null, and `legacy_id` set;
- `requirement` entries are not memory: they become rows in the parity record for the owner, not proposals;
- `stale` entries and entries the owner dropped at CP1 are listed in the plan as not placed, with the reason, so the count check (bundle entries = placed + requirements + not placed) holds;
- a second import of the same bundle (same `meta.import_manifest_sha256`) creates nothing.

The owner approves the placement plan in batches in the memory view. The memories it creates carry `provenance = legacy-import` and no invented rationale, author or origin card (point 26). Export data stays local: no bundle text is committed anywhere except through an approved, scrubbed proposal.

## 11. Schema

These tables replace the reservation in `schema.sql` ("Reserved for memory.md (T1.5)"). They load after `schema.sql`:

```sql
CREATE TABLE memory_sources (
  id               TEXT PRIMARY KEY,
  project_id       TEXT NOT NULL REFERENCES projects(id),   -- 'fleet' for host-wide
  boundary         TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  kind             TEXT NOT NULL CHECK (kind IN ('tiller_doc', 'dotfiles', 'project_file', 'foreign')),
  loaded_path      TEXT NOT NULL,          -- pattern, relative to the project root or '~'
  canonical_repo   TEXT,                   -- null for foreign
  canonical_path   TEXT,
  apply_method     TEXT NOT NULL CHECK (apply_method IN ('tiller_commit', 'dotfiles_rebuild', 'project_delivery', 'none')),
  load_mode        TEXT NOT NULL CHECK (load_mode IN ('rendered', 'eager', 'lazy')),
  roles            TEXT NOT NULL DEFAULT '["orchestrator","scout","builder","reviewer","verifier"]', -- JSON
  rebuild_command  TEXT,
  added_at         TEXT NOT NULL,
  retired_at       TEXT,
  rev              INTEGER NOT NULL DEFAULT 1,
  CHECK ((kind = 'foreign') = (apply_method = 'none')),
  CHECK (kind = 'foreign' OR (canonical_repo IS NOT NULL AND canonical_path IS NOT NULL)),
  CHECK ((kind = 'tiller_doc') = (load_mode = 'rendered')),
  CHECK (kind <> 'dotfiles' OR apply_method = 'dotfiles_rebuild'),
  CHECK (kind <> 'project_file' OR apply_method = 'project_delivery'),
  CHECK (kind <> 'tiller_doc' OR apply_method = 'tiller_commit')
) STRICT;
CREATE UNIQUE INDEX memory_sources_loaded ON memory_sources(project_id, loaded_path) WHERE retired_at IS NULL;

CREATE TABLE memory_revisions (
  id                  TEXT PRIMARY KEY,
  source_id           TEXT NOT NULL REFERENCES memory_sources(id),
  blob_sha            TEXT NOT NULL,       -- git blob of the canonical file
  commit_sha          TEXT,
  installed_sha256    TEXT,                -- dotfiles and foreign: the installed bytes
  parent_revision_id  TEXT REFERENCES memory_revisions(id),
  review              TEXT NOT NULL CHECK (review IN ('approved', 'unreviewed', 'revoked')),
  provenance          TEXT NOT NULL CHECK (provenance IN ('tiller', 'external', 'legacy-import', 'baseline')),
  approval_id         TEXT REFERENCES approvals(id),
  scrub               TEXT NOT NULL CHECK (scrub IN ('passed', 'flagged')),
  recorded_at         TEXT NOT NULL,
  recorded_by         TEXT NOT NULL,
  CHECK (review <> 'approved' OR scrub = 'passed'),
  CHECK (provenance <> 'tiller' OR approval_id IS NOT NULL)
) STRICT;
CREATE INDEX memory_revisions_source ON memory_revisions(source_id, recorded_at);
CREATE UNIQUE INDEX memory_revisions_content ON memory_revisions(source_id, blob_sha, coalesce(installed_sha256, ''));

CREATE TABLE evidence (
  id                TEXT PRIMARY KEY,
  project_id        TEXT NOT NULL REFERENCES projects(id),
  boundary          TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  kind              TEXT NOT NULL CHECK (kind IN
                      ('observation', 'transcript', 'card', 'review', 'test', 'ci',
                       'incident', 'owner_message', 'legacy')),
  ref               TEXT NOT NULL,         -- JSON pointer; local paths stay here, never in git
  excerpt           TEXT,                  -- local only, never rendered into a session
  card_id           TEXT REFERENCES cards(id),
  run_id            TEXT REFERENCES runs(id),
  session_id        TEXT REFERENCES sessions(id),
  independence_key  TEXT NOT NULL,
  captured_at       TEXT NOT NULL,
  captured_by       TEXT NOT NULL,
  superseded_by     TEXT REFERENCES evidence(id),
  contradicts_memory_id TEXT             -- forward reference to memories; marks it due at once
) STRICT;
CREATE INDEX evidence_project ON evidence(project_id, captured_at);

CREATE TABLE memories (
  id                      TEXT PRIMARY KEY,
  source_id               TEXT NOT NULL REFERENCES memory_sources(id),
  section                 TEXT,
  text_sha256             TEXT NOT NULL,
  legacy_id               TEXT,
  scope_kind              TEXT NOT NULL CHECK (scope_kind IN ('fleet', 'project', 'role')),
  scope_value             TEXT,
  class                   TEXT NOT NULL CHECK (class IN ('rule', 'note', 'preference')),
  validity                TEXT NOT NULL CHECK (validity IN ('durable', 'until_condition', 'until_date')),
  validity_condition      TEXT,            -- JSON
  valid_until             TEXT,
  review_due_at           TEXT,
  review_paths            TEXT,            -- JSON globs; a landed change touching one marks it due
  review_status           TEXT NOT NULL CHECK (review_status IN
                            ('approved', 'unreviewed', 'due', 'expired', 'retired')),
  due_reason              TEXT CHECK (due_reason IN ('interval', 'contradicting_evidence', 'project_change')),
  provenance              TEXT NOT NULL CHECK (provenance IN
                            ('proposal', 'owner_edit', 'external', 'legacy-import', 'baseline')),
  rationale               TEXT,            -- never invented; null when none was stated
  authored_by_session_id  TEXT REFERENCES sessions(id),
  proposal_id             TEXT,            -- forward reference to proposals
  created_revision_id     TEXT NOT NULL REFERENCES memory_revisions(id),
  reviewed_at             TEXT,
  reviewed_by             TEXT,
  retired_at              TEXT,
  retired_revision_id     TEXT REFERENCES memory_revisions(id),
  retire_reason           TEXT,
  revoke_older            INTEGER NOT NULL DEFAULT 0 CHECK (revoke_older IN (0, 1)),
  rev                     INTEGER NOT NULL DEFAULT 1,
  CHECK ((scope_kind = 'fleet') = (scope_value IS NULL)),
  CHECK (validity <> 'until_condition' OR validity_condition IS NOT NULL),
  CHECK (validity <> 'until_date' OR valid_until IS NOT NULL),
  CHECK (validity <> 'durable' OR review_status IN ('unreviewed', 'retired') OR review_due_at IS NOT NULL),
  CHECK ((review_status = 'retired') = (retired_at IS NOT NULL)),
  CHECK (provenance <> 'proposal' OR proposal_id IS NOT NULL),
  CHECK (provenance <> 'legacy-import' OR legacy_id IS NOT NULL),
  CHECK ((review_status = 'due') = (due_reason IS NOT NULL))
) STRICT;
CREATE INDEX memories_source ON memories(source_id) WHERE review_status <> 'retired';
CREATE INDEX memories_due ON memories(review_due_at) WHERE review_status = 'approved';

CREATE TABLE memory_evidence (
  memory_id    TEXT NOT NULL REFERENCES memories(id),
  evidence_id  TEXT NOT NULL REFERENCES evidence(id),
  PRIMARY KEY (memory_id, evidence_id)
) STRICT;

CREATE TABLE proposals (
  id                      TEXT PRIMARY KEY,
  project_id              TEXT NOT NULL REFERENCES projects(id),
  boundary                TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  target_source_id        TEXT NOT NULL REFERENCES memory_sources(id),
  op                      TEXT NOT NULL CHECK (op IN
                            ('add', 'rewrite', 'remove', 'retire', 'promote', 'accept_external')),
  memory_id               TEXT REFERENCES memories(id),
  base_revision_id        TEXT NOT NULL REFERENCES memory_revisions(id),
  base_blob               TEXT NOT NULL,
  diff                    TEXT NOT NULL,
  diff_sha256             TEXT NOT NULL,
  memory_fields           TEXT NOT NULL DEFAULT '{}',   -- JSON: scope, class, validity, section
  rationale               TEXT,
  expected_effect         TEXT,
  token_budget_after      TEXT,            -- JSON: role -> tokens loaded after the change
  authored_by_session_id  TEXT REFERENCES sessions(id),
  card_id                 TEXT REFERENCES cards(id),
  created_at              TEXT NOT NULL,
  created_by              TEXT NOT NULL,
  state                   TEXT NOT NULL DEFAULT 'proposed' CHECK (state IN
                            ('proposed', 'accepted', 'applied', 'discarded', 'stale', 'failed')),
  approval_id             TEXT REFERENCES approvals(id),
  decided_at              TEXT,
  applied_revision_id     TEXT REFERENCES memory_revisions(id),
  failure_reason          TEXT,
  rev                     INTEGER NOT NULL DEFAULT 1,
  CHECK (op IN ('add', 'accept_external') OR memory_id IS NOT NULL),
  CHECK (op IN ('accept_external', 'retire', 'remove') OR expected_effect IS NOT NULL),
  CHECK ((state = 'applied') = (applied_revision_id IS NOT NULL)),
  CHECK (state IN ('proposed', 'discarded') OR approval_id IS NOT NULL)
) STRICT;
CREATE INDEX proposals_open ON proposals(target_source_id) WHERE state IN ('proposed', 'accepted');

CREATE TABLE proposal_evidence (
  proposal_id  TEXT NOT NULL REFERENCES proposals(id),
  evidence_id  TEXT NOT NULL REFERENCES evidence(id),
  PRIMARY KEY (proposal_id, evidence_id)
) STRICT;

CREATE TABLE session_loads (
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  revision_id  TEXT NOT NULL REFERENCES memory_revisions(id),
  trigger      TEXT NOT NULL CHECK (trigger IN ('startup', 'resume', 'clear', 'compact')),
  loaded_at    TEXT NOT NULL,
  -- the trigger is in the key from migration 0019, so a copy in the same
  -- millisecond as another load of the session gets its own rows
  PRIMARY KEY (session_id, revision_id, trigger, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);
```

The evidence requirement ("at least one evidence row") and the promotion rule span rows, so `mem.propose` checks them inside its transaction, not a `CHECK`. `meta` gains the key `boundary`.

Durability (state.md, section 10): `memory_sources`, `memory_revisions`, `memories`, `proposals`, `proposal_evidence`, `memory_evidence` and `evidence` are authoritative and backed up with `state.db` inside the host's boundary. `session_loads` is authoritative for the audit question it answers and is pruned with its session's retention. The memory files themselves are authoritative under git.

## 12. Mutations

Each follows state.md, section 8: one `BEGIN IMMEDIATE` transaction with its event rows.

| Mutation | Caller | Preconditions | Writes | Events |
|---|---|---|---|---|
| `mem.note` | fleet role (`tiller mem note [--contradicts <memory>]`), tailer, retro, owner | boundary matches; card or session exists; a contradicted memory exists and is not retired | `evidence`; with `--contradicts`, `memories.review_status = due`, `due_reason = contradicting_evidence` | `evidence.captured`, `memory.due` |
| `mem.propose` | fleet role (`tiller mem propose`), curator, owner, reconcile (expiry, external edit) | target not `foreign`, not retired; boundaries match; base revision is current approved; at least one evidence row (not for `accept_external`, `retire`, `remove`); promotion rule; diff applies to `base_blob` | `proposals`, `proposal_evidence` | `proposal.created` |
| `mem.decide` | board (Keep, Discard, "Approve memory change and merge"), `tiller mem approve` (owner; by id, or in bulk by `--class`, `--boundary` or `--project`, each proposal decided on its own through the same gate and approval as the board's Keep, a held one refused with the gate's reason while the rest go on; `--dry-run` writes nothing) | proposal `proposed`; base still current; budget rule; idempotency key new or identical | `approvals` (`kind = memory`, `accepted` or `rejected`), `proposals.state`, `approval_id`; with the combined action on a PR-delivered project, the flag that lets the daemon accept the merge approval once its diff check passes (section 7) | `approval.decided`, `proposal.decided` |
| `mem.repropose` | board (owner, "Re-propose on current text") | proposal `proposed`; the board's `base_blob` is the file's current blob; the proposal's `base_blob` is not | a new `proposals` row (op, author, rationale and evidence of the old one; `memory_fields.supersedes`), `proposal_evidence`; the old proposal `discarded`, its reason naming the new one; its `change_set_members` row in a set that is not closed deleted, and that set `closed` `abandoned` if it has no members left | `proposal.created`, `proposal.stale`, `change_set.member_removed`, `change_set.closed` (a set left empty) |
| `mem.apply` | daemon only | approval `accepted`; blob and diff hash match under the repo lock | `approvals` (`executing`, then `succeeded`, `failed` or `stale`), `memory_revisions`, `memories` (a retire or rewrite also sets `due_reason` null), `proposals` | `approval.executing`, `approval.finished`, `memory.applied` or `proposal.stale` |
| `mem.reconcile` | reconcile (T2.7) | none | `memory_revisions` (`external`, `unreviewed`), `memories` (line mapping), `memories.review_status` (`due` with `due_reason` for the interval and the project-change triggers; `expired` with `due_reason` null), retire proposals for expired entries | `memory.revision_seen`, `memory.due`, `memory.expired`, `proposal.created` |
| `mem.outside_edit` | daemon only (the memory repo watcher) | the repo's `HEAD` or a document's text moved since the watcher's last look, and the change is not Tiller's own commit (`HEAD` is a commit Tiller recorded since then and every changed document matches it) | `meta.memory_watch` (the watcher's last look, kept so a restart compares with it) | `memory_repo.changed` with `head` (null without a commit) and `files` (the documents whose text changed): the board's cue to read the memory view again. It records no revision; seeing the edit as `external` is `mem.reconcile`. An edit made while the daemon was down is one change on its first look |
| `mem.watch` | daemon only (the memory repo watcher) | the watcher's last look moved with no outside change (its first look, or Tiller's own commit) | `meta.memory_watch` | none: it is rebuildable bookkeeping, and without it the first look is the baseline |
| `mem.accept_external` | board (owner) | revision `unreviewed`; scrub `passed` | `approvals` (decided by `owner`, bound to the revision's diff from its parent and the parent's blob), `memory_revisions.review = approved`, `memories.review_status` | `approval.decided`, `memory.accepted` |
| `mem.review` | board (owner) | memory `due` | `memories.review_due_at` (now plus the class's interval), `reviewed_at`, `review_status = approved`, `due_reason` null | `memory.reviewed` |
| `mem.load` | the launcher (`run.start`, a promotion, `run.relaunch`, the orchestrator launch), from the revisions it rendered; the daemon, on a `SessionStart` observation for `resume`, `clear` or `compact`, copying the newest set (the hook records the trigger and hashes nothing, protocol.md 4.2) | launch check passed | `session_loads`, `sessions.config_hash` | `session.memory_loaded` |
| `mem.source_sync` | install, reconcile | none | `memory_sources`, `baseline` revisions | `memory.source_added`, `memory.source_retired` |

## 13. Coverage

### Second opinion points this spec covers

| Point | How |
|---|---|
| 7 | Memory commits serialized per repo with `flock`, committed from a temporary index, only their own path |
| 16 | A memory approval binds `diff_sha256` and the base blob; rechecked under the lock; stale refused |
| 18 | `meta.boundary`, boundary on every row, `foreign` sources, no export across boundaries |
| 22 | The memory part of `config_hash` from `session_loads` |
| 23 | Proposals for successes and mistakes, with evidence and an expected effect; "nothing worth saving" is normal |
| 24 | No notes tier; auto memory off; `evidence`, `proposals` and `observations` never rendered; the launch check |
| 25 | Scope, evidence, validity conditions, review status; durable entries reviewed, never aged out; independent evidence for promotion; no cross-boundary promotion |
| 26 | Separate evidence, rationale, author and approval fields; `legacy-import` with no invented rationale |
| 27 | The source map; dotfiles edited at source and rebuilt; external edits reconciled as `external` |
| 28 | Retire, undo and revoke as new revisions with a preview; `session_loads` for corrections or restarts |
| 29 | Excerpts and transcript references stay in `state.db`; the scrub hook on every memory commit |
| 31 | No `memory` field on any role; reviewers get approved memory as input |
| 32 | The launch check verifies L1 on every launch, on every host, beyond the probe |
| 40 | The per-role token budget is computed on the context each role actually loads, imports included |

### Parity rows this spec gives a home

P42 (startup budget, enforced at approval and checked at launch), P16 (the standing duty is an `obligations` row; memory holds only the rule text if the owner wants it there).

### Firstmate contracts departed from

| Firstmate | Tiller | Why |
|---|---|---|
| Notes tier: agents write learnings that load next start; reviewed after | No notes tier: evidence and proposals, loaded only after approval | Point 24: automatic notes are already applied learning |
| 30-day decay unless reinforced | Validity conditions and review dates | Point 25: age discards rarely used but important knowledge |
| "Why" copied from the preceding message | Separate rationale field, null when not stated | Point 26 |
| PostToolUse auto-commit of agent writes | Agents never write canonical files; the daemon applies approved diffs | Point 27: watching Write and Edit misses other writers and bundles concurrent changes |
| Rollback is `git revert` | Retire, undo and revoke as reviewed revisions, with affected sessions listed | Point 28 |

## Decided at CP2

The owner's decisions on this spec's open items, Monday 2026-11-02 (`cp2-open.md` items 1 and 21 to 24, with round 2 revisions S01, S02 and S04):

1. **The launch check and the owner's own dotfiles edits** (C21, amended by S04): one-click accept kept; signed commits are not trusted. Under the approved-snapshot rule the edit no longer holds launches: it does not load until accepted (sections 2 and 9).
2. **One click or two for a memory PR** (C22, reinterpreted by S01): one combined "Approve memory change and merge" action, bound to the exact revision; new commits invalidate it, required checks still apply, and there is no standing memory grant (section 7).
3. **Review interval** (C23, refined by S02): built-in defaults per class, `rule` 180 days, `preference` 365 days, `note` 90 days; review also triggered by contradicting evidence or a project change; overdue only flags; checkable facts are queried (section 5).
4. **The memory repo's remote** (C24): a personal host pushes to one private remote (`memory.remote`), set at that host's cutover; a work host never pushes (section 3).
5. **Worker edits to project memory files** (C01, amended by S04): allowed in the worker's worktree through a PR the owner accepts as a memory change, never loaded before approval; `.claude/rules/*.md` changes only through `tiller mem propose` (sections 2, 7 and 9).
6. **What the load record proves** (V09, kept and reworded by S04): `session_loads` records the approved revisions Tiller rendered for a launch, not a later hash of files on disk (section 7).
