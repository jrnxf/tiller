-- Migration 7 down: back to hyphen-free project ids of at most 16
-- characters. The down hook in migrate.rs runs first: it refuses while any
-- project id needs the wider rule, then restores the old check's text.

UPDATE meta SET value = '6' WHERE key = 'schema_version';
