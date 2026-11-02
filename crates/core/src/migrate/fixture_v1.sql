-- A populated schema-1 database: at least one row in every table, with the
-- foreign keys the live system writes, including the two pairs of tables that
-- reference each other (cards and asks, sessions and runs). The migration test
-- loads it and runs every migration down and up again over it.

INSERT INTO meta (key, value) VALUES
  ('host_id', '01JC00000000000000000HOST1'),
  ('created_at', '2026-11-02T09:00:00.000Z'),
  ('boundary', 'personal'),
  ('boot_id', 'boot-1');

INSERT INTO hosts (id, label, kind, lane_capacity, ram_floor_mb, disk_floor_mb, last_seen_at)
VALUES ('01JC00000000000000000HOST1', 'host-a', 'local', 4, 2048, 10240, '2026-11-02T09:00:00.000Z');
INSERT INTO hosts (id, label, kind, herdr_machine, stale_reason)
VALUES ('01JC00000000000000000HOST2', 'host-b', 'remote', 'machine-b', 'unreachable');

INSERT INTO projects (id, name, delivery, owner_host_id, added_at, next_card_seq)
VALUES ('fleet', 'Fleet', 'local-only', '01JC00000000000000000HOST1', '2026-11-02T09:00:00.000Z', 2);
INSERT INTO projects (id, name, repo, remote, delivery, checks, capacity, deploy_workflow,
                      deploy_environment, merge_gate, owner_host_id, added_at, next_card_seq)
VALUES ('alpha', 'Alpha', 'example/alpha', 'https://example.com/example/alpha.git', 'direct-push',
        '["cargo test"]', 2, 'deploy.yml', 'production', 'enforced',
        '01JC00000000000000000HOST1', '2026-11-02T09:00:00.000Z', 3);

INSERT INTO sessions (id, claude_session_id, role, project_id, host_id, pane_id, started_at,
                      activity, activity_observed_at, activity_source)
VALUES ('01JC0000000000000000000SO1', 'claude-orch-1', 'orchestrator', 'alpha',
        '01JC00000000000000000HOST1', 'pane-1', '2026-11-02T09:01:00.000Z',
        'idle', '2026-11-02T09:02:00.000Z', 'hook');

INSERT INTO asks (id, project_id, session_id, source, text, received_at, disposition, resolved_at,
                  resolved_by_session_id)
VALUES ('01JC0000000000000000000AS1', 'alpha', '01JC0000000000000000000SO1', 'prompt',
        'make the thing', '2026-11-02T09:03:00.000Z', 'new', '2026-11-02T09:03:05.000Z',
        '01JC0000000000000000000SO1');
INSERT INTO asks (id, project_id, source, text, received_at)
VALUES ('01JC0000000000000000000AS2', 'alpha', 'script', 'weekly note', '2026-11-02T09:04:00.000Z');

INSERT INTO cards (id, display_id, project_id, kind, title, state, created_at, rev)
VALUES ('01JC0000000000000000000CF1', 'fleet-1', 'fleet', 'ops', 'Host health', 'open',
        '2026-11-02T09:00:00.000Z', 1);
INSERT INTO cards (id, display_id, project_id, kind, title, body, priority, size, acceptance,
                   dispatch_override, created_at, created_by_ask_id, rev)
VALUES ('01JC0000000000000000000CA1', 'alpha-1', 'alpha', 'ship', 'Make the thing', 'brief', 1, 'M',
        'tests pass', '{"model":"opus","effort":"high"}', '2026-11-02T09:03:05.000Z',
        '01JC0000000000000000000AS1', 2);
INSERT INTO cards (id, display_id, legacy_id, project_id, kind, title, fixes_card_id, state, outcome,
                   closed_at, closed_by, created_at)
VALUES ('01JC0000000000000000000CA2', 'alpha-2', 'fm-7', 'alpha', 'scout', 'Look into it',
        '01JC0000000000000000000CA1', 'closed', 'reported', '2026-11-02T10:00:00.000Z', 'owner',
        '2026-11-02T09:05:00.000Z');

