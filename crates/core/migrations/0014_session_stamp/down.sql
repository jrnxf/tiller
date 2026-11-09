-- Migration 14 down: forget what sessions were launched with, so none counts
-- as stale and no restart waits.

ALTER TABLE sessions DROP COLUMN restart_queued_at;
ALTER TABLE sessions DROP COLUMN launch_hash;
ALTER TABLE sessions DROP COLUMN tiller_version;

UPDATE meta SET value = '13' WHERE key = 'schema_version';
