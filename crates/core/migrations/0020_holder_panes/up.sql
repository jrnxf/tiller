-- Migration 20: the terminal holder's state (ADR 0004; terminal-holder.md
-- 11.1, task H10).
--
-- The panes cache is rebuilt with the holder's columns, and every herdr row
-- goes: no herdr pane outlives the cutover. Until crates/herdr is deleted
-- (H13), the herdr poller still writes its local panes here, with no
-- holder instance. No table references panes.
--
-- The observations source check gains 'holder'. A check cannot change in
-- place, so the table is rebuilt in rowid order; old rows keep 'herdr' as
-- history. No table references observations.
--
-- hosts.herdr_machine is dropped by the migration's hook, after it checks
-- that no remote host is a saved herdr machine with no peer pairing.

DROP TABLE panes;

CREATE TABLE panes (                      -- owner: the terminal holder
  pane_id          TEXT NOT NULL,         -- the holder's pane id
  host_id          TEXT NOT NULL REFERENCES hosts(id),
  holder_instance  TEXT,                  -- the holder instance that reported it; null for a herdr pane
  session_id       TEXT REFERENCES sessions(id), -- from the pane's key
  pid              INTEGER,               -- the pane's child
  label            TEXT,                  -- meta.label
  state            TEXT NOT NULL CHECK (state IN ('running', 'exited')),
  exit_code        INTEGER,
  exit_signal      INTEGER,
  exited_at        TEXT,
  metadata         TEXT NOT NULL DEFAULT '{}', -- the pane's meta, JSON
  fetched_at       TEXT NOT NULL,
  gone_at          TEXT,
  PRIMARY KEY (host_id, pane_id),
  CHECK (state = 'exited' OR (exit_code IS NULL AND exit_signal IS NULL AND exited_at IS NULL))
) STRICT;

ALTER TABLE observations RENAME TO observations_v19;
DROP INDEX observations_session;
DROP INDEX observations_recorded;

CREATE TABLE observations (
  id           TEXT PRIMARY KEY,
  session_id   TEXT REFERENCES sessions(id),   -- null until the source can be mapped
  run_id       TEXT REFERENCES runs(id),
  source       TEXT NOT NULL CHECK (source IN ('hook', 'transcript', 'herdr', 'holder', 'liveness', 'status_line')),
  event        TEXT NOT NULL,                  -- hook event or detected kind; mapping in protocol.md
  verdict      TEXT CHECK (verdict IN
                 ('working', 'idle', 'waiting_permission', 'waiting_input', 'errored', 'ended', 'unknown')),
  observed_at  TEXT NOT NULL,
  recorded_at  TEXT NOT NULL,
  payload      TEXT NOT NULL DEFAULT '{}'      -- JSON; unknown fields are kept
) STRICT;
CREATE INDEX observations_session ON observations(session_id, observed_at);
CREATE INDEX observations_recorded ON observations(recorded_at);   -- retention

INSERT INTO observations (id, session_id, run_id, source, event, verdict, observed_at, recorded_at, payload)
SELECT id, session_id, run_id, source, event, verdict, observed_at, recorded_at, payload
FROM observations_v19 ORDER BY rowid;

DROP TABLE observations_v19;

UPDATE meta SET value = '20' WHERE key = 'schema_version';
