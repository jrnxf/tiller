-- Migration 13: the host's bug project (state.md, Host). `tiller bug file`
-- files a Tiller bug there while it names an open project this host owns,
-- else in `fleet`. Every host starts with none, so every bug keeps landing in
-- `fleet` until the owner picks one.

ALTER TABLE hosts ADD COLUMN bug_project_id TEXT REFERENCES projects(id);

UPDATE meta SET value = '13' WHERE key = 'schema_version';
