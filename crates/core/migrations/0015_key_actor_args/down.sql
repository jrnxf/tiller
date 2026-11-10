-- Migration 15 down: forget who used each idempotency key and with what
-- arguments, so a key replays on its mutation name alone.

ALTER TABLE idempotency_keys DROP COLUMN args_sha256;
ALTER TABLE idempotency_keys DROP COLUMN actor;

UPDATE meta SET value = '14' WHERE key = 'schema_version';
