// What the board's run controls did to a fixture session (protocol.md 7.4,
// P11): a pause, a pane brought back by Relaunch or Resume session, an agent
// stopped, an orchestrator restarted. The fixture responses are built fresh on
// each read, so the changes live here.
import type { AgentRow, PauseInfo } from "./api-types.ts";
import { lisbonDate, lisbonInstant } from "./lisbon-input.ts";

interface SessionChange {
  // Drops the scenario's starting state, as a relaunch or a restart does.
  fresh?: boolean;
  pause?: PauseInfo | null;
  activity?: string;
  // When the change was made: the agent's activity is observed then.
  at?: string;
  stopped?: boolean;
  // A first stop found a draft in the composer.
  drafted?: boolean;
}

const changes = new Map<string, SessionChange>();

export function changeSession(session: string, change: SessionChange): void {
  changes.set(session, { ...changes.get(session), ...change });
}

export function sessionStopped(session: string): boolean {
  return changes.get(session)?.stopped === true;
}

export function sessionDrafted(session: string): boolean {
  return changes.get(session)?.drafted === true;
}

// Back to the scenario's starting point, for tests.
export function resetLifecycleFixture(): void {
  changes.clear();
}

const minutes = (now: number, delta: number) => new Date(now + delta * 60_000).toISOString();

// The `lifecycle` scenario's starting point, by herdr pane name: beta-7's run
// is paused until tomorrow morning, alpha-44's pane is gone, and gamma's
// orchestrator has been silent long enough for its lane to read stale.
function seedFor(name: string | null, now: number): SessionChange & { quiet?: number } {
  if (name === "beta-7-build") {
    return {
      pause: {
        until: lisbonInstant(lisbonDate(now, 1), "09:00") ?? minutes(now, 18 * 60),
        at: minutes(now, -35),
        by: "board",
        reason: "Waiting for the vendor's new bucket.",
      },
    };
  }
  if (name === "alpha-44-build") return { activity: "ended", at: minutes(now, -14) };
  if (name === "gamma-orch") return { quiet: 41 };
  return {};
}

// The agent as the run controls left it, or null once it was stopped.
export function withLifecycle(agent: AgentRow, scenario: string, now: number): AgentRow | null {
  const change = changes.get(agent.session_id) ?? {};
  if (change.stopped) return null;
  const seed = scenario === "lifecycle" && !change.fresh ? seedFor(agent.herdr_name, now) : {};
  const { quiet, ...seeded } = seed;
  const state = { ...seeded, ...change };
  const at = quiet === undefined ? state.at : minutes(now, -quiet);
  return {
    ...agent,
    ...(state.activity ? { activity: state.activity } : {}),
    ...(at ? { activity_observed_at: at, last_output_at: at } : {}),
    ...(state.pause ? { pause: state.pause } : {}),
  };
}
