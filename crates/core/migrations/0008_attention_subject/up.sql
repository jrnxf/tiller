-- Migration 8: an attention item can name what it is about (state.md,
-- Attention). `subject_session_id` is a live session the question asks the
-- owner to act on; `subject_project_id` is the project it is about. Neither
-- is a foreign key: a project delete withdraws the items about the project
-- and keeps the ids, so the page can still say what became of the subject.
--
-- Open recovery items whose evidence names a live session get that session
-- as their subject. Every open item whose evidence names a known session gets
-- that session's project; an item about an ended session names no session,
-- because it asks what to do after it ended.

ALTER TABLE attention ADD COLUMN subject_session_id TEXT;
ALTER TABLE attention ADD COLUMN subject_project_id TEXT;

UPDATE attention
SET subject_session_id = json_extract(evidence, '$.session_id')
WHERE kind = 'recovery' AND answered_at IS NULL AND withdrawn_at IS NULL
  AND evidence IS NOT NULL AND json_valid(evidence)
  AND json_type(evidence, '$.session_id') = 'text'
  AND json_extract(evidence, '$.session_id') IN (SELECT id FROM sessions WHERE ended_at IS NULL);

UPDATE attention
SET subject_project_id = (SELECT project_id FROM sessions WHERE id = attention.subject_session_id)
WHERE subject_session_id IS NOT NULL;

-- An item about a session that already ended still belongs to its project.
UPDATE attention
SET subject_project_id = (
  SELECT project_id FROM sessions WHERE id = json_extract(attention.evidence, '$.session_id')
)
WHERE subject_project_id IS NULL AND answered_at IS NULL AND withdrawn_at IS NULL
  AND evidence IS NOT NULL AND json_valid(evidence)
  AND json_type(evidence, '$.session_id') = 'text';

UPDATE meta SET value = '8' WHERE key = 'schema_version';
