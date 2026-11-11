-- Migration 17: the orchestrator's reply to an ask or a card comment
-- (state.md, Reply). The orchestrator sends it with `tiller reply`, so the
-- board stores only what it chose to send as the answer. No hook writes a
-- reply: hooks stay observations. A reply answers one ask or one comment
-- (an outbox note on a card), never both.

CREATE TABLE replies (
  id          TEXT PRIMARY KEY NOT NULL,
  project_id  TEXT NOT NULL REFERENCES projects(id),
  ask_id      TEXT REFERENCES asks(id),         -- the ask it answers
  message_id  TEXT REFERENCES outbox(id),       -- or the card comment it answers
  card_id     TEXT REFERENCES cards(id),        -- the card it links to
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  body        TEXT NOT NULL,                    -- work data; never exported to git
  replied_at  TEXT NOT NULL,
  read_at     TEXT,                             -- when the owner marked it read
  rev         INTEGER NOT NULL DEFAULT 1,
  CHECK ((ask_id IS NULL) <> (message_id IS NULL))
) STRICT;
CREATE INDEX replies_unread ON replies(project_id, replied_at) WHERE read_at IS NULL;
CREATE INDEX replies_ask ON replies(ask_id) WHERE ask_id IS NOT NULL;

UPDATE meta SET value = '17' WHERE key = 'schema_version';
