-- Migration 23: memory moves to kb (ADR 0005, docs/spec/memory.md).
-- The hook in migrate.rs first reads every approved tiller memory document
-- from the memory repo, then runs this file, then writes one pending note
-- per document line and the `memory.migrated` event, all in this one
-- transaction. The memory repo stays on disk.
--
-- Notes that agents suggest wait in memory_pending for the owner. A kept or
-- discarded row is deleted; its event keeps the record.

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
) STRICT;
CREATE INDEX memory_pending_project ON memory_pending(project_id, created_at);

-- One row per kb note a launch loaded. The trigger and time key each load,
-- so a resume, /clear or /compact copies the newest set as its own rows.
CREATE TABLE session_notes (
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  note_id     TEXT NOT NULL,                  -- the kb note id
  sha256      TEXT NOT NULL,                  -- of the rendered section
  trigger     TEXT NOT NULL CHECK (trigger IN ('startup', 'resume', 'clear', 'compact')),
  loaded_at   TEXT NOT NULL,
  PRIMARY KEY (session_id, note_id, trigger, loaded_at)
) STRICT;

-- Retros lose their links to evidence and proposals. A full retro ends in
-- nothing, one pending note, or unfinished; the note row goes when the
-- owner keeps or discards it, so pending_id is a plain reference. Old
-- 'evidence' and 'proposed' outcomes read as 'noted'.
ALTER TABLE retros RENAME TO retros_v22;
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
  outcome             TEXT CHECK (outcome IN ('nothing', 'noted', 'unfinished')),
  learning            TEXT,
  pending_id          TEXT,                     -- the memory_pending row it added
  curator_session     TEXT,
  curator_started_at  TEXT,
  finished_at         TEXT,
  finished_by         TEXT,
  created_at          TEXT NOT NULL,
  rev                 INTEGER NOT NULL DEFAULT 1,
  CHECK ((state = 'done') = (outcome IS NOT NULL)),
  CHECK ((state = 'done') = (finished_at IS NOT NULL)),
  CHECK (depth = 'full' OR (state = 'done' AND outcome = 'nothing'))
) STRICT;
CREATE UNIQUE INDEX retros_one_per_close ON retros(card_id, closed_at) WHERE trigger IS NULL;
CREATE UNIQUE INDEX retros_one_per_trigger ON retros(trigger) WHERE trigger IS NOT NULL;
CREATE INDEX retros_pending ON retros(created_at) WHERE state = 'pending';

INSERT INTO retros (id, card_id, project_id, closed_at, trigger, depth, reason, counts, config_hashes,
                    state, outcome, learning, pending_id, curator_session, curator_started_at,
                    finished_at, finished_by, created_at, rev)
SELECT id, card_id, project_id, closed_at, trigger, depth, reason, counts, config_hashes, state,
       CASE WHEN outcome IN ('evidence', 'proposed') THEN 'noted' ELSE outcome END,
       learning, NULL, curator_session, curator_started_at, finished_at, finished_by, created_at, rev
FROM retros_v22 ORDER BY rowid;

DROP TABLE retros_v22;

-- The old store: link tables first, so no row is left pointing at a
-- dropped table.
DROP INDEX change_set_members_proposal;
DROP TABLE change_set_members;
DROP INDEX eval_runs_set;
DROP TABLE eval_runs;
DROP INDEX change_sets_open_name;
DROP TABLE change_sets;
DROP INDEX session_loads_revision;
DROP TABLE session_loads;
DROP TABLE proposal_evidence;
DROP TABLE memory_evidence;
DROP INDEX proposals_open;
DROP TABLE proposals;
DROP INDEX memories_due;
DROP INDEX memories_source;
DROP TABLE memories;
DROP INDEX evidence_project;
DROP TABLE evidence;
DROP INDEX memory_revisions_content;
DROP INDEX memory_revisions_source;
DROP TABLE memory_revisions;
DROP INDEX memory_sources_loaded;
DROP TABLE memory_sources;

UPDATE meta SET value = '23' WHERE key = 'schema_version';
