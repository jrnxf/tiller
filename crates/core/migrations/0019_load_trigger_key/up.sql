-- Migration 19: a session's load rows are keyed by trigger too (memory.md
-- section 7). Before this, a resume, /clear or /compact copied in the same
-- millisecond as another load of the session hit the old key, wrote
-- nothing, and stayed owed. Now each trigger gets its own rows.
--
-- SQLite cannot change a primary key in place, so the table is rebuilt in
-- rowid order: a load set reads in the order it was written. The old table
-- moves aside first, so the new one is created under its own name. No other
-- table references session_loads.

ALTER TABLE session_loads RENAME TO session_loads_v18;
DROP INDEX session_loads_revision;

CREATE TABLE session_loads (
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  revision_id  TEXT NOT NULL REFERENCES memory_revisions(id),
  trigger      TEXT NOT NULL CHECK (trigger IN ('startup', 'resume', 'clear', 'compact')),
  loaded_at    TEXT NOT NULL,
  PRIMARY KEY (session_id, revision_id, trigger, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);

INSERT INTO session_loads (session_id, revision_id, trigger, loaded_at)
SELECT session_id, revision_id, trigger, loaded_at FROM session_loads_v18 ORDER BY rowid;

DROP TABLE session_loads_v18;

UPDATE meta SET value = '19' WHERE key = 'schema_version';
