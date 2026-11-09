-- Migration 14: what a session was launched with (state.md, Session). The
-- tiller version and a hash of the role prompt, settings template and MCP
-- config the launch wrote let the board and `tiller agent restart --stale`
-- tell a session on an older tiller from one on the running one. A session
-- launched before this has neither, and counts as stale with version
-- "unknown". `restart_queued_at` is a person's request to restart a working
-- session once it goes idle; it survives a daemon restart with the row.

ALTER TABLE sessions ADD COLUMN tiller_version TEXT;
ALTER TABLE sessions ADD COLUMN launch_hash TEXT;
ALTER TABLE sessions ADD COLUMN restart_queued_at TEXT;

UPDATE meta SET value = '14' WHERE key = 'schema_version';
