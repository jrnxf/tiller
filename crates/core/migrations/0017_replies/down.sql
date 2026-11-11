-- Migration 17 down: the replies are dropped; an answer lives only in the
-- orchestrator's pane again.

DROP TABLE replies;

UPDATE meta SET value = '16' WHERE key = 'schema_version';
