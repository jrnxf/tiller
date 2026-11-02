-- Migration 1: the state.db schema as approved at CP2 (Monday 2026-11-02).
--
-- docs/spec/schema.sql followed by the memory tables of docs/spec/memory.md,
-- section 11. The connection pragmas (WAL, synchronous, foreign_keys,
-- busy_timeout) are set by Store::open, since journal_mode cannot change
-- inside the migration's transaction.

-- ---------------------------------------------------------------------------
-- Bookkeeping
-- ---------------------------------------------------------------------------

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
-- keys: schema_version, host_id, created_at, import_manifest_sha256,
--       boundary (memory.md), boot_id (recovery.md)

CREATE TABLE idempotency_keys (
  key        TEXT PRIMARY KEY,
  command    TEXT NOT NULL,
  entity     TEXT,
  entity_id  TEXT,
  response   TEXT NOT NULL,              -- JSON, returned verbatim on replay
  created_at TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- Hosts and projects
-- ---------------------------------------------------------------------------

CREATE TABLE hosts (
  id             TEXT PRIMARY KEY,
  label          TEXT NOT NULL UNIQUE,     -- role label, e.g. host-a
  kind           TEXT NOT NULL CHECK (kind IN ('local', 'remote')),
  herdr_machine  TEXT,                     -- saved machine id; null for local
  lane_capacity  INTEGER,                  -- local only: max active runs
  ram_floor_mb   INTEGER,                  -- spawn refuses below these floors
  disk_floor_mb  INTEGER,
  -- cache (herdr poller): when this host was last reachable and why not
  last_seen_at   TEXT,
  stale_reason   TEXT,
  rev            INTEGER NOT NULL DEFAULT 1,
  CHECK (kind <> 'local' OR lane_capacity IS NOT NULL)
) STRICT;

CREATE TABLE projects (
  id             TEXT PRIMARY KEY CHECK (id GLOB '[a-z][a-z0-9]*' AND length(id) BETWEEN 2 AND 16),
  name           TEXT NOT NULL,
  repo           TEXT,                     -- owner/name; null for local-only
  remote         TEXT,                     -- git remote URL
  default_branch TEXT NOT NULL DEFAULT 'main',
  delivery       TEXT NOT NULL CHECK (delivery IN
                   ('no-mistakes', 'no-mistakes-prod-only', 'direct-pr', 'direct-push', 'local-only')),
  checks         TEXT NOT NULL DEFAULT '[]', -- JSON: commands a direct push must run
  capacity       INTEGER NOT NULL DEFAULT 2, -- max active runs for this project
  -- deploy watch for direct-push projects (state.md, section 7; CP2, C02 and S07)
  deploy_workflow        TEXT,             -- workflow file, or null for no watch
  deploy_environment     TEXT,             -- GitHub environment the run must reach, or null
  deploy_timeout_minutes INTEGER NOT NULL DEFAULT 60 CHECK (deploy_timeout_minutes > 0),
  -- enforced only while the daemon's ruleset check holds (authority.md, section 6; CP2, S03)
  merge_gate     TEXT NOT NULL DEFAULT 'detect' CHECK (merge_gate IN ('enforced', 'detect')),
  owner_host_id  TEXT NOT NULL REFERENCES hosts(id),
  added_at       TEXT NOT NULL,
  next_card_seq  INTEGER NOT NULL DEFAULT 1, -- read and bumped inside card.create
  retired_at     TEXT,
  rev            INTEGER NOT NULL DEFAULT 1
) STRICT;

-- ---------------------------------------------------------------------------
-- Cards and what points at them
-- ---------------------------------------------------------------------------

CREATE TABLE cards (
  id                 TEXT PRIMARY KEY,
  display_id         TEXT NOT NULL UNIQUE,   -- alpha-42, minted in card.create
  legacy_id          TEXT,                   -- Firstmate task id on import
  project_id         TEXT NOT NULL REFERENCES projects(id),
  kind               TEXT NOT NULL CHECK (kind IN ('ship', 'scout', 'question', 'ops')),
  title              TEXT NOT NULL,
  body               TEXT,
  priority           INTEGER NOT NULL DEFAULT 2 CHECK (priority BETWEEN 0 AND 4),
  category           TEXT,
  size               TEXT CHECK (size IN ('S', 'M', 'L')),
  acceptance         TEXT,
  fixes_card_id      TEXT REFERENCES cards(id),  -- a later defect points back
  dispatch_override  TEXT,                   -- JSON {model, effort}; owner only (protocol.md 6.4; CP2, C04)
  state              TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
  outcome            TEXT CHECK (outcome IN
                       ('landed', 'reported', 'answered', 'completed',
                        'cancelled', 'failed', 'abandoned', 'superseded')),
  closed_at          TEXT,
  closed_by          TEXT,                   -- session:<id>, board, daemon, owner
  created_at         TEXT NOT NULL,
  created_by_ask_id  TEXT REFERENCES asks(id),  -- forward reference; SQLite checks it on write
  rev                INTEGER NOT NULL DEFAULT 1,
  CHECK ((state = 'open' AND outcome IS NULL AND closed_at IS NULL)
      OR (state = 'closed' AND outcome IS NOT NULL AND closed_at IS NOT NULL))
) STRICT;
CREATE INDEX cards_project_state ON cards(project_id, state);
CREATE UNIQUE INDEX cards_legacy ON cards(project_id, legacy_id) WHERE legacy_id IS NOT NULL;

CREATE TABLE sessions (
  id                    TEXT PRIMARY KEY,
  claude_session_id     TEXT UNIQUE,         -- null until SessionStart binds it
  role                  TEXT NOT NULL CHECK (role IN
                          ('orchestrator', 'scout', 'builder', 'reviewer', 'verifier')),
  project_id            TEXT NOT NULL REFERENCES projects(id),
  run_id                TEXT REFERENCES runs(id), -- null for an orchestrator
  host_id               TEXT NOT NULL REFERENCES hosts(id),
  pane_id               TEXT,                -- cache (herdr)
  transcript_path       TEXT,
  harness               TEXT NOT NULL DEFAULT 'claude',  -- protocol.md section 10
  config_hash           TEXT,
  started_at            TEXT NOT NULL,
  ended_at              TEXT,
  end_reason            TEXT,
  -- projection of the newest observations row, updated in its transaction
  activity              TEXT NOT NULL DEFAULT 'unknown' CHECK (activity IN
                          ('working', 'idle', 'waiting_permission', 'waiting_input',
                           'errored', 'ended', 'unknown')),
  activity_observed_at  TEXT,
  activity_source       TEXT,
  rev                   INTEGER NOT NULL DEFAULT 1
) STRICT;
-- at most one live orchestrator per project
CREATE UNIQUE INDEX sessions_live_orchestrator ON sessions(project_id)
  WHERE role = 'orchestrator' AND ended_at IS NULL;
-- at most one live session per run: a relaunch or resume ends the old row
-- in the transaction that inserts the new one
CREATE UNIQUE INDEX sessions_live_run ON sessions(run_id)
  WHERE run_id IS NOT NULL AND ended_at IS NULL;
CREATE INDEX sessions_run ON sessions(run_id);

CREATE TABLE asks (
  id                      TEXT PRIMARY KEY,
  project_id              TEXT NOT NULL REFERENCES projects(id),
  session_id              TEXT REFERENCES sessions(id),   -- null for a script note
  source                  TEXT NOT NULL CHECK (source IN ('prompt', 'board', 'script', 'import')),
  text                    TEXT NOT NULL,                  -- work data; never exported to git
  received_at             TEXT NOT NULL,
  disposition             TEXT NOT NULL DEFAULT 'unresolved' CHECK (disposition IN
                            ('unresolved', 'new', 'followup', 'chat', 'cancelled')),
  resolved_at             TEXT,
  resolved_by_session_id  TEXT REFERENCES sessions(id),
  CHECK ((disposition = 'unresolved') = (resolved_at IS NULL))
) STRICT;
CREATE INDEX asks_unresolved ON asks(project_id, received_at) WHERE disposition = 'unresolved';

CREATE TABLE ask_cards (
  ask_id               TEXT NOT NULL REFERENCES asks(id),
  card_id              TEXT NOT NULL REFERENCES cards(id),
  relation             TEXT NOT NULL CHECK (relation IN ('created', 'followup')),
  linked_at            TEXT NOT NULL,
  linked_by_session_id TEXT REFERENCES sessions(id),
  PRIMARY KEY (ask_id, card_id)
) STRICT;
CREATE INDEX ask_cards_card ON ask_cards(card_id);

CREATE TABLE card_deps (
  card_id             TEXT NOT NULL REFERENCES cards(id),
  depends_on_card_id  TEXT NOT NULL REFERENCES cards(id),
  added_at            TEXT NOT NULL,
  added_by            TEXT NOT NULL,
  released_at         TEXT,
  release_reason      TEXT CHECK (release_reason IN
                        ('landed', 'reported', 'answered', 'completed', 'removed')),
  PRIMARY KEY (card_id, depends_on_card_id),
  CHECK (card_id <> depends_on_card_id),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
) STRICT;
CREATE INDEX card_deps_open ON card_deps(depends_on_card_id) WHERE released_at IS NULL;

CREATE TABLE holds (
  id              TEXT PRIMARY KEY,
  card_id         TEXT NOT NULL REFERENCES cards(id),
  until_at        TEXT NOT NULL,          -- re-ask moment, UTC
  reason          TEXT NOT NULL,
  set_at          TEXT NOT NULL,
  set_by          TEXT NOT NULL,
  released_at     TEXT,
  release_reason  TEXT CHECK (release_reason IN ('due', 'answered', 'removed')),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
) STRICT;
-- at most one open hold per card
CREATE UNIQUE INDEX holds_open ON holds(card_id) WHERE released_at IS NULL;

CREATE TABLE attention (
  id                      TEXT PRIMARY KEY,
  card_id                 TEXT NOT NULL REFERENCES cards(id),
  kind                    TEXT NOT NULL CHECK (kind IN
                            ('decision', 'credential', 'merge', 'review', 'recovery', 'report')),
  question                TEXT NOT NULL,
  evidence                TEXT,              -- text, or JSON pointers (event ids, artifacts, urls)
  recommendation          TEXT,
  consequence             TEXT,
  options                 TEXT,              -- JSON list, optional
  opened_at               TEXT NOT NULL,
  opened_by_session_id    TEXT REFERENCES sessions(id),
  opened_by_run_id        TEXT REFERENCES runs(id),
  answered_at             TEXT,
  answer                  TEXT,              -- the owner's exact words
  answered_via            TEXT CHECK (answered_via IN ('board', 'chat', 'cli')),
  answer_idempotency_key  TEXT UNIQUE,
  withdrawn_at            TEXT,
  withdraw_reason         TEXT,
  rev                     INTEGER NOT NULL DEFAULT 1,
  CHECK ((answered_at IS NULL) = (answer IS NULL)),
  CHECK ((answered_at IS NULL) = (answered_via IS NULL)),
  -- a replay with the same key returns the stored response through
  -- idempotency_keys; a different answer to an answered item is refused by
  -- attention.answer's precondition, since one row holds one answer
  CHECK (answered_at IS NULL OR withdrawn_at IS NULL)
) STRICT;
CREATE INDEX attention_open ON attention(card_id, opened_at)
  WHERE answered_at IS NULL AND withdrawn_at IS NULL;

-- ---------------------------------------------------------------------------
-- Runs, PRs, approvals, reservations
-- ---------------------------------------------------------------------------

CREATE TABLE runs (
  id                     TEXT PRIMARY KEY,
  card_id                TEXT NOT NULL REFERENCES cards(id),
  role                   TEXT NOT NULL CHECK (role IN ('scout', 'builder', 'reviewer', 'verifier')),
  attempt                INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
  predecessor_run_id     TEXT REFERENCES runs(id),
  model                  TEXT NOT NULL,
  effort                 TEXT NOT NULL CHECK (effort IN ('low', 'medium', 'high', 'xhigh', 'max')),
  host_id                TEXT NOT NULL REFERENCES hosts(id),
  worktree_path          TEXT,
  branch                 TEXT,
  base_branch            TEXT,
  base_sha               TEXT,
  supervisor_session_id  TEXT NOT NULL REFERENCES sessions(id), -- exactly one owner at a time
  started_at             TEXT NOT NULL,
  ended_at               TEXT,
  outcome                TEXT CHECK (outcome IN
                           ('landed', 'reported', 'failed', 'abandoned', 'relaunched', 'cancelled', 'cut')),
  outcome_note           TEXT,
  pause_until            TEXT,               -- a dated worker pause: no alarms before it
  last_status_line       TEXT,               -- projection of the newest status observation
  last_status_at         TEXT,
  config_hash            TEXT,
  idempotency_key        TEXT UNIQUE,
  rev                    INTEGER NOT NULL DEFAULT 1,
  CHECK ((ended_at IS NULL) = (outcome IS NULL))
) STRICT;
-- one active run per card and role
CREATE UNIQUE INDEX runs_active_role ON runs(card_id, role) WHERE ended_at IS NULL;
CREATE UNIQUE INDEX runs_attempt ON runs(card_id, role, attempt);
CREATE INDEX runs_supervisor ON runs(supervisor_session_id) WHERE ended_at IS NULL;

CREATE TABLE prs (
  id              TEXT PRIMARY KEY,
  project_id      TEXT NOT NULL REFERENCES projects(id),
  card_id         TEXT NOT NULL REFERENCES cards(id),
  run_id          TEXT REFERENCES runs(id),   -- null when reconcile found the PR
  number          INTEGER NOT NULL,           -- in projects.repo
  url             TEXT NOT NULL,
  linked_at       TEXT NOT NULL,
  linked_by       TEXT NOT NULL,
  -- cache (GitHub through gh-axi); fetched_at says how old it is
  state           TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'merged', 'closed')),
  head_sha        TEXT,
  base_ref        TEXT,
  checks          TEXT NOT NULL DEFAULT 'none' CHECK (checks IN ('pending', 'success', 'failure', 'none')),
  review_decision TEXT NOT NULL DEFAULT 'none' CHECK (review_decision IN ('approved', 'changes_requested', 'none')),
  mergeable       TEXT NOT NULL DEFAULT 'unknown' CHECK (mergeable IN ('clean', 'dirty', 'blocked', 'unknown')),
  merged_at       TEXT,
  merge_sha       TEXT,
  fetched_at      TEXT,
  fetch_error     TEXT,
  -- cache (no-mistakes run records)
  pipeline_state  TEXT NOT NULL DEFAULT 'none' CHECK (pipeline_state IN ('none', 'running', 'passed', 'failed')),
  UNIQUE (project_id, number)
) STRICT;
CREATE INDEX prs_run ON prs(run_id);
CREATE INDEX prs_card_open ON prs(card_id) WHERE state = 'open';

