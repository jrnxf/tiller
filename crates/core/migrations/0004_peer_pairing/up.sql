-- Migration 4: pairing records for the federated board (ADR 0003; state.md
-- sections 3 and 8). On the board host, a paired peer's scope and host id
-- on its `hosts` row; on the peer, one `peer_keys` row per board host it
-- accepted. The private signing key is a file, never a column.
--
-- The checks are column checks, not the table checks of schema.sql, because
-- ADD COLUMN cannot add a table check and DROP COLUMN refuses a column that
-- another column's check names. They hold the same rules: only a remote host
-- is paired, and the scope and the host id are set together.

ALTER TABLE hosts ADD COLUMN peer_scope TEXT
  CHECK (peer_scope IS NULL OR (peer_scope IN ('read', 'act') AND kind = 'remote'));
-- the peer's meta.host_id, checked on every hello
ALTER TABLE hosts ADD COLUMN peer_host_id TEXT
  CHECK ((peer_scope IS NULL) = (peer_host_id IS NULL));

CREATE TABLE peer_keys (
  id              TEXT PRIMARY KEY,         -- ULID, the key id in every frame
  viewer_label    TEXT NOT NULL,
  viewer_host_id  TEXT NOT NULL,            -- the board host's meta.host_id
  public_key      TEXT NOT NULL,            -- Ed25519, base64
  scope           TEXT NOT NULL CHECK (scope IN ('read', 'act')),
  accepted_at     TEXT NOT NULL,
  revoked_at      TEXT,
  rev             INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX peer_keys_one_live ON peer_keys(viewer_host_id) WHERE revoked_at IS NULL;

UPDATE meta SET value = '4' WHERE key = 'schema_version';
