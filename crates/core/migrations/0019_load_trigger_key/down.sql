-- Migration 19 down: key load rows by session, revision and time again.
-- Where two triggers loaded the same revision in the same millisecond, the
-- row written first is kept, so the session still reads that revision as
-- loaded at that moment.

ALTER TABLE session_loads RENAME TO session_loads_v19;
DROP INDEX session_loads_revision;

CREATE TABLE session_loads (
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  revision_id  TEXT NOT NULL REFERENCES memory_revisions(id),
  trigger      TEXT NOT NULL CHECK (trigger IN ('startup', 'resume', 'clear', 'compact')),
  loaded_at    TEXT NOT NULL,
  PRIMARY KEY (session_id, revision_id, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);

INSERT OR IGNORE INTO session_loads (session_id, revision_id, trigger, loaded_at)
SELECT session_id, revision_id, trigger, loaded_at FROM session_loads_v19 ORDER BY rowid;

DROP TABLE session_loads_v19;

UPDATE meta SET value = '18' WHERE key = 'schema_version';