CREATE TABLE grants (
  id          TEXT PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES projects(id),
  kind        TEXT NOT NULL CHECK (kind IN ('merge', 'land_direct')),
  conditions  TEXT NOT NULL,            -- JSON; shape in authority.md, section 6
  granted_at  TEXT NOT NULL,
  granted_by  TEXT NOT NULL,
  revoked_at  TEXT,
  revoke_reason TEXT
) STRICT;

CREATE TABLE approvals (
  id                        TEXT PRIMARY KEY,
  kind                      TEXT NOT NULL CHECK (kind IN ('merge', 'land_direct', 'memory', 'away')),
  project_id                TEXT NOT NULL REFERENCES projects(id),
  card_id                   TEXT REFERENCES cards(id),
  run_id                    TEXT REFERENCES runs(id),
  -- binding: which columns apply depends on kind (CHECK below). repo is a
  -- snapshot of the bound action, kept even if the project's repo moves.
  repo                      TEXT,
  pr_id                     TEXT REFERENCES prs(id),
  branch                    TEXT,
  head_sha                  TEXT,
  diff_sha256               TEXT,
  base_rev                  TEXT,
  scope                     TEXT,            -- JSON, away only
  memory_paths              TEXT,            -- JSON: source-map paths a merge or land_direct diff touches
  linked_approval_id        TEXT REFERENCES approvals(id),  -- memory approvals only: the merge approval accepted with it (CP2, S01)
  action                    TEXT NOT NULL CHECK (action IN ('merge', 'squash', 'rebase', 'push', 'apply', 'away')),
  state                     TEXT NOT NULL DEFAULT 'requested' CHECK (state IN
                              ('requested', 'accepted', 'rejected', 'executing',
                               'succeeded', 'failed', 'uncertain', 'stale')),
  requested_at              TEXT NOT NULL,
  requested_by              TEXT NOT NULL,
  decided_at                TEXT,
  decided_by                TEXT,            -- owner, or grant:<id>
  decision_idempotency_key  TEXT UNIQUE,
  grant_id                  TEXT REFERENCES grants(id),
  executed_at               TEXT,
  finished_at               TEXT,
  result                    TEXT,            -- JSON: merge sha, error text
  rev                       INTEGER NOT NULL DEFAULT 1,
  CHECK (kind <> 'merge'       OR (card_id IS NOT NULL AND repo IS NOT NULL AND pr_id IS NOT NULL AND head_sha IS NOT NULL AND action IN ('merge', 'squash', 'rebase'))),
  CHECK (kind <> 'land_direct' OR (card_id IS NOT NULL AND repo IS NOT NULL AND branch IS NOT NULL AND head_sha IS NOT NULL AND action = 'push')),
  CHECK (kind <> 'memory'      OR (diff_sha256 IS NOT NULL AND base_rev IS NOT NULL AND action = 'apply')),
  CHECK (kind <> 'away'        OR (scope IS NOT NULL AND action = 'away')),
  CHECK (memory_paths IS NULL OR (kind IN ('merge', 'land_direct') AND grant_id IS NULL)),
  CHECK (linked_approval_id IS NULL OR kind = 'memory'),
  CHECK ((state IN ('requested', 'stale')) OR decided_at IS NOT NULL),
  CHECK ((state IN ('executing', 'succeeded', 'failed', 'uncertain')) = (executed_at IS NOT NULL))
) STRICT;
CREATE INDEX approvals_open ON approvals(pr_id) WHERE state IN ('requested', 'accepted', 'executing');
-- one in-flight execution per PR
CREATE UNIQUE INDEX approvals_executing ON approvals(pr_id) WHERE state = 'executing' AND pr_id IS NOT NULL;

