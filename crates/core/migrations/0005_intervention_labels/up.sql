-- Migration 5: the owner's relabels of the Health page's intervention
-- classifier (state.md, Intervention; build-plan T7.1). The classifier itself
-- is computed from the rows it reads and stores nothing; this table holds
-- only the labels the owner set over it.

CREATE TABLE intervention_labels (
  ref_id      TEXT PRIMARY KEY,          -- an attention id, or the id of an owner note in the outbox
  label       TEXT NOT NULL CHECK (label IN
                ('correction', 'unplanned_escalation', 'rework', 'planned')),
  labeled_at  TEXT NOT NULL,
  labeled_by  TEXT NOT NULL              -- the actor, as in events.actor
) STRICT;

UPDATE meta SET value = '5' WHERE key = 'schema_version';
