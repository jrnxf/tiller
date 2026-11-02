-- Migration 1 down: drop every object migration 1 created, views first,
-- then tables in reverse creation order. Two pairs reference each other
-- (cards and asks, sessions and runs), so foreign key checks wait for the
-- commit, when nothing is left to check.

PRAGMA defer_foreign_keys = ON;

DROP VIEW capacity;
DROP VIEW asked;
DROP VIEW card_column;
DROP VIEW session_activity;
DROP TABLE session_loads;
DROP TABLE proposal_evidence;
DROP TABLE proposals;
DROP TABLE memory_evidence;
DROP TABLE memories;
DROP TABLE evidence;
DROP TABLE memory_revisions;
DROP TABLE memory_sources;
DROP TABLE quota_windows;
DROP TABLE worktrees;
DROP TABLE panes;
DROP TABLE handoffs;
DROP TABLE reviews;
DROP TABLE artifacts;
DROP TABLE obligations;
DROP TABLE events;
DROP TABLE observations;
DROP TABLE outbox;
DROP TABLE reservations;
DROP TABLE approvals;
DROP TABLE grants;
DROP TABLE prs;
DROP TABLE runs;
DROP TABLE attention;
DROP TABLE holds;
DROP TABLE card_deps;
DROP TABLE ask_cards;
DROP TABLE asks;
DROP TABLE sessions;
DROP TABLE cards;
DROP TABLE projects;
DROP TABLE hosts;
DROP TABLE idempotency_keys;
DROP TABLE meta;
