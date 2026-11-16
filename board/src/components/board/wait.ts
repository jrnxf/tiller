import type { LaneHealth } from "../states/degraded";

// What a card that has not started waits for, in the owner's words. The
// board cannot start a run (cli-board-parity.md, run control), so a queued
// card must say whose move is next and what the owner can do about it.
export interface CardWait {
  // The short answer, for the card's details.
  title: string;
  // One more sentence: what happens next, or what to do.
  detail: string;
  // The wait will not end by itself.
  stuck: boolean;
  // The owner can end the wait by starting the project's orchestrator.
  startable: boolean;
  // Only the orchestrator's turn is in the way, so the owner's own
  // `tiller spawn` starts the card now.
  direct: boolean;
}

export interface WaitInput {
  // The card is open, sits in Queued and has no live run.
  queued: boolean;
  // A hold or an open dependency keeps the card in the queue.
  gated: boolean;
  projectName: string;
  // The label of the peer that owns the card, when one does.
  peer: string | null;
  // False while a peer's link is down: nothing about it is fresh.
  reachable: boolean;
  // Undefined until the agents read arrives.
  orchestrator: { activity: string } | null | undefined;
  health: LaneHealth;
}

export function cardWait(input: WaitInput): CardWait | null {
  if (!input.queued) return null;
  const { projectName } = input;
  // A hold shows in its own note and a dependency in the Status line, so the
  // row would only say it a third time.
  if (input.gated) return null;
  if (!input.reachable) {
    return {
      title: `${input.peer ?? "Its host"} to answer`,
      detail: `Nothing here is fresh while ${input.peer ?? "the host"} does not answer, so the card may have started already.`,
      stuck: true,
      startable: false,
      direct: false,
    };
  }
  if (input.orchestrator === undefined) return null;
  if (input.orchestrator === null) {
    return {
      title: `${projectName} has no orchestrator`,
      detail:
        "Nothing starts this card until the orchestrator runs. Start it, and it picks the card up.",
      stuck: true,
      startable: true,
      direct: false,
    };
  }
  if (input.health.kind === "disconnected") {
    return {
      title: "The terminal holder to answer",
      detail: `The ${projectName} orchestrator cannot be reached, last seen ${input.health.ago} ago. See Health for the recovery step.`,
      stuck: true,
      startable: false,
      direct: false,
    };
  }
  if (input.health.kind === "stale") {
    return {
      title: `The ${projectName} orchestrator, silent for ${input.health.ago}`,
      detail:
        "It may be stuck. Open its terminal on the Agents screen, or restart it from its lane on the board.",
      stuck: true,
      startable: false,
      direct: false,
    };
  }
  const busy = input.orchestrator.activity === "working";
  return {
    title: `The ${projectName} orchestrator`,
    detail: busy
      ? "It is working now. It starts a run when it reaches this card and a slot is free."
      : "It starts a run on its next turn, when a slot is free.",
    stuck: false,
    startable: false,
    direct: true,
  };
}

const SPAWN_ROLE: Record<string, string> = { ship: "builder", scout: "scout" };

// The shell command that starts the card's first run now, for the kinds
// whose first role is fixed. Null for the others.
export function spawnCommand(displayId: string, kind: string): string | null {
  const role = SPAWN_ROLE[kind];
  return role ? `tiller spawn ${displayId} ${role}` : null;
}
