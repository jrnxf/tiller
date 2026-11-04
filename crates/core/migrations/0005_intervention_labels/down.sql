-- Migration 5 down: the owner's relabels are dropped; the classifier's own
-- labels are recomputed from the rows, so only the overrides are lost.

DROP TABLE intervention_labels;

UPDATE meta SET value = '4' WHERE key = 'schema_version';