INSERT INTO ask_cards (ask_id, card_id, relation, linked_at, linked_by_session_id)
VALUES ('01JC0000000000000000000AS1', '01JC0000000000000000000CA1', 'created',
        '2026-11-02T09:03:05.000Z', '01JC0000000000000000000SO1');

INSERT INTO card_deps (card_id, depends_on_card_id, added_at, added_by, released_at, release_reason)
VALUES ('01JC0000000000000000000CA1', '01JC0000000000000000000CA2', '2026-11-02T09:06:00.000Z',
        'owner', '2026-11-02T10:00:00.000Z', 'reported');

INSERT INTO holds (id, card_id, until_at, reason, set_at, set_by, released_at, release_reason)
VALUES ('01JC0000000000000000000HO1', '01JC0000000000000000000CA1', '2026-11-03T09:00:00.000Z',
        'wait a day', '2026-11-02T09:07:00.000Z', 'owner', '2026-11-02T09:08:00.000Z', 'removed');

INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, worktree_path, branch,
                  base_branch, base_sha, supervisor_session_id, started_at, config_hash,
                  idempotency_key, last_status_line, last_status_at)
VALUES ('01JC0000000000000000000RU1', '01JC0000000000000000000CA1', 'builder', 1, 'opus', 'high',
        '01JC00000000000000000HOST1', '/home/user/worktrees/alpha-1', 'alpha-1', 'main',
        'abc123', '01JC0000000000000000000SO1', '2026-11-02T09:10:00.000Z', 'cfg-1', 'spawn-key-1',
        'working: tests', '2026-11-02T09:20:00.000Z');

INSERT INTO sessions (id, claude_session_id, role, project_id, run_id, host_id, pane_id,
                      transcript_path, config_hash, started_at, activity, activity_observed_at,
                      activity_source)
VALUES ('01JC0000000000000000000SW1', 'claude-work-1', 'builder', 'alpha',
        '01JC0000000000000000000RU1', '01JC00000000000000000HOST1', 'pane-2',
        '/home/user/.claude/projects/alpha/claude-work-1.jsonl', 'cfg-1',
        '2026-11-02T09:10:01.000Z', 'working', '2026-11-02T09:21:00.000Z', 'hook');

INSERT INTO attention (id, card_id, kind, question, evidence, recommendation, consequence, options,
                       opened_at, opened_by_session_id, opened_by_run_id, answered_at, answer,
                       answered_via, answer_idempotency_key, rev)
VALUES ('01JC0000000000000000000AT1', '01JC0000000000000000000CA1', 'decision', 'Which way?',
        '["01JC0000000000000000000EV1"]', 'left', 'left is faster', '["left","right"]',
        '2026-11-02T09:30:00.000Z', '01JC0000000000000000000SO1', '01JC0000000000000000000RU1',
        '2026-11-02T09:31:00.000Z', 'left', 'board', 'answer-key-1', 2);
INSERT INTO attention (id, card_id, kind, question, opened_at)
VALUES ('01JC0000000000000000000AT2', '01JC0000000000000000000CF1', 'recovery', 'Backup failed',
        '2026-11-02T09:32:00.000Z');

INSERT INTO prs (id, project_id, card_id, run_id, number, url, linked_at, linked_by, state,
                 head_sha, base_ref, checks, review_decision, mergeable, fetched_at, pipeline_state)
VALUES ('01JC0000000000000000000PR1', 'alpha', '01JC0000000000000000000CA1',
        '01JC0000000000000000000RU1', 12, 'https://github.com/example/alpha/pull/12',
        '2026-11-02T09:40:00.000Z', 'session:01JC0000000000000000000SW1', 'open', 'def456', 'main',
        'success', 'approved', 'clean', '2026-11-02T09:41:00.000Z', 'passed');

INSERT INTO grants (id, project_id, kind, conditions, granted_at, granted_by)
VALUES ('01JC0000000000000000000GR1', 'alpha', 'merge', '{"checks":"green"}',
        '2026-11-02T09:00:00.000Z', 'owner');