CREATE TABLE reservations (
  id              TEXT PRIMARY KEY,
  host_id         TEXT NOT NULL REFERENCES hosts(id),
  project_id      TEXT NOT NULL REFERENCES projects(id),
  run_id          TEXT NOT NULL REFERENCES runs(id),
  reserved_at     TEXT NOT NULL,
  reserved_by     TEXT NOT NULL,
  released_at     TEXT,
  release_reason  TEXT CHECK (release_reason IN ('ended', 'spawn_failed', 'reconciled')),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
) STRICT;
CREATE UNIQUE INDEX reservations_open_run ON reservations(run_id) WHERE released_at IS NULL;
CREATE INDEX reservations_open_host ON reservations(host_id) WHERE released_at IS NULL;
CREATE INDEX reservations_open_project ON reservations(project_id) WHERE released_at IS NULL;

-- ---------------------------------------------------------------------------
-- Delivery, observation, events
-- ---------------------------------------------------------------------------

CREATE TABLE outbox (
  id                 TEXT PRIMARY KEY,
  target_session_id  TEXT NOT NULL REFERENCES sessions(id),
  target_pane_id     TEXT,                 -- cache at enqueue time
  kind               TEXT NOT NULL CHECK (kind IN ('wake', 'steer', 'answer', 'note')),
  body               TEXT NOT NULL,        -- the payload; the pane gets only `tiller inbox <id>`
  card_id            TEXT REFERENCES cards(id),
  reply_to           TEXT,                 -- an ask or attention id
  idempotency_key    TEXT UNIQUE,
  state              TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered', 'acked', 'failed')),
  attempts           INTEGER NOT NULL DEFAULT 0,
  next_attempt_at    TEXT,
  created_at         TEXT NOT NULL,
  created_by         TEXT NOT NULL,
  delivered_at       TEXT,
  acked_at           TEXT,
  failed_at          TEXT,
  failure_reason     TEXT,
  rev                INTEGER NOT NULL DEFAULT 1,
  CHECK ((state = 'failed') = (failed_at IS NOT NULL)),
  CHECK (state <> 'acked' OR acked_at IS NOT NULL)
) STRICT;
CREATE INDEX outbox_due ON outbox(next_attempt_at) WHERE state IN ('pending', 'delivered');
CREATE INDEX outbox_session ON outbox(target_session_id, state);

