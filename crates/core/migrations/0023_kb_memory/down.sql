-- Migration 23 down: the old memory tables come back empty, in their
-- schema 22 form. The rows they held are gone; the backup taken before the
-- migration keeps them. Pending notes and session notes are dropped, and a
-- retro that added a note reads as 'nothing', since schema 22 needs the
-- evidence or proposal row such an outcome named.

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
CREATE UNIQUE INDEX memory_revisions_content ON memory_revisions(source_id, blob_sha, coalesce(installed_sha256, ''));
CREATE INDEX memory_revisions_source ON memory_revisions(source_id, recorded_at);

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
CREATE INDEX memories_due ON memories(review_due_at) WHERE review_status = 'approved';
CREATE INDEX memories_source ON memories(source_id) WHERE review_status <> 'retired';

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
  PRIMARY KEY (session_id, revision_id, trigger, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);

CREATE TABLE change_sets (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  state           TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'measuring', 'closed')),
  window_cards    INTEGER NOT NULL CHECK (window_cards > 0),
  opened_at       TEXT NOT NULL,
  opened_by       TEXT NOT NULL,
  measuring_from  TEXT,
  closed_at       TEXT,
  close_reason    TEXT CHECK (close_reason IN ('window_complete', 'abandoned')),
  rev             INTEGER NOT NULL DEFAULT 1,
  CHECK ((state = 'closed') = (closed_at IS NOT NULL)),
  CHECK ((state = 'closed') = (close_reason IS NOT NULL)),
  CHECK (state = 'open' OR measuring_from IS NOT NULL OR close_reason = 'abandoned')
) STRICT;
CREATE UNIQUE INDEX change_sets_open_name ON change_sets(name) WHERE state <> 'closed';

CREATE TABLE change_set_members (
  change_set_id  TEXT NOT NULL REFERENCES change_sets(id),
  proposal_id    TEXT NOT NULL REFERENCES proposals(id),
  added_at       TEXT NOT NULL,
  PRIMARY KEY (change_set_id, proposal_id)
) STRICT;
CREATE INDEX change_set_members_proposal ON change_set_members(proposal_id);

CREATE TABLE eval_runs (
  id                TEXT PRIMARY KEY,
  change_set_id     TEXT NOT NULL REFERENCES change_sets(id),
  split             TEXT NOT NULL CHECK (split IN ('holdout')),
  members_sha256    TEXT NOT NULL,
  candidate_sha256  TEXT,
  cases_total       INTEGER NOT NULL,
  cases_passed      INTEGER NOT NULL,
  overall_score     REAL,
  cost_usd          REAL,
  partial           INTEGER NOT NULL DEFAULT 0 CHECK (partial IN (0, 1)),
  passed            INTEGER NOT NULL CHECK (passed IN (0, 1)),
  claude_version    TEXT,
  judge_model       TEXT,
  result_path       TEXT,                     -- local; never committed
  ran_at            TEXT NOT NULL,
  recorded_by       TEXT NOT NULL
) STRICT;
CREATE INDEX eval_runs_set ON eval_runs(change_set_id, ran_at);

DROP TABLE session_notes;
DROP INDEX memory_pending_project;
DROP TABLE memory_pending;

ALTER TABLE retros RENAME TO retros_v23;
DROP INDEX retros_one_per_close;
DROP INDEX retros_one_per_trigger;
DROP INDEX retros_pending;

CREATE TABLE retros (
  id                  TEXT PRIMARY KEY,
  card_id             TEXT NOT NULL REFERENCES cards(id),
  project_id          TEXT NOT NULL REFERENCES projects(id),
  closed_at           TEXT NOT NULL,            -- the close this retro is for
  trigger             TEXT REFERENCES cards(id), -- the fix card that re-opened it, or null
  depth               TEXT NOT NULL CHECK (depth IN ('light', 'full')),
  reason              TEXT NOT NULL CHECK (reason IN ('incident', 'surprising_win', 'sample', 'routine')),
  counts              TEXT NOT NULL DEFAULT '{}',   -- JSON, computed by the sweep
  config_hashes       TEXT NOT NULL DEFAULT '[]',   -- JSON list
  state               TEXT NOT NULL CHECK (state IN ('pending', 'done')),
  outcome             TEXT CHECK (outcome IN ('nothing', 'evidence', 'proposed', 'unfinished')),
  learning            TEXT,
  evidence_id         TEXT REFERENCES evidence(id),
  proposal_id         TEXT REFERENCES proposals(id),
  curator_session     TEXT,
  curator_started_at  TEXT,
  finished_at         TEXT,
  finished_by         TEXT,
  created_at          TEXT NOT NULL,
  rev                 INTEGER NOT NULL DEFAULT 1,
  CHECK ((state = 'done') = (outcome IS NOT NULL)),
  CHECK ((state = 'done') = (finished_at IS NOT NULL)),
  CHECK (depth = 'full' OR (state = 'done' AND outcome = 'nothing')),
  CHECK (outcome <> 'evidence' OR evidence_id IS NOT NULL),
  CHECK (outcome <> 'proposed' OR proposal_id IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX retros_one_per_close ON retros(card_id, closed_at) WHERE trigger IS NULL;
CREATE UNIQUE INDEX retros_one_per_trigger ON retros(trigger) WHERE trigger IS NOT NULL;
CREATE INDEX retros_pending ON retros(created_at) WHERE state = 'pending';

INSERT INTO retros (id, card_id, project_id, closed_at, trigger, depth, reason, counts, config_hashes,
                    state, outcome, learning, evidence_id, proposal_id, curator_session,
                    curator_started_at, finished_at, finished_by, created_at, rev)
SELECT id, card_id, project_id, closed_at, trigger, depth, reason, counts, config_hashes, state,
       CASE WHEN outcome = 'noted' THEN 'nothing' ELSE outcome END,
       learning, NULL, NULL, curator_session, curator_started_at, finished_at, finished_by,
       created_at, rev
FROM retros_v23 ORDER BY rowid;

DROP TABLE retros_v23;

UPDATE meta SET value = '22' WHERE key = 'schema_version';
