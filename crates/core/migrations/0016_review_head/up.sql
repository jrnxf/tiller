-- Migration 16: the commit a review covered (authority.md section 6,
-- Standing grants). A grant's `reviews` condition counts only a review whose
-- `head_sha` is the PR head it lands, so a push after a review needs a new
-- review before a grant accepts it. A review recorded before this has no
-- `head_sha` and never satisfies a grant; it stays evidence for the owner.
-- The check is a column check, because ALTER TABLE cannot add a table check.

ALTER TABLE reviews ADD COLUMN head_sha TEXT
  CHECK (head_sha IS NULL OR (length(head_sha) IN (40, 64) AND head_sha NOT GLOB '*[^0-9a-f]*'));

UPDATE meta SET value = '16' WHERE key = 'schema_version';
