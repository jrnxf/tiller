-- Migration 10: a card moves forward only (state.md section 6). Between
-- the steps of its work it no longer falls back to Queued: a ship card with
-- an open PR and no active run is Landing whatever the PR's checks say, a
-- merge whose result is uncertain is still merging, and a card whose work
-- has begun is Working between one run and the next. Queued is for a card
-- never started, parked, blocked, or sent back by the owner until its next
-- run. The card_column view changes, and approvals gain an index by card.

DROP VIEW card_column;

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
    WHERE kind IN ('merge', 'land_direct') AND state IN ('accepted', 'executing', 'uncertain')
    GROUP BY card_id
  ),
  open_prs AS (
    -- the card's open PRs: which badge a ship card shows in Landing once no
    -- run is active on it
    SELECT card_id,
           max(fetched_at IS NULL) AS unread,
           max(checks = 'failure' OR pipeline_state = 'failed') AS failing
    FROM prs
    WHERE state = 'open'
    GROUP BY card_id
  ),
  progress AS (
    -- per open card, since it was created or last reopened: when its newest
    -- run started, and when the owner last sent it back (rejected its merge
    -- or direct land, or the merge failed). A send-back holds until the
    -- next run starts.
    SELECT p.card_id, p.run_at,
           p.back_at IS NOT NULL AND p.back_at > coalesce(p.run_at, '') AS back
    FROM (
      SELECT c.id AS card_id,
             (SELECT max(r.started_at) FROM runs r
              WHERE r.card_id = c.id AND r.started_at > coalesce(ro.at, '')) AS run_at,
             (SELECT max(coalesce(a.finished_at, a.decided_at)) FROM approvals a
              WHERE a.card_id = c.id AND a.kind IN ('merge', 'land_direct')
                AND a.state IN ('rejected', 'failed')
                AND coalesce(a.finished_at, a.decided_at) > coalesce(ro.at, '')) AS back_at
      FROM cards c
      LEFT JOIN (
        SELECT entity_id AS card_id, max(at) AS at FROM events
        WHERE kind = 'card.reopened'
        GROUP BY entity_id
      ) ro ON ro.card_id = c.id
      WHERE c.state = 'open'
    ) p
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
    WHEN oh.card_id IS NOT NULL OR od.card_id IS NOT NULL THEN 'queued'
    WHEN pg.back THEN 'queued'
    WHEN c.kind = 'ship' AND op.card_id IS NOT NULL THEN 'landing'
    WHEN pg.run_at IS NOT NULL THEN 'working'
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
    WHEN pg.back THEN 'filed'
    WHEN c.kind = 'ship' AND op.unread = 1 THEN 'checks'
    WHEN c.kind = 'ship' AND op.failing = 1 THEN 'checks_failed'
    WHEN c.kind = 'ship' AND op.card_id IS NOT NULL THEN 'ready'
    WHEN pg.run_at IS NOT NULL THEN 'between_runs'
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
LEFT JOIN open_prs           op ON op.card_id = c.id
LEFT JOIN progress           pg ON pg.card_id = c.id
LEFT JOIN open_holds         oh ON oh.card_id = c.id
LEFT JOIN open_deps          od ON od.card_id = c.id
WHERE c.state = 'open'
   OR julianday('now') - julianday(c.closed_at) <= 3;

-- the send-back lookup reads a card's approvals
CREATE INDEX approvals_card ON approvals(card_id);

UPDATE meta SET value = '10' WHERE key = 'schema_version';
