-- Migration 6 down: drop the learning loop. Evidence rows a retro wrote stay;
-- they are memory's (memory.md section 6).

DROP TABLE config_manifests;
DROP INDEX eval_runs_set;
DROP TABLE eval_runs;
DROP INDEX change_set_members_proposal;
DROP TABLE change_set_members;
DROP INDEX change_sets_open_name;
DROP TABLE change_sets;
DROP INDEX retros_pending;
DROP INDEX retros_one_per_trigger;
DROP INDEX retros_one_per_close;
DROP TABLE retros;

UPDATE meta SET value = '5' WHERE key = 'schema_version';
