# Memory

This is the contract for memory in Tiller: what a fleet role loads at startup, where each piece of it lives, how a change to it is proposed, approved, applied, retired and audited, and where unapproved material is kept so it never loads. It builds on `state.md` and `schema.sql` (T1.1). The DDL for the tables it adds is in section 11; T2.1 adds it to the first migration with the rest of `schema.sql`.

Written Monday 2026-11-02, Lisbon time (WET). Task T1.5. Status: draft for CP2.

## 1. Principles

1. **Nothing unapproved loads.** A fleet role (orchestrator, scout, builder, reviewer, verifier) loads only text whose exact revision is approved. Unapproved material (evidence, proposals, raw transcripts, external edits not yet reviewed) lives only in `state.db` or in a file revision marked `unreviewed`, and no launch path reads either (plan v2, point 24).
2. **Approved memory lives in files under git.** The file is the authority for the text (ADR 0002). `state.db` holds the metadata about each line (scope, evidence, validity, review status, provenance) and every unapproved record. No marker bytes go into memory files, so metadata costs no startup tokens.
3. **One canonical file per memory, edited at its source.** The source map (section 3) names, for every file that can load, the one file a change is written to and how the change reaches the loaded copy. A file installed by the dotfiles is never edited where it is installed (point 27).
4. **Approval binds the exact change.** A memory approval binds the SHA-256 of the diff and the git blob of the file it applies to. Either one moving makes the approval stale (point 16).
5. **Evidence, rationale, author and approval are separate fields.** None is copied from another, and none is invented. Imported memory says `legacy-import` and carries a rationale only when its source stated one verbatim (point 26).
6. **Validity, not age.** A temporary fact expires on its condition. A durable one comes up for review on a date and is never deleted by age (point 25).
7. **Nothing crosses the personal and work boundary.** Each host has one boundary. Evidence, proposals and memory metadata never leave it, and a change built from one boundary's evidence never targets a file that loads in the other (point 25, point 18).

Fleet roles only. The owner's own interactive sessions keep native auto memory and load whatever the owner chooses; this spec governs them only where they share a file with the fleet (the source map), and there a change still needs approval before a fleet role loads it.

## 2. What a fleet role loads

Every channel by which text can enter a fleet role's context, and the rule that keeps unapproved text out of it. Section 9 turns each row into a check.