INSERT INTO approvals (id, kind, project_id, card_id, run_id, repo, pr_id, head_sha, action, state,
                       requested_at, requested_by, decided_at, decided_by, decision_idempotency_key,
                       grant_id, memory_paths)
VALUES ('01JC0000000000000000000AP1', 'merge', 'alpha', '01JC0000000000000000000CA1',
        '01JC0000000000000000000RU1', 'example/alpha', '01JC0000000000000000000PR1', 'def456',
        'squash', 'accepted', '2026-11-02T09:42:00.000Z', 'daemon', '2026-11-02T09:43:00.000Z',
        'owner', 'decide-key-1', NULL, '["AGENTS.md"]');

INSERT INTO reservations (id, host_id, project_id, run_id, reserved_at, reserved_by)
VALUES ('01JC0000000000000000000RE1', '01JC00000000000000000HOST1', 'alpha',
        '01JC0000000000000000000RU1', '2026-11-02T09:10:00.000Z', 'session:01JC0000000000000000000SO1');

INSERT INTO outbox (id, target_session_id, target_pane_id, kind, body, card_id, reply_to,
                    idempotency_key, state, attempts, created_at, created_by, delivered_at, acked_at)
VALUES ('01JC0000000000000000000OB1', '01JC0000000000000000000SO1', 'pane-1', 'answer', 'left',
        '01JC0000000000000000000CA1', '01JC0000000000000000000AT1', 'outbox-key-1', 'acked', 1,
        '2026-11-02T09:31:00.000Z', 'board', '2026-11-02T09:31:01.000Z', '2026-11-02T09:31:05.000Z');

INSERT INTO observations (id, session_id, run_id, source, event, verdict, observed_at, recorded_at,
                          payload)
VALUES ('01JC0000000000000000000OS1', '01JC0000000000000000000SW1', '01JC0000000000000000000RU1',
        'hook', 'UserPromptSubmit', 'working', '2026-11-02T09:21:00.000Z',
        '2026-11-02T09:21:00.050Z', '{"unknown_field":1}');

INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, rev_after, card_id, payload)
VALUES ('01JC0000000000000000000EV1', '2026-11-02T09:03:05.000Z', 'card.create', 'session:01JC0000000000000000000SO1',
        'card', '01JC0000000000000000000CA1', 'card.created', 1, '01JC0000000000000000000CA1', '{}');

INSERT INTO obligations (id, project_id, card_id, what, trigger_kind, trigger_event, due_at, state,
                         opened_at, opened_by, met_at, met_by_event_seq)
VALUES ('01JC0000000000000000000OL1', 'alpha', '01JC0000000000000000000CA1', 'report when green',
        'event', '{"kind":"pr.changed"}', NULL, 'met', '2026-11-02T09:45:00.000Z', 'daemon',
        '2026-11-02T09:46:00.000Z', 1);

INSERT INTO artifacts (id, card_id, run_id, kind, title, url, created_at, created_by_session_id)
VALUES ('01JC0000000000000000000AR1', '01JC0000000000000000000CA2', NULL, 'report', 'Findings',
        'https://example.com/report', '2026-11-02T09:50:00.000Z', '01JC0000000000000000000SO1');

INSERT INTO reviews (id, run_id, card_id, reviewer, coverage, findings_high, verdict, recorded_at)
VALUES ('01JC0000000000000000000RV1', '01JC0000000000000000000RU1', '01JC0000000000000000000CA1',
        'code-review', 'diff', 1, 'fail', '2026-11-02T09:55:00.000Z');

INSERT INTO handoffs (id, project_id, session_id, written_at, active_cards, pending_attention,
                      workers, conclusions, next_action)
VALUES ('01JC0000000000000000000HF1', 'alpha', '01JC0000000000000000000SO1',
        '2026-11-02T10:05:00.000Z', '["01JC0000000000000000000CA1"]', '[]',
        '[{"run":"01JC0000000000000000000RU1","line":"building"}]', 'going well', 'merge it');

