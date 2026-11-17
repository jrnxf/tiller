-- Migration 22 down: forget the sessions' context use and the actor index.

DROP INDEX events_actor;

ALTER TABLE sessions DROP COLUMN context_at;
ALTER TABLE sessions DROP COLUMN context_window;
ALTER TABLE sessions DROP COLUMN context_used;

UPDATE meta SET value = '21' WHERE key = 'schema_version';
