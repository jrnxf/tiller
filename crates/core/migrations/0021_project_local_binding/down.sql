-- Migration 21 down: forget the local binding opt-in. Every builder and
-- verifier launched after this runs with local ports closed again.

ALTER TABLE projects DROP COLUMN local_binding;

UPDATE meta SET value = '20' WHERE key = 'schema_version';
