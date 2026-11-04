---
role: curator
description: An incident retro where the owner corrected the same mistake on two cards ends with one learning that names the cause and points at the record, without quoting the owner.
max_turns: 20
allowed_tools: [Read, Glob, Grep]
---

Retro `01JC0000000000000000000RT9` of card alpha-61.

```json
{
  "retro": {"id": "01JC0000000000000000000RT9", "card_id": "01JC0000000000000000000CA61", "depth": "full", "reason": "incident", "state": "pending",
            "counts": {"elapsed_minutes": 410, "attempts": {"builder": 2}, "attention": {"decision": 1}, "findings": {"high": 0, "medium": 0, "low": 0},
                       "guard_breaches": 0, "fix_cards": 0, "runs": 2, "run_outcomes": {"failed": 1, "landed": 1}, "models": ["opus"], "status_lines": 7}},
  "card": {"id": "01JC0000000000000000000CA61", "tag": "[alpha-61]", "title": "Move the nightly job to the new scheduler", "kind": "ship", "size": "M", "outcome": "landed",
           "acceptance": "the nightly job runs from the new scheduler and the old entry is removed", "created_at": "2026-11-02T08:00:00.000Z", "closed_at": "2026-11-02T14:50:00.000Z", "project": "alpha"},
  "fix_cards": [],
  "runs": [
    {"id": "01JC0000000000000000000RU61", "role": "builder", "attempt": 1, "model": "opus", "outcome": "failed", "outcome_note": "pushed branch rewritten; relaunched as a new attempt",
     "last_status_line": "working: rebasing fm/alpha-61 onto main", "transcripts": ["transcripts/claude-build-61-1.jsonl"]},
    {"id": "01JC0000000000000000000RU62", "role": "builder", "attempt": 2, "model": "opus", "outcome": "landed", "last_status_line": "done: PR #97 merged",
     "transcripts": ["transcripts/claude-build-61-2.jsonl"]}
  ],
  "attention": [
    {"id": "01JC0000000000000000000AT61", "kind": "decision", "question": "The builder rebased fm/alpha-61 after it was pushed and the reviewer's comments no longer match. Relaunch as a new attempt from the pushed head?",
     "answer": "Yes. And this is the second time this week a builder rewrote a pushed branch; alpha-57 did the same thing on Monday.", "answered_via": "board", "answered_at": "2026-11-02T11:20:00.000Z"}
  ],
  "reviews": [],
  "events": [
    {"seq": 1201, "kind": "run.started", "entity": "run", "entity_id": "01JC0000000000000000000RU61"},
    {"seq": 1210, "kind": "pr.opened", "entity": "pr", "entity_id": "97"},
    {"seq": 1218, "kind": "attention.opened", "entity": "attention", "entity_id": "01JC0000000000000000000AT61"},
    {"seq": 1225, "kind": "attention.answered", "entity": "attention", "entity_id": "01JC0000000000000000000AT61"},
    {"seq": 1226, "kind": "run.ended", "entity": "run", "entity_id": "01JC0000000000000000000RU61"},
    {"seq": 1227, "kind": "run.started", "entity": "run", "entity_id": "01JC0000000000000000000RU62"},
    {"seq": 1260, "kind": "card.closed", "entity": "card", "entity_id": "01JC0000000000000000000CA61"}
  ],
  "evidence": [
    {"id": "01JC0000000000000000000EV57", "kind": "owner_message", "ref": {"attention": "01JC0000000000000000000AT57", "card": "01JC0000000000000000000CA57"},
     "excerpt": "builder force-pushed fm/alpha-57 over the reviewer's head", "captured_at": "2026-10-31T16:00:00.000Z", "captured_by": "owner"}
  ],
  "earlier_retros": [],
  "finish": {
    "nothing": "tiller retro finish 01JC0000000000000000000RT9 --nothing",
    "learning": "tiller retro finish 01JC0000000000000000000RT9 --learning <one sentence> --evidence '<json>'",
    "proposed": "tiller retro finish 01JC0000000000000000000RT9 --proposed <proposal>"
  }
}
```