INSERT INTO panes (pane_id, host_id, workspace_label, agent_name, agent_status, session_id,
                   metadata, fetched_at)
VALUES ('pane-2', '01JC00000000000000000HOST1', 'alpha', 'alpha-1-build', 'working',
        '01JC0000000000000000000SW1', '{"tiller_run":"01JC0000000000000000000RU1"}',
        '2026-11-02T09:21:00.000Z');

INSERT INTO worktrees (path, host_id, project_id, run_id, branch, head_sha, dirty,
                       unlanded_commits, fetched_at)
VALUES ('/home/user/worktrees/alpha-1', '01JC00000000000000000HOST1', 'alpha',
        '01JC0000000000000000000RU1', 'alpha-1', 'def456', 0, 2, '2026-11-02T09:21:00.000Z');

INSERT INTO quota_windows (label, percent_remaining, resets_at, pace, title, kind, fetched_at)
VALUES ('claude:weekly', 27.5, '2026-11-03T14:00:00.000Z', 'on_pace', 'Week', 'weekly',
        '2026-11-02T09:00:00.000Z');

INSERT INTO idempotency_keys (key, command, entity, entity_id, response, created_at)
VALUES ('spawn-key-1', 'run.start', 'run', '01JC0000000000000000000RU1', '{"value":{},"events":[1]}',
        '2026-11-02T09:10:00.000Z');

INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo,
                            canonical_path, apply_method, load_mode, added_at)
VALUES ('01JC0000000000000000000MS1', 'alpha', 'personal', 'project_file', 'AGENTS.md',
        'example/alpha', 'AGENTS.md', 'project_delivery', 'eager', '2026-11-02T09:00:00.000Z');

INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub, recorded_at,
                              recorded_by)
VALUES ('01JC0000000000000000000MR1', '01JC0000000000000000000MS1', 'blob1', 'approved', 'baseline',
        'passed', '2026-11-02T09:00:00.000Z', 'daemon');

INSERT INTO evidence (id, project_id, boundary, kind, ref, excerpt, card_id, independence_key,
                      captured_at, captured_by)
VALUES ('01JC0000000000000000000EI1', 'alpha', 'personal', 'review', '{"review":"01JC0000000000000000000RV1"}',
        'flaky test', '01JC0000000000000000000CA1', 'run:01JC0000000000000000000RU1',
        '2026-11-02T09:56:00.000Z', 'session:01JC0000000000000000000SW1');

INSERT INTO memories (id, source_id, section, text_sha256, scope_kind, scope_value, class, validity,
                      review_due_at, review_status, provenance, created_revision_id)
VALUES ('01JC0000000000000000000ME1', '01JC0000000000000000000MS1', 'Watch out for', 'sha-1',
        'project', 'alpha', 'rule', 'durable', '2027-02-02T09:00:00.000Z', 'approved', 'baseline',
        '01JC0000000000000000000MR1');

INSERT INTO memory_evidence (memory_id, evidence_id)
VALUES ('01JC0000000000000000000ME1', '01JC0000000000000000000EI1');

INSERT INTO proposals (id, project_id, boundary, target_source_id, op, memory_id, base_revision_id,
                       base_blob, diff, diff_sha256, expected_effect, created_at, created_by)
VALUES ('01JC0000000000000000000PP1', 'alpha', 'personal', '01JC0000000000000000000MS1', 'rewrite',
        '01JC0000000000000000000ME1', '01JC0000000000000000000MR1', 'blob1', '@@ -1 +1 @@',
        'diff-sha-1', 'fewer flakes', '2026-11-02T09:57:00.000Z', 'session:01JC0000000000000000000SW1');

INSERT INTO proposal_evidence (proposal_id, evidence_id)
VALUES ('01JC0000000000000000000PP1', '01JC0000000000000000000EI1');

INSERT INTO session_loads (session_id, revision_id, trigger, loaded_at)
VALUES ('01JC0000000000000000000SW1', '01JC0000000000000000000MR1', 'startup',
        '2026-11-02T09:10:02.000Z');
