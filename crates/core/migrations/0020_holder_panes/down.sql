-- Migration 20 down: back to the herdr shape. The hook refuses first while
-- an observation came from the holder. The holder's panes rows go, since
-- schema 19's panes are herdr's, and every host comes back with no saved
-- herdr machine: the ones the migration dropped are gone (terminal-holder.md
-- 11.3, a one-way door for state).
--
-- hosts is rebuilt so its text reads as schema 19's: ADD COLUMN would put
-- herdr_machine last. A rename would rewrite every other table's reference
-- to hosts, so the rows go to a temporary copy, and the table is dropped and
-- made again under its own name. The drop leaves the rows that reference a
-- host without their parent until the copy is back, so the foreign keys are
-- checked at commit, not at once. No reference cascades.

ALTER TABLE observations RENAME TO observations_v20;
DROP INDEX observations_session;
DROP INDEX observations_recorded;

CREATE TABLE observations (
  id           TEXT PRIMARY KEY,
  session_id   TEXT REFERENCES sessions(id),   -- null until the source can be mapped
  run_id       TEXT REFERENCES runs(id),
  source       TEXT NOT NULL CHECK (source IN ('hook', 'transcript', 'herdr', 'liveness', 'status_line')),
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
FROM observations_v20 ORDER BY rowid;

DROP TABLE observations_v20;

DROP TABLE panes;

CREATE TABLE panes (                      -- owner: herdr
  pane_id          TEXT NOT NULL,
  host_id          TEXT NOT NULL REFERENCES hosts(id),
  workspace_label  TEXT,
  agent_name       TEXT,
  agent_status     TEXT,                  -- herdr's own word, stored verbatim
  session_id       TEXT REFERENCES sessions(id),
  metadata         TEXT NOT NULL DEFAULT '{}', -- JSON row tokens
  fetched_at       TEXT NOT NULL,
  gone_at          TEXT,
  PRIMARY KEY (host_id, pane_id)
) STRICT;

CREATE TEMP TABLE hosts_v20 AS SELECT rowid AS row, * FROM hosts;
PRAGMA defer_foreign_keys = ON;
DROP TABLE hosts;

CREATE TABLE hosts (
  id             TEXT PRIMARY KEY,
  label          TEXT NOT NULL UNIQUE,     -- role label, e.g. host-a
  kind           TEXT NOT NULL CHECK (kind IN ('local', 'remote')),
  herdr_machine  TEXT,                     -- saved machine id; null for local
  lane_capacity  INTEGER,                  -- local only: max active runs
  ram_floor_mb   INTEGER,                  -- spawn refuses below these floors
  disk_floor_mb  INTEGER,
  -- cache (herdr poller): when this host was last reachable and why not
  last_seen_at   TEXT,
  stale_reason   TEXT,
  rev            INTEGER NOT NULL DEFAULT 1, peer_scope TEXT
  CHECK (peer_scope IS NULL OR (peer_scope IN ('read', 'act') AND kind = 'remote')), peer_host_id TEXT
  CHECK ((peer_scope IS NULL) = (peer_host_id IS NULL)), bug_project_id TEXT REFERENCES projects(id),
  CHECK (kind <> 'local' OR lane_capacity IS NOT NULL)
) STRICT;

INSERT INTO hosts (id, label, kind, lane_capacity, ram_floor_mb, disk_floor_mb, last_seen_at,
                   stale_reason, rev, peer_scope, peer_host_id, bug_project_id)
SELECT id, label, kind, lane_capacity, ram_floor_mb, disk_floor_mb, last_seen_at,
       stale_reason, rev, peer_scope, peer_host_id, bug_project_id
FROM hosts_v20 ORDER BY row;

DROP TABLE hosts_v20;

UPDATE meta SET value = '19' WHERE key = 'schema_version';
