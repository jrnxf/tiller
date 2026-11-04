---
role: curator
description: A sampled routine card whose only hiccup was a tool version the builder could have checked ends with nothing worth saving; no learning, no proposal.
max_turns: 20
allowed_tools: [Read, Glob, Grep]
---

Retro `01JC0000000000000000000RT7` of card alpha-44.

```json
{
  "retro": {"id": "01JC0000000000000000000RT7", "card_id": "01JC0000000000000000000CA44", "depth": "full", "reason": "sample", "state": "pending",
            "counts": {"elapsed_minutes": 95, "attempts": {"builder": 1, "reviewer": 1}, "attention": {}, "findings": {"high": 0, "medium": 0, "low": 1},
                       "guard_breaches": 0, "fix_cards": 0, "runs": 2, "run_outcomes": {"landed": 1, "reported": 1}, "models": ["opus"], "status_lines": 4}},
  "card": {"id": "01JC0000000000000000000CA44", "tag": "[alpha-44]", "title": "Add the CSV export button", "kind": "ship", "size": "S", "outcome": "landed",
           "acceptance": "the button downloads export.csv with every visible row", "created_at": "2026-11-03T09:00:00.000Z", "closed_at": "2026-11-03T10:35:00.000Z", "project": "alpha"},
  "fix_cards": [],
  "runs": [
    {"id": "01JC0000000000000000000RU44", "role": "builder", "attempt": 1, "model": "opus", "outcome": "landed", "last_status_line": "done: PR #91 merged",
     "transcripts": ["transcripts/claude-build-44.jsonl"]},
    {"id": "01JC0000000000000000000RV44", "role": "reviewer", "attempt": 1, "model": "opus", "outcome": "reported", "last_status_line": "done pass; high 0 medium 0 low 1",
     "transcripts": ["transcripts/claude-review-44.jsonl"]}
  ],
  "attention": [],
  "reviews": [{"reviewer": "code-review", "coverage": "diff", "verdict": "pass", "findings_high": 0, "findings_medium": 0, "findings_low": 1}],
  "events": [
    {"seq": 910, "kind": "run.started", "entity": "run", "entity_id": "01JC0000000000000000000RU44"},
    {"seq": 914, "kind": "run.status", "entity": "run", "entity_id": "01JC0000000000000000000RU44"},
    {"seq": 921, "kind": "pr.opened", "entity": "pr", "entity_id": "91"},
    {"seq": 930, "kind": "run.ended", "entity": "run", "entity_id": "01JC0000000000000000000RV44"},
    {"seq": 933, "kind": "card.closed", "entity": "card", "entity_id": "01JC0000000000000000000CA44"}
  ],
  "evidence": [],
  "earlier_retros": [],
  "finish": {
    "nothing": "tiller retro finish 01JC0000000000000000000RT7 --nothing",
    "learning": "tiller retro finish 01JC0000000000000000000RT7 --learning <one sentence> --evidence '<json>'",
    "proposed": "tiller retro finish 01JC0000000000000000000RT7 --proposed <proposal>"
  }
}
```
