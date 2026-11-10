import type { AwayBrief, AwayBriefResponse } from "./api-types";

// The away contract the fixtures hold: alpha and beta, six new runs, existing
// grants may merge. Times are fixed so the labels in tests do not move.
export function awayBrief(running: boolean): AwayBrief {
  return {
    approval_id: "01JAWAY0000000000000000000",
    away: running,
    started_at: "2026-11-02T20:40:00.000Z",
    until_at: running ? "2026-11-12T18:00:00.000Z" : "2026-11-03T18:00:00.000Z",
    ended_at: running ? null : "2026-11-03T08:15:00.000Z",
    ended_reason: running ? null : "ended by the owner",
    orders: "Keep going on alpha-42 and alpha-43. Hold anything that changes the schema.",
    projects: ["alpha", "beta"],
    merge: "grants",
    spend: {
      max_new_runs: 6,
      new_runs: running ? 2 : 4,
      quota_floor_percent: 20,
      cap_reached: false,
    },
    runs: running
      ? []
      : [
          {
            run_id: "01JRUN00000000000000000001",
            card: "alpha-42",
            title: "Add the machines summary to the sidebar",
            role: "builder",
            started_at: "2026-11-02T21:10:00.000Z",
            outcome: "landed",
            status: null,
          },
          {
            run_id: "01JRUN00000000000000000002",
            card: "alpha-42",
            title: "Add the machines summary to the sidebar",
            role: "reviewer",
            started_at: "2026-11-02T22:30:00.000Z",
            outcome: "reported",
            status: null,
          },
          {
            run_id: "01JRUN00000000000000000003",
            card: "alpha-43",
            title: "Move the poller behind a trait",
            role: "builder",
            started_at: "2026-11-03T01:05:00.000Z",
            outcome: null,
            status: null,
          },
          {
            run_id: "01JRUN00000000000000000004",
            card: "beta-4",
            title: "Fix the export_spec tests",
            role: "builder",
            started_at: "2026-11-03T03:17:00.000Z",
            outcome: "failed",
            status: null,
          },
        ],
    landed: running
      ? []
      : [
          {
            card: "alpha-42",
            title: "Add the machines summary to the sidebar",
            state: "done",
            outcome: "landed",
          },
        ],
    merged_by_grant: running
      ? []
      : [
          {
            card: "alpha-42",
            action: "merge",
            pr_url: "https://github.com/example/alpha/pull/48",
            grant_id: "01JGRANT00000000000000003A",
            state: "accepted",
            at: "2026-11-03T06:20:00.000Z",
          },
        ],
    waiting_decisions: running
      ? []
      : [
          {
            attention_id: "01JATTN0000000000000000000",
            card: "alpha-41",
            kind: "decision",
            question: "Pick the retry policy for the export job",
            recommendation: null,
            standing: "pending",
            answer: null,
          },
        ],
    waiting_approvals: running
      ? []
      : [
          {
            approval_id: "01JAPPR0000000000000000001",
            card: "alpha-44",
            action: "merge",
            requested_at: "2026-11-03T05:40:00.000Z",
          },
        ],
  };
}

// `idle`: the owner has never been away. `running`: away now. `back`: the
// contract ended and the brief waits.
export function awayFor(scenario: "idle" | "running" | "back"): AwayBriefResponse {
  return { brief: scenario === "idle" ? null : awayBrief(scenario === "running") };
}
