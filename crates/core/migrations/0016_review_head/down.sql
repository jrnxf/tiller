-- Migration 16 down: forget which commit each review covered. Every review
-- then reads as it did before 16.

ALTER TABLE reviews DROP COLUMN head_sha;

UPDATE meta SET value = '15' WHERE key = 'schema_version';
