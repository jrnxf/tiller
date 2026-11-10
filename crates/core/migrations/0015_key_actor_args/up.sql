-- Migration 15: who first used an idempotency key, and with what arguments
-- (state.md, Bookkeeping; protocol.md 6.3). A reused key from another actor,
-- or with other arguments, is refused instead of returning the first
-- request's result. `args_sha256` is the SHA-256 of the normalized arguments
-- when the caller sent them. A key stored before this has neither, and both
-- checks pass for it until it expires.

ALTER TABLE idempotency_keys ADD COLUMN actor TEXT;
ALTER TABLE idempotency_keys ADD COLUMN args_sha256 TEXT;

UPDATE meta SET value = '15' WHERE key = 'schema_version';
