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
  // The agent entered its activity then, not when the scenario began.
  since?: string;
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

// The ids of the cards Cancel card ended.
const cancelled = new Set<string>();

export function cancelCard(card: string): void {
  cancelled.add(card);
}

export function cardCancelled(card: string): boolean {
  return cancelled.has(card);
}

// Back to the scenario's starting point, for tests.
export function resetLifecycleFixture(): void {
  changes.clear();
  cancelled.clear();
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

// The `dialog` scenario's starting point: alpha-42 and beta-7 wait on a dialog
// no hook asked for, so their hooks say `waiting_input` (P7).
function dialogSeed(name: string | null, now: number): SessionChange {
  return name === "alpha-42-build" || name === "beta-7-build"
    ? { activity: "waiting_input", at: minutes(now, -0.2), since: minutes(now, -0.2) }
    : {};
}

function seedOf(
  scenario: string,
  name: string | null,
  now: number,
): SessionChange & { quiet?: number } {
  if (scenario === "lifecycle") return seedFor(name, now);
  if (scenario === "dialog") return dialogSeed(name, now);
  return {};
}

// The agent as the run controls left it, or null once it was stopped.
export function withLifecycle(agent: AgentRow, scenario: string, now: number): AgentRow | null {
  const change = changes.get(agent.session_id) ?? {};
  if (change.stopped) return null;
  const seed = change.fresh ? {} : seedOf(scenario, agent.herdr_name, now);
  const { quiet, ...seeded } = seed;
  const state = { ...seeded, ...change };
  const at = quiet === undefined ? state.at : minutes(now, -quiet);
  return {
    ...agent,
    ...(state.activity ? { activity: state.activity } : {}),
    ...(at ? { activity_observed_at: at, last_output_at: at } : {}),
    ...(state.pause ? { pause: state.pause } : {}),
    ...(state.since ? { state_since: state.since } : {}),
  };
}