| # | Channel | Rule |
|---|---|---|
| L1 | Claude Code auto memory (`<config dir>/projects/<slug>/memory/MEMORY.md`) | Off. Every fleet launch sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` (host-capabilities: honored from a herdr pane on host B; host A pending the herdr-launched case). Fallback on a host that does not honor it: `autoMemoryDirectory` pointing at an empty read-only directory, which the launch check verifies is still empty |
| L2 | Subagent persistent `memory:` | No role or agent file Tiller launches has a `memory` field (T4.1). Reviewers get approved memory as input and have no memory tool (point 31) |
| L3 | The role prompt appended with `--append-system-prompt-file` | Tiller renders it per session: the role file plus the Tiller memory documents for the session's project and `fleet`, read from the approved git blobs by SHA (`git cat-file blob`), never from the working tree. Uncommitted or unapproved edits to those documents cannot reach it |
| L4 | User `CLAUDE.md` and its `@` imports, installed by the dotfiles | Loads as installed. The launch check compares the installed bytes with an approved revision of the source (section 9) |
| L5 | Project `CLAUDE.md`, `AGENTS.md`, `CLAUDE.local.md`, ancestors, nested files loaded lazily, `.claude/rules/*.md`, and every `@` import they reach | Loads from the worker's worktree. The launch check compares every such file in the worktree with an approved revision of its source; the role's deny rules stop the role editing them in place (section 9) |
| L6 | Skills, agent definitions, plugins, output styles, hook output injected as context, MCP server instructions | Harness configuration, not memory text. It changes only through the dotfiles or the project repo, enters `sessions.config_hash`, and is checked by T4.1's launch check of effective instructions. Fleet roles launch with `--strict-mcp-config`. `tiller hook` never emits evidence or proposal text as context |
| L7 | Task content: card bodies, attention answers, outbox bodies through `tiller inbox` | Not memory. The protocol (T1.3) never renders a row of `evidence` or `proposals` into any of them, and the API serves those tables only to the owner's board (T1.4) |

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
| `tiller_doc` | rendered into the role prompt (L3) | `~/.tiller/memory/fleet.md` and `~/.tiller/memory/projects/<id>.md` in the host's memory repo | `tiller_commit`: commit to the memory repo | The memory view's per-project documents, with plain sections ("How we work here", "Watch out for", "Preferences"), one line per entry. The memory repo is a git repo that never leaves the host's boundary; a personal host may push it to a private remote, a work host never pushes it off the machine |
| `dotfiles` | `~/.claude/CLAUDE.md`, user `AGENTS.md`, dotfiles skills and roles | the file in the dotfiles repo that home-manager installs from | `dotfiles_rebuild`: commit to the dotfiles repo, then run `rebuild_command` | Never edited in the installed location or the Nix store. Applied means the installed bytes equal the committed blob, checked after the rebuild |
| `project_file` | `AGENTS.md`, `CLAUDE.md`, `.claude/rules/*.md` in a worktree | the same path on the project's default branch | `project_delivery`: the project's delivery mode (section 7) | Public by default: the scrub hook applies (section 8) |
| `foreign` | a file that loads here but belongs to the other boundary, such as the personal dotfiles installed on the work host | outside this host's boundary | `none` | Loads are recorded and checked against the bytes the owner last approved on this host; no proposal can target it |

The source map is built at install (T6.x) by walking every path in L4 and L5 for each owned project, and is refreshed by reconcile when a project is added or a new nested file appears. A loadable file with no source-map row is itself a drift (section 9): the launch check refuses it rather than loading an unmapped file.

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
  - `review_due_at`: for `durable`, the next review date (default 180 days after approval or last review; configurable per class).
- **Review status**: `review_status`:
  - `approved`: loads.
  - `unreviewed`: added by an external edit or a baseline, awaiting the owner. The revision that holds it does not load until accepted (section 4).
  - `due`: durable and past `review_due_at`. Still loads. The memory view lists it for review; the owner keeps it (new `review_due_at`) or retires it. Never deleted by age.
  - `expired`: its condition or date is met. Reconcile opens a retire proposal in the same transaction. It still loads until that retire is approved and applied, because removing a line is itself a change to approve; the memory view shows expired lines first.
  - `retired`: removed from the canonical file. The row and its history stay.
- **Provenance**: `provenance` (`proposal`, `owner_edit`, `external`, `legacy-import`, `baseline`), `rationale` (the stated reason, written by the proposer or owner; null when none was given, and always null for `legacy-import` unless the source stated one verbatim), `authored_by_session_id` (the session that wrote the proposal; null for owner edits, external and imported), `proposal_id`, `created_revision_id`, `reviewed_at`, `reviewed_by`.
- **Retiring**: `retired_at`, `retired_revision_id`, `retire_reason`, `revoke_older` (0 or 1).
- `rev`.

Line identity across edits: a change applied by Tiller knows which memory it touched, so the mapping is exact. For an external revision, reconcile diffs the parent blob against the new one: unchanged lines keep their memory, a line replaced inside a hunk keeps its memory with `review_status = unreviewed`, a new line gets a new memory with `provenance = external`, and a removed line's memory is marked `retired` with `retire_reason = external edit`.

The memory view's provenance hint on each line (card id and date) reads from this row; clicking it opens the canonical file at the revision, the approval and the sessions that loaded it (section 7).

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

## 7. Approve, apply, retire

### Approval binding

A memory approval is an `approvals` row (state.md) with `kind = memory`, `action = apply`, `project_id` (the proposal's, `fleet` for host-wide), `diff_sha256` (the proposal's), and `base_rev` set to the proposal's `base_blob`: the git blob of the canonical file the diff applies to. Binding the blob, not the commit, means unrelated commits elsewhere in the same repo do not stale it. The proposal points at it through `proposals.approval_id`.

The approval moves through the same states as any other (`requested`, `accepted`, `rejected`, `executing`, `succeeded`, `failed`, `uncertain`, `stale`). Owner edits made in the memory view are a proposal and an approval written in one act, decided by `owner`. Standing grants never accept a memory approval in v1 (`grants.kind` has no `memory`).

### Apply

`mem.apply`, run by the daemon only, for an `accepted` approval:

1. Take the per-repo lock (`flock` on a lock file beside the canonical repo's `.git`), so two writers to one repo serialize (point 7).
2. Read the canonical file's blob at `HEAD` and in the working tree. Both must equal `base_rev`, and `sha256(diff)` must equal `diff_sha256`. On any mismatch the approval goes `stale`, the proposal goes `stale`, and the memory view offers to rebase it into a new proposal that needs a new approval.
3. Apply the diff to a temporary index (`GIT_INDEX_FILE`) built from `HEAD`, so nothing another writer staged is bundled in, and commit only that path. The message names the change; trailers are `Tiller-Proposal: <id>` and `Tiller-Approval: <id>`. No quoted chat, no `Why:` copied from a message, no agent credit.
4. Run the scrub hook on the commit (section 8). A failure undoes the commit before it leaves the lock and marks the approval and proposal `failed` with the finding.
5. Deliver by `apply_method`:
   - `tiller_commit`: done; the next rendered prompt reads the new blob.
   - `dotfiles_rebuild`: run `rebuild_command`; applied when the installed bytes hash to the new blob's content. With no rebuild command, the revision is recorded with `installed_sha256` still the old one and the memory view says "committed, waiting for rebuild"; fleet launches keep checking against the installed bytes, which still match the previous approved revision.
   - `project_delivery`: by the project's delivery mode. `direct-push`: push to the default branch after the project's `checks`. PR modes: push a branch and open a PR whose diff the daemon checks equals the approved diff on that path and touches nothing else; landing then follows the project's normal merge approval or grant. `local-only`: commit on the default branch.
6. Record the new `memory_revisions` row (`approved`, `provenance = tiller`, `approval_id`), update or insert the `memories` rows, mark the approval `succeeded` and the proposal `applied`, all in one transaction with their events.

A merge or land-direct approval for a worker's PR that touches a source-map path shows a "memory change" flag on the board, and accepting it with the flag shown also writes the memory approval for that file's diff and base blob. A worker cannot change memory through a PR the owner never saw as a memory change.

### Retire and roll back

Rolling back means retiring a memory, not erasing history (point 28).

- **Retire** is a `retire` proposal whose diff removes the entry. Before approval, the board previews the diff and any conflict with later revisions (a `git apply --check` against the current blob). On apply, the memory row gets `retired_at`, `retired_revision_id`, `retire_reason`; the git history and the row stay.
- **Undo** in the memory view's "Recent changes" rail builds the reverse of that commit as a proposal (`git revert --no-commit` into a temporary index, conflicts shown) and applies it through the same path. It is a new revision, not a rewrite of history.
- **Revoke**: setting `revoke_older = 1` on a retire marks every earlier revision containing the entry `revoked`, so no launch on a stale worktree or an old base can load it again (section 9). Use it for a harmful rule; an ordinary retire leaves older approved revisions loadable on old bases.
- Three different acts, kept apart: reverting the source file (the retire or undo above), removing it from future context (the next session start reads the new revision), and repairing its consequences (cards the owner opens; Tiller never infers them).

### Which sessions loaded a revision

`session_loads`: one row per (session, revision) for every source revision a session could load, written by the daemon on each `SessionStart` observation (start, resume, clear and compaction each re-read memory files; the hook only records the trigger, so it stays inside its budget), and by `run.start` for the rendered prompt.

- `session_id`, `revision_id`, `trigger` (`startup`, `resume`, `clear`, `compact`), `loaded_at`.
- Lazy sources count as loaded at session start: the row means "could have loaded", which is the safe reading for a correction.

`sessions.config_hash` covers the memory part as SHA-256 over the sorted `(source_id, blob_sha)` pairs of the session's newest load set, together with the role file, skills, settings fragment, model and CLI version (state.md, point 22).

After a retire or a rewrite applies, the board lists every live session whose newest load set holds an earlier revision of that source containing the entry, and offers per session: send a correction (an outbox `note` naming the retired text's replacement, or "disregard: <entry>") or relaunch (`run.relaunch`, which re-reads memory). A session that ended is listed for the record only.

## 8. Scrub hook and the boundary

### Scrub hook

Every commit Tiller makes to a canonical file passes the scrub before it leaves the per-repo lock (section 7, step 4). The scrub uses the same rules as `scripts/scrub-check.sh`: home and local paths, LAN hostnames and addresses (documentation ranges allowed), email addresses, machine names, and private names kept as SHA-256 hashes in a terms file (the host's own `~/.tiller/scrub-terms.sha256`, plus an optional plain-text extra list kept outside any repo). It checks the added lines and the commit message. It runs on every source kind, including the private memory repo, so a file can move to a public repo later without a leak (public by default).

A scrub failure blocks the commit; it never silently strips text. Reconcile runs the same scrub on external revisions and records the result in `memory_revisions.scrub`. A `flagged` revision cannot be accepted until a later revision fixes it.

Tiller does not install git hooks into repos it does not own (the dotfiles, project repos): writing hook files there is outside a normal PR. The scrub is enforced in Tiller's own commit path and checked on every external revision; the project's own CI scrub (where it has one) is the second line.

### No promotion across the boundary

Each host's Tiller has one boundary, stored in `meta.boundary` (`personal` or `work`) at install and never changed in place. Tiller runs separately on each host, and the work host's `state.db`, memory repo and backups stay on the work host (build plan: work data).

- Every `evidence`, `proposals` and `memory_sources` row carries a `boundary`, and `mem.propose` and `mem.note` refuse a row whose boundary differs from `meta.boundary`.
- A source that loads on this host but belongs to the other boundary is `foreign`: the personal dotfiles installed on the work host are the case today. Loads of a foreign source are recorded and checked; no proposal may target it. Work evidence can therefore never reach the personal dotfiles, and personal evidence can never reach a work repo.
- Work-wide memory that should load in every work project lives in the work host's own memory repo (`~/.tiller/memory/fleet.md` on that host).
- Export and import (M6) never carry `evidence`, `proposals`, `memories` or `session_loads` across hosts of different boundaries; `tiller import` refuses a bundle whose boundary differs from `meta.boundary`.
- Sharing a preference across the boundary is the owner's own act, done by hand in each boundary. Tiller never offers it.

## 9. The check: nothing unapproved reaches a fleet role

Acceptance for T1.5. Each guarantee is labeled **enforced** (code refuses, with the test that proves it) or **convention** (relied on, with the reason it is not enforced).

### Launch check

`run.start`, `run.relaunch` and the orchestrator launch run the memory launch check before the pane starts, inside the spawn transaction's preconditions:

1. List every loadable file for the role and worktree: L4 and L5 paths, including every `@` import reached and every nested `CLAUDE.md` and `.claude/rules/*.md` in the worktree.
2. Each file must map to a source-map row. An unmapped file refuses the launch.
3. Each file's bytes (worktree copy for project files, installed copy for dotfiles and foreign) must hash to an `approved` revision of its source; a `revoked` or `unreviewed` revision, or bytes matching no revision, refuse the launch.
4. Render the L3 prompt from the current approved blobs of the Tiller documents.
5. Verify L1 is off (the environment variable is set, or the fallback directory is empty and read-only) and L2 has no `memory` field.
6. Write `session_loads` and `config_hash`.

A refusal opens a `review` attention item on the card the run is for (for an orchestrator launch, on the host's `fleet-1` card, state.md Project) naming the file, its blob and what it failed. The owner accepts the external revision (one click, section 4), or Tiller reverts the worktree copy for a project file that drifted only in that worktree. The launch is retried after the answer.

### Guarantees

| Guarantee | Label | Test |
|---|---|---|
| A fleet launch with an unmapped loadable file is refused | enforced | `memory::launch::unmapped_file_refuses` |
| A fleet launch where any loadable file's bytes are not an approved revision is refused (worktree edit, uncommitted dotfiles edit, external commit, revoked revision) | enforced | `memory::launch::unapproved_bytes_refuse` (property test over source kinds and drift kinds) |
| The rendered role prompt is built only from approved blobs; a dirty working tree in the memory repo does not change it | enforced | `memory::render::reads_approved_blob_not_worktree` |
| Every fleet launch sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, or the fallback directory is empty and read-only | enforced | `agents::spawn::auto_memory_off` |
| No role or agent file Tiller launches has a `memory` field | enforced | `agents::roles::no_memory_field` |
| No `tiller` command output in a fleet session, no hook output and no rendered prompt contains a row of `evidence`, `proposals` or `observations` | enforced | `memory::isolation::unapproved_tables_never_rendered` (canary strings in each table, asserted absent from every rendered surface) |
| The API serves `evidence` and `proposals` only to the owner's board | enforced (T1.4 authority) | `api::authority::proposals_owner_only` |
| An approval whose diff or base blob no longer matches is `stale` and applies nothing | enforced | `memory::apply::moved_base_goes_stale` |
| Two applies on one repo serialize and commit only their own path | enforced | `memory::apply::concurrent_writers_do_not_bundle` |
| A commit that fails the scrub is never kept | enforced | `memory::scrub::flagged_commit_is_undone` |
| A proposal across the boundary, or at a `foreign` source, is refused | enforced | `memory::boundary::cross_boundary_proposal_refused` |
| A promotion without two independent evidence keys is refused | enforced | `memory::promote::needs_independent_evidence` |
| An approval that would put a role over its startup budget is refused | enforced | `agents::roles::startup_context_stays_under_budget` (P42) |
| A fleet role cannot write a source-map path in place with Edit or Write | enforced (permission deny rules in the role's settings fragment, honored without hooks) | `agents::guards::deny_memory_paths_edit` |
| A fleet role cannot write a source-map path through Bash | enforced where hooks fire (PreToolUse guard, T4.3); convention on a host whose hooks are blocked by managed policy, where the launch check and reconcile still catch the drift before the next launch | `agents::guards::bash_write_to_memory_path_refused` |
| A lazy file changed mid-session in the role's own worktree is not loaded by that session | convention: the two guards above stop the write; on a hooks-blocked host a Bash write could load into that one session before reconcile sees it. Reconcile then flags the run and the PR carries the memory-change flag | `reconcile::memory::worktree_drift_flags_run` |
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
  superseded_by     TEXT REFERENCES evidence(id)
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
  review_status           TEXT NOT NULL CHECK (review_status IN
                            ('approved', 'unreviewed', 'due', 'expired', 'retired')),
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
  CHECK (provenance <> 'legacy-import' OR legacy_id IS NOT NULL)
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
  PRIMARY KEY (session_id, revision_id, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);
```

The evidence requirement ("at least one evidence row") and the promotion rule span rows, so `mem.propose` checks them inside its transaction, not a `CHECK`. `meta` gains the key `boundary`.

Durability (state.md, section 10): `memory_sources`, `memory_revisions`, `memories`, `proposals`, `proposal_evidence`, `memory_evidence` and `evidence` are authoritative and backed up with `state.db` inside the host's boundary. `session_loads` is authoritative for the audit question it answers and is pruned with its session's retention. The memory files themselves are authoritative under git.

## 12. Mutations

Each follows state.md, section 8: one `BEGIN IMMEDIATE` transaction with its event rows.

| Mutation | Caller | Preconditions | Writes | Events |
|---|---|---|---|---|
| `mem.note` | fleet role (`tiller mem note`), tailer, retro, owner | boundary matches; card or session exists | `evidence` | `evidence.captured` |
| `mem.propose` | fleet role (`tiller mem propose`), curator, owner, reconcile (expiry, external edit) | target not `foreign`, not retired; boundaries match; base revision is current approved; at least one evidence row (not for `accept_external`, `retire`, `remove`); promotion rule; diff applies to `base_blob` | `proposals`, `proposal_evidence` | `proposal.created` |
| `mem.decide` | board (Keep, Discard), `tiller mem approve` (owner) | proposal `proposed`; base still current; budget rule; idempotency key new or identical | `approvals` (`kind = memory`, `accepted` or `rejected`), `proposals.state`, `approval_id` | `approval.decided`, `proposal.decided` |
| `mem.apply` | daemon only | approval `accepted`; blob and diff hash match under the repo lock | `approvals` (`executing`, then `succeeded`, `failed` or `stale`), `memory_revisions`, `memories`, `proposals` | `approval.executing`, `approval.finished`, `memory.applied` or `proposal.stale` |
| `mem.reconcile` | reconcile (T2.7) | none | `memory_revisions` (`external`, `unreviewed`), `memories` (line mapping), `memories.review_status` (`due`, `expired`), retire proposals for expired entries | `memory.revision_seen`, `memory.due`, `memory.expired`, `proposal.created` |
| `mem.accept_external` | board (owner) | revision `unreviewed`; scrub `passed` | `approvals` (decided by `owner`, bound to the revision's diff from its parent and the parent's blob), `memory_revisions.review = approved`, `memories.review_status` | `approval.decided`, `memory.accepted` |
| `mem.review` | board (owner) | memory `due` | `memories.review_due_at`, `reviewed_at`, `review_status = approved` | `memory.reviewed` |
| `mem.load` | `run.start` launch check; the daemon, on each `SessionStart` observation (the hook records the trigger and hashes nothing, protocol.md 4.2) | launch check passed | `session_loads`, `sessions.config_hash` | `session.memory_loaded` |
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

## Open for CP2

1. The launch check refuses on any unapproved byte, so an owner's hand edit to the dotfiles holds fleet launches until the owner accepts it with one click. The alternative is to trust commits signed by the owner's key; this draft does not, because fleet roles commit as the owner too.
2. A memory change to a PR-delivered project needs the memory approval and then the project's normal merge approval. A grant that lands an applied memory PR automatically would make it one click.
3. The default review interval for durable entries (180 days here) and whether it differs by class.
4. Whether the host's memory repo on a personal host pushes to a private remote, and which. This draft allows one, inside the boundary.