CREATE TABLE observations (
  id           TEXT PRIMARY KEY,
  session_id   TEXT REFERENCES sessions(id),   -- null until the source can be mapped
  run_id       TEXT REFERENCES runs(id),
  source       TEXT NOT NULL CHECK (source IN ('hook', 'transcript', 'herdr', 'liveness', 'status_line')),
  event        TEXT NOT NULL,                  -- hook event or detected kind; mapping in protocol.md
  verdict      TEXT CHECK (verdict IN
                 ('working', 'idle', 'waiting_permission', 'waiting_input', 'errored', 'ended', 'unknown')),
  observed_at  TEXT NOT NULL,
  recorded_at  TEXT NOT NULL,
  payload      TEXT NOT NULL DEFAULT '{}'      -- JSON; unknown fields are kept
) STRICT;
CREATE INDEX observations_session ON observations(session_id, observed_at);
CREATE INDEX observations_recorded ON observations(recorded_at);   -- retention

CREATE TABLE events (
  seq              INTEGER PRIMARY KEY AUTOINCREMENT,   -- SSE Last-Event-ID
  id               TEXT NOT NULL UNIQUE,
  at               TEXT NOT NULL,
  txn              TEXT NOT NULL,            -- mutation name, e.g. card.close
  actor            TEXT NOT NULL,            -- session:<id>, board, daemon, hook, import, owner
  entity           TEXT NOT NULL,
  entity_id        TEXT NOT NULL,
  kind             TEXT NOT NULL,            -- e.g. card.created, run.ended
  rev_after        INTEGER,                  -- null for append-only entities
  card_id          TEXT,                     -- denormalized for timelines
  payload          TEXT NOT NULL DEFAULT '{}', -- JSON: changed fields and checked preconditions
  idempotency_key  TEXT
) STRICT;
CREATE INDEX events_entity ON events(entity, entity_id, seq);
CREATE INDEX events_card ON events(card_id, seq);
CREATE INDEX events_kind ON events(kind, seq);

