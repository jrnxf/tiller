-- Migration 22: the orchestrator chat thread (docs/plan/orchestrator-chat.md).
-- A session keeps the context use of its newest transcript usage row: the
-- tokens of the prompt (input + cache read + cache creation), the window of
-- its model, and the transcript time of that row. The transcript tailer
-- writes them with no event and no rev bump, as it writes `activity`.
-- The thread reads a project's events by the orchestrator sessions that
-- wrote them, so the log gets an index by actor.

ALTER TABLE sessions ADD COLUMN context_used INTEGER;
ALTER TABLE sessions ADD COLUMN context_window INTEGER;
ALTER TABLE sessions ADD COLUMN context_at TEXT;

CREATE INDEX events_actor ON events(actor, seq);

UPDATE meta SET value = '22' WHERE key = 'schema_version';
