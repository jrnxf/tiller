-- Migration 4 down: drop the pairing records. A paired host's row stays, as
-- the herdr machine it was before pairing.

DROP INDEX peer_keys_one_live;
DROP TABLE peer_keys;
-- peer_host_id first: its check names peer_scope
ALTER TABLE hosts DROP COLUMN peer_host_id;
ALTER TABLE hosts DROP COLUMN peer_scope;

UPDATE meta SET value = '3' WHERE key = 'schema_version';