-- ---------------------------------------------------------------------------
-- Obligations, artifacts, reviews, handoffs
-- ---------------------------------------------------------------------------

CREATE TABLE obligations (
  id                TEXT PRIMARY KEY,
  project_id        TEXT NOT NULL REFERENCES projects(id),
  card_id           TEXT REFERENCES cards(id),
  what              TEXT NOT NULL,
  trigger_kind      TEXT NOT NULL CHECK (trigger_kind IN ('event', 'date', 'standing')),
  trigger_event     TEXT,                  -- JSON: event kind and entity filter
  due_at            TEXT,
  state             TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'met', 'cancelled')),
  opened_at         TEXT NOT NULL,
  opened_by         TEXT NOT NULL,
  met_at            TEXT,
  met_by_event_seq  INTEGER REFERENCES events(seq),
  CHECK (trigger_kind <> 'event' OR trigger_event IS NOT NULL),
  CHECK (trigger_kind <> 'date' OR due_at IS NOT NULL)
) STRICT;
CREATE INDEX obligations_open ON obligations(project_id) WHERE state = 'open';

CREATE TABLE artifacts (
  id                     TEXT PRIMARY KEY,
  card_id                TEXT NOT NULL REFERENCES cards(id),
  run_id                 TEXT REFERENCES runs(id),
  kind                   TEXT NOT NULL CHECK (kind IN ('page', 'screenshot', 'report', 'video', 'other')),
  title                  TEXT NOT NULL,
  url                    TEXT,
  path                   TEXT,             -- local, outside any repo
  created_at             TEXT NOT NULL,
  created_by_session_id  TEXT REFERENCES sessions(id),
  retired_at             TEXT,
  CHECK (url IS NOT NULL OR path IS NOT NULL)
) STRICT;
CREATE INDEX artifacts_card ON artifacts(card_id);

CREATE TABLE reviews (
  id               TEXT PRIMARY KEY,
  run_id           TEXT NOT NULL REFERENCES runs(id),
  card_id          TEXT NOT NULL REFERENCES cards(id),
  reviewer         TEXT NOT NULL CHECK (reviewer IN ('code-review', 'spec-review', 'no-mistakes', 'human')),
  coverage         TEXT NOT NULL CHECK (coverage IN ('diff', 'spec', 'full')),
  findings_high    INTEGER NOT NULL DEFAULT 0,
  findings_medium  INTEGER NOT NULL DEFAULT 0,
  findings_low     INTEGER NOT NULL DEFAULT 0,
  verdict          TEXT NOT NULL CHECK (verdict IN ('pass', 'fail', 'ask-user')),
  recorded_at      TEXT NOT NULL,
  source_url       TEXT
) STRICT;
CREATE INDEX reviews_run ON reviews(run_id);

CREATE TABLE handoffs (
  id                 TEXT PRIMARY KEY,
  project_id         TEXT NOT NULL REFERENCES projects(id),
  session_id         TEXT NOT NULL REFERENCES sessions(id),
  written_at         TEXT NOT NULL,
  active_cards       TEXT NOT NULL,       -- JSON list of card ids (projection at write time)
  pending_attention  TEXT NOT NULL,       -- JSON list of attention ids
  workers            TEXT NOT NULL,       -- JSON list: run id and one line each
  conclusions        TEXT NOT NULL,
  next_action        TEXT NOT NULL
) STRICT;
CREATE INDEX handoffs_project ON handoffs(project_id, written_at);

-- ---------------------------------------------------------------------------
-- Caches of facts owned elsewhere (every row carries fetched_at)
-- ---------------------------------------------------------------------------

CREATE TABLE panes (                      -- owner: herdr
  pane_id          TEXT NOT NULL,
  host_id          TEXT NOT NULL REFERENCES hosts(id),
  workspace_label  TEXT,
  agent_name       TEXT,
  agent_status     TEXT,                  -- herdr's own word, stored verbatim
  session_id       TEXT REFERENCES sessions(id),
  metadata         TEXT NOT NULL DEFAULT '{}', -- JSON row tokens
  fetched_at       TEXT NOT NULL,
  gone_at          TEXT,
  PRIMARY KEY (host_id, pane_id)
) STRICT;

CREATE TABLE worktrees (                  -- owner: treehouse and git
  path              TEXT NOT NULL,
  host_id           TEXT NOT NULL REFERENCES hosts(id),
  project_id        TEXT REFERENCES projects(id),
  run_id            TEXT REFERENCES runs(id),
  branch            TEXT,
  head_sha          TEXT,
  dirty             INTEGER CHECK (dirty IN (0, 1)),
  unlanded_commits  INTEGER,              -- null means unknown: cleanup refuses
  fetched_at        TEXT NOT NULL,
  gone_at           TEXT,
  PRIMARY KEY (host_id, path)
) STRICT;

CREATE TABLE quota_windows (              -- owner: quota-axi; never an account id or email
  label                    TEXT PRIMARY KEY,
  percent_remaining        REAL,
  resets_at                TEXT,
  pace                     TEXT,
  reserve                  REAL,
  burn_multiple            REAL,
  elapsed_percent          REAL,
  projected_exhaustion_at  TEXT,
  title                    TEXT,             -- display label; label is <provider>:<window id>
  kind                     TEXT,             -- weekly, model, credits, ... (quota-axi)
  fetched_at               TEXT NOT NULL,
  fetch_error              TEXT
) STRICT;

-- ---------------------------------------------------------------------------
-- Memory (docs/spec/memory.md, section 11). evidence and proposals are never
-- loaded into an agent's context.
-- ---------------------------------------------------------------------------

