-- Migration 7: project ids may hold single hyphens and run to 32 characters
-- (state.md section 2), so `acme-web2-staging` can match its repo. Every
-- existing id already fits the wider rule, so no row changes.
--
-- SQLite cannot alter a table check, and rebuilding `projects` would mean
-- turning foreign keys off, which cannot happen inside the migration's
-- transaction. The new check only accepts more ids and changes nothing on
-- disk, so the up hook in migrate.rs rewrites the check's text in
-- sqlite_schema, the documented path for such a change
-- (https://www.sqlite.org/lang_altertable.html, section 7). This file runs
-- before the hook.

UPDATE meta SET value = '7' WHERE key = 'schema_version';
