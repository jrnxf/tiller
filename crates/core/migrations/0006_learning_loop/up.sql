-- Migration 6: the learning loop (docs/design/learning-loop.md, T7.3).
-- Retros per closed card, change sets (one change or a named bundle), the
-- holdout eval runs that gate an approval, and the config manifests behind
-- every run's config_hash. Nothing here is ever rendered into a fleet role.

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
-- one retro per close, plus one per fix card that re-opens it
CREATE UNIQUE INDEX retros_one_per_close ON retros(card_id, closed_at) WHERE trigger IS NULL;
CREATE UNIQUE INDEX retros_one_per_trigger ON retros(trigger) WHERE trigger IS NOT NULL;
CREATE INDEX retros_pending ON retros(created_at) WHERE state = 'pending';

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
-- a proposal is in at most one set that is not closed: change_set.add checks
-- it across rows, since a closed set keeps its members as history
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

CREATE TABLE config_manifests (
  config_hash    TEXT PRIMARY KEY,
  parts          TEXT NOT NULL,              -- JSON: part -> value
  first_seen_at  TEXT NOT NULL
) STRICT;

UPDATE meta SET value = '6' WHERE key = 'schema_version';