CREATE TABLE memory_sources (
  id               TEXT PRIMARY KEY,
  project_id       TEXT NOT NULL REFERENCES projects(id),   -- 'fleet' for host-wide
  boundary         TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  kind             TEXT NOT NULL CHECK (kind IN ('tiller_doc', 'dotfiles', 'project_file', 'foreign')),
  loaded_path      TEXT NOT NULL,          -- pattern, relative to the project root or '~'
  canonical_repo   TEXT,                   -- null for foreign
  canonical_path   TEXT,
  apply_method     TEXT NOT NULL CHECK (apply_method IN ('tiller_commit', 'dotfiles_rebuild', 'project_delivery', 'none')),
  load_mode        TEXT NOT NULL CHECK (load_mode IN ('rendered', 'eager', 'lazy')),
  roles            TEXT NOT NULL DEFAULT '["orchestrator","scout","builder","reviewer","verifier"]', -- JSON
  rebuild_command  TEXT,
  added_at         TEXT NOT NULL,
  retired_at       TEXT,
  rev              INTEGER NOT NULL DEFAULT 1,
  CHECK ((kind = 'foreign') = (apply_method = 'none')),
  CHECK (kind = 'foreign' OR (canonical_repo IS NOT NULL AND canonical_path IS NOT NULL)),
  CHECK ((kind = 'tiller_doc') = (load_mode = 'rendered')),
  CHECK (kind <> 'dotfiles' OR apply_method = 'dotfiles_rebuild'),
  CHECK (kind <> 'project_file' OR apply_method = 'project_delivery'),
  CHECK (kind <> 'tiller_doc' OR apply_method = 'tiller_commit')
) STRICT;
CREATE UNIQUE INDEX memory_sources_loaded ON memory_sources(project_id, loaded_path) WHERE retired_at IS NULL;

CREATE TABLE memory_revisions (
  id                  TEXT PRIMARY KEY,
  source_id           TEXT NOT NULL REFERENCES memory_sources(id),
  blob_sha            TEXT NOT NULL,       -- git blob of the canonical file
  commit_sha          TEXT,
  installed_sha256    TEXT,                -- dotfiles and foreign: the installed bytes
  parent_revision_id  TEXT REFERENCES memory_revisions(id),
  review              TEXT NOT NULL CHECK (review IN ('approved', 'unreviewed', 'revoked')),
  provenance          TEXT NOT NULL CHECK (provenance IN ('tiller', 'external', 'legacy-import', 'baseline')),
  approval_id         TEXT REFERENCES approvals(id),
  scrub               TEXT NOT NULL CHECK (scrub IN ('passed', 'flagged')),
  recorded_at         TEXT NOT NULL,
  recorded_by         TEXT NOT NULL,
  CHECK (review <> 'approved' OR scrub = 'passed'),
  CHECK (provenance <> 'tiller' OR approval_id IS NOT NULL)
) STRICT;
CREATE INDEX memory_revisions_source ON memory_revisions(source_id, recorded_at);
CREATE UNIQUE INDEX memory_revisions_content ON memory_revisions(source_id, blob_sha, coalesce(installed_sha256, ''));

CREATE TABLE evidence (
  id                TEXT PRIMARY KEY,
  project_id        TEXT NOT NULL REFERENCES projects(id),
  boundary          TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  kind              TEXT NOT NULL CHECK (kind IN
                      ('observation', 'transcript', 'card', 'review', 'test', 'ci',
                       'incident', 'owner_message', 'legacy')),
  ref               TEXT NOT NULL,         -- JSON pointer; local paths stay here, never in git
  excerpt           TEXT,                  -- local only, never rendered into a session
  card_id           TEXT REFERENCES cards(id),
  run_id            TEXT REFERENCES runs(id),
  session_id        TEXT REFERENCES sessions(id),
  independence_key  TEXT NOT NULL,
  captured_at       TEXT NOT NULL,
  captured_by       TEXT NOT NULL,
  superseded_by     TEXT REFERENCES evidence(id),
  contradicts_memory_id TEXT             -- forward reference to memories; marks it due at once
) STRICT;
CREATE INDEX evidence_project ON evidence(project_id, captured_at);

CREATE TABLE memories (
  id                      TEXT PRIMARY KEY,
  source_id               TEXT NOT NULL REFERENCES memory_sources(id),
  section                 TEXT,
  text_sha256             TEXT NOT NULL,
  legacy_id               TEXT,
  scope_kind              TEXT NOT NULL CHECK (scope_kind IN ('fleet', 'project', 'role')),
  scope_value             TEXT,
  class                   TEXT NOT NULL CHECK (class IN ('rule', 'note', 'preference')),
  validity                TEXT NOT NULL CHECK (validity IN ('durable', 'until_condition', 'until_date')),
  validity_condition      TEXT,            -- JSON
  valid_until             TEXT,
  review_due_at           TEXT,
  review_paths            TEXT,            -- JSON globs; a landed change touching one marks it due
  review_status           TEXT NOT NULL CHECK (review_status IN
                            ('approved', 'unreviewed', 'due', 'expired', 'retired')),
  due_reason              TEXT CHECK (due_reason IN ('interval', 'contradicting_evidence', 'project_change')),
  provenance              TEXT NOT NULL CHECK (provenance IN
                            ('proposal', 'owner_edit', 'external', 'legacy-import', 'baseline')),
  rationale               TEXT,            -- never invented; null when none was stated
  authored_by_session_id  TEXT REFERENCES sessions(id),
  proposal_id             TEXT,            -- forward reference to proposals
  created_revision_id     TEXT NOT NULL REFERENCES memory_revisions(id),
  reviewed_at             TEXT,
  reviewed_by             TEXT,
  retired_at              TEXT,
  retired_revision_id     TEXT REFERENCES memory_revisions(id),
  retire_reason           TEXT,
  revoke_older            INTEGER NOT NULL DEFAULT 0 CHECK (revoke_older IN (0, 1)),
  rev                     INTEGER NOT NULL DEFAULT 1,
  CHECK ((scope_kind = 'fleet') = (scope_value IS NULL)),
  CHECK (validity <> 'until_condition' OR validity_condition IS NOT NULL),
  CHECK (validity <> 'until_date' OR valid_until IS NOT NULL),
  CHECK (validity <> 'durable' OR review_status IN ('unreviewed', 'retired') OR review_due_at IS NOT NULL),
  CHECK ((review_status = 'retired') = (retired_at IS NOT NULL)),
  CHECK (provenance <> 'proposal' OR proposal_id IS NOT NULL),
  CHECK (provenance <> 'legacy-import' OR legacy_id IS NOT NULL),
  CHECK ((review_status = 'due') = (due_reason IS NOT NULL))
) STRICT;
CREATE INDEX memories_source ON memories(source_id) WHERE review_status <> 'retired';
CREATE INDEX memories_due ON memories(review_due_at) WHERE review_status = 'approved';

