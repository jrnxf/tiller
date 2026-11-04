-- Migration 9 down: the publish records are dropped; a direct-pr builder
-- pushes and opens its own PR again.

DROP TABLE publishes;

UPDATE meta SET value = '8' WHERE key = 'schema_version';
