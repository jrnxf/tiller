import type { RecapResponse } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

const HOUR = 3_600_000;

// The recap the stand-in server answers: counts since the last mark, or since
// a day back before the first. `empty` has nothing new, and so does any
// recap once its mark is newer than everything.
export function recapFor(scenario: Scenario, now: number, markedAt: string | null): RecapResponse {
  const quiet = scenario === "empty" || markedAt !== null;
  const since = markedAt ?? new Date(now - 6 * HOUR).toISOString();
  return {
    seen_at: markedAt ?? new Date(now - 6 * HOUR).toISOString(),
    since,
    landed: quiet ? 0 : 3,
    decisions: quiet ? 0 : 2,
    failed_runs: quiet ? 0 : 1,
    suggestions: quiet ? 0 : 4,
  };
}