CREATE TABLE memory_evidence (
  memory_id    TEXT NOT NULL REFERENCES memories(id),
  evidence_id  TEXT NOT NULL REFERENCES evidence(id),
  PRIMARY KEY (memory_id, evidence_id)
) STRICT;

CREATE TABLE proposals (
  id                      TEXT PRIMARY KEY,
  project_id              TEXT NOT NULL REFERENCES projects(id),
  boundary                TEXT NOT NULL CHECK (boundary IN ('personal', 'work')),
  target_source_id        TEXT NOT NULL REFERENCES memory_sources(id),
  op                      TEXT NOT NULL CHECK (op IN
                            ('add', 'rewrite', 'remove', 'retire', 'promote', 'accept_external')),
  memory_id               TEXT REFERENCES memories(id),
  base_revision_id        TEXT NOT NULL REFERENCES memory_revisions(id),
  base_blob               TEXT NOT NULL,
  diff                    TEXT NOT NULL,
  diff_sha256             TEXT NOT NULL,
  memory_fields           TEXT NOT NULL DEFAULT '{}',   -- JSON: scope, class, validity, section
  rationale               TEXT,
  expected_effect         TEXT,
  token_budget_after      TEXT,            -- JSON: role -> tokens loaded after the change
  authored_by_session_id  TEXT REFERENCES sessions(id),
  card_id                 TEXT REFERENCES cards(id),
  created_at              TEXT NOT NULL,
  created_by              TEXT NOT NULL,
  state                   TEXT NOT NULL DEFAULT 'proposed' CHECK (state IN
                            ('proposed', 'accepted', 'applied', 'discarded', 'stale', 'failed')),
  approval_id             TEXT REFERENCES approvals(id),
  decided_at              TEXT,
  applied_revision_id     TEXT REFERENCES memory_revisions(id),
  failure_reason          TEXT,
  rev                     INTEGER NOT NULL DEFAULT 1,
  CHECK (op IN ('add', 'accept_external') OR memory_id IS NOT NULL),
  CHECK (op IN ('accept_external', 'retire', 'remove') OR expected_effect IS NOT NULL),
  CHECK ((state = 'applied') = (applied_revision_id IS NOT NULL)),
  CHECK (state IN ('proposed', 'discarded') OR approval_id IS NOT NULL)
) STRICT;
CREATE INDEX proposals_open ON proposals(target_source_id) WHERE state IN ('proposed', 'accepted');

CREATE TABLE proposal_evidence (
  proposal_id  TEXT NOT NULL REFERENCES proposals(id),
  evidence_id  TEXT NOT NULL REFERENCES evidence(id),
  PRIMARY KEY (proposal_id, evidence_id)
) STRICT;

CREATE TABLE session_loads (
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  revision_id  TEXT NOT NULL REFERENCES memory_revisions(id),
  trigger      TEXT NOT NULL CHECK (trigger IN ('startup', 'resume', 'clear', 'compact')),
  loaded_at    TEXT NOT NULL,
  PRIMARY KEY (session_id, revision_id, loaded_at)
) STRICT;
CREATE INDEX session_loads_revision ON session_loads(revision_id);

-- ---------------------------------------------------------------------------
-- Derived views: the only place a board column exists
-- ---------------------------------------------------------------------------

-- Activity with staleness derived from age. The thresholds are the defaults
-- recovery.md section 2 sets per role (orchestrator 30 min, workers 15 min).
CREATE VIEW session_activity AS
SELECT
  s.id            AS session_id,
  s.run_id,
  s.role,
  s.activity,
  s.activity_observed_at,
  CASE
    WHEN s.ended_at IS NOT NULL THEN 'ended'
    WHEN s.activity_observed_at IS NULL THEN 'unknown'
    -- a prompt is never stale: it stays in Needs you until a newer observation
    WHEN s.activity IN ('waiting_permission', 'waiting_input') THEN s.activity
    -- a dated pause silences staleness until 15 min after it ends
    WHEN r.pause_until IS NOT NULL
         AND julianday('now') - julianday(r.pause_until) <= 15.0 / 1440 THEN s.activity
    WHEN julianday('now') - julianday(s.activity_observed_at) >
         CASE s.role WHEN 'orchestrator' THEN 30.0 / 1440 ELSE 15.0 / 1440 END
      THEN 'stale'
    ELSE s.activity
  END             AS effective_activity
FROM sessions s
LEFT JOIN runs r ON r.id = s.run_id;

-- One row per open or recently closed card: its column and badge.
-- Precedence follows state.md, section 6.
CREATE VIEW card_column AS
WITH
  open_attention AS (
    -- the oldest open item names the badge; SQLite's bare-column rule makes
    -- `kind` come from the row that holds min(opened_at)
    SELECT card_id, min(opened_at) AS oldest_at, kind AS a_kind
    FROM attention
    WHERE answered_at IS NULL AND withdrawn_at IS NULL
    GROUP BY card_id
  ),
  run_activity AS (
    -- one row per live session of an active run (at most one, by the
    -- sessions_live_run index; a run with none ranks unknown), ranked so a
    -- card with several runs shows the most urgent badge: prompt, errored,
    -- working, paused, idle, stale, unknown. The rank is compared, never the
    -- activity text, so min() cannot pick an alphabetically early verdict.
    SELECT r.card_id, r.id AS run_id,
           CASE
             WHEN a.effective_activity IN ('waiting_permission', 'waiting_input') THEN 0
             WHEN a.effective_activity = 'errored' THEN 1
             WHEN a.effective_activity = 'working' THEN 2
             WHEN r.pause_until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') THEN 3
             WHEN a.effective_activity = 'idle' THEN 4
             WHEN a.effective_activity = 'stale' THEN 5
             ELSE 6
           END AS rank
    FROM runs r
    LEFT JOIN session_activity a
      ON a.run_id = r.id AND a.effective_activity <> 'ended'
    WHERE r.ended_at IS NULL
  ),
  active_runs AS (
    SELECT card_id,
           count(DISTINCT run_id) AS n,
           min(rank) AS rank
    FROM run_activity
    GROUP BY card_id
  ),
  landing_prs AS (
    -- an open PR with checks or the pipeline still running, whether or not
    -- the run that opened it is still active or known
    SELECT card_id, 1 AS landing
    FROM prs
    WHERE state = 'open' AND (checks = 'pending' OR pipeline_state = 'running')
    GROUP BY card_id
  ),
  landing_approvals AS (
    SELECT card_id, 1 AS landing
    FROM approvals
    WHERE kind IN ('merge', 'land_direct') AND state IN ('accepted', 'executing')
    GROUP BY card_id
  ),
  deploy_watches AS (
    -- an open deploy-watch obligation (state.md, section 7)
    SELECT card_id, 1 AS landing
    FROM obligations
    WHERE state = 'open' AND trigger_kind = 'event'
      AND json_extract(trigger_event, '$.kind') = 'deploy.finished'
    GROUP BY card_id
  ),
  open_holds AS (
    SELECT card_id, until_at, reason FROM holds WHERE released_at IS NULL
  ),
  open_deps AS (
    SELECT d.card_id, min(c.display_id) AS blocked_by
    FROM card_deps d JOIN cards c ON c.id = d.depends_on_card_id
    WHERE d.released_at IS NULL
    GROUP BY d.card_id
  )
SELECT
  c.id, c.display_id, c.project_id, c.kind,
  CASE
    WHEN c.state = 'closed' THEN 'done'
    WHEN oa.card_id IS NOT NULL OR ar.rank = 0 THEN 'needs_you'
    WHEN c.kind = 'ship' AND (lp.landing = 1 OR la.landing = 1 OR dw.landing = 1) THEN 'landing'
    WHEN ar.n > 0 THEN 'working'
    ELSE 'queued'
  END AS "column",
  CASE
    WHEN c.state = 'closed' THEN c.outcome
    WHEN oa.card_id IS NOT NULL THEN oa.a_kind
    WHEN ar.rank = 0 THEN 'permission_prompt'
    WHEN c.kind = 'ship' AND la.landing = 1 THEN 'merging'
    WHEN c.kind = 'ship' AND lp.landing = 1 THEN 'checks'
    WHEN c.kind = 'ship' AND dw.landing = 1 THEN 'deploying'
    WHEN ar.rank = 1 THEN 'errored'
    WHEN ar.rank = 2 THEN 'working'
    WHEN ar.rank = 3 THEN 'paused'
    WHEN ar.rank = 4 THEN 'idle'
    WHEN ar.rank = 5 THEN 'stale'
    WHEN ar.n > 0 THEN 'unknown'
    WHEN oh.card_id IS NOT NULL THEN 'parked'
    WHEN od.card_id IS NOT NULL THEN 'blocked'
    ELSE 'filed'
  END AS badge,
  oh.until_at   AS parked_until,
  oh.reason     AS parked_reason,
  od.blocked_by AS blocked_by,
  c.closed_at
FROM cards c
LEFT JOIN open_attention     oa ON oa.card_id = c.id
LEFT JOIN active_runs        ar ON ar.card_id = c.id
LEFT JOIN landing_prs        lp ON lp.card_id = c.id
LEFT JOIN landing_approvals  la ON la.card_id = c.id
LEFT JOIN deploy_watches     dw ON dw.card_id = c.id
LEFT JOIN open_holds         oh ON oh.card_id = c.id
LEFT JOIN open_deps          od ON od.card_id = c.id
WHERE c.state = 'open'
   OR julianday('now') - julianday(c.closed_at) <= 7;

-- Unresolved asks are the Asked column. They are not cards.
CREATE VIEW asked AS
SELECT id, project_id, session_id, received_at,
       (julianday('now') - julianday(received_at)) * 86400 AS age_seconds
FROM asks
WHERE disposition = 'unresolved';

-- Free slots per host and project, from open reservations only.
CREATE VIEW capacity AS
SELECT h.id AS host_id, h.label,
       h.lane_capacity,
       (SELECT count(*) FROM reservations r WHERE r.host_id = h.id AND r.released_at IS NULL) AS reserved,
       h.lane_capacity - (SELECT count(*) FROM reservations r WHERE r.host_id = h.id AND r.released_at IS NULL) AS free
FROM hosts h
WHERE h.kind = 'local';

INSERT INTO meta (key, value) VALUES ('schema_version', '1');
