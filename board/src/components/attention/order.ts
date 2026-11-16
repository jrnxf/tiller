// The order of the Inbox's rows that wait on the owner: the most urgent card
// first, and among equals the one that has waited longest. A row answered
// from this tab stays listed, after every row that still waits.

export interface Waits {
  // The card's priority, 0 (urgent) to 4. A wait with no card is an
  // orchestrator stopped on a dialog, which holds up its whole project.
  priority: number;
  // When it began to wait on the owner.
  since: string;
  answered: boolean;
}

export function mostUrgentFirst(a: Waits, b: Waits): number {
  if (a.answered !== b.answered) return a.answered ? 1 : -1;
  if (a.priority !== b.priority) return a.priority - b.priority;
  return a.since.localeCompare(b.since);
}

// The priority of a wait whose card the board does not have: the middle.
export const UNKNOWN_PRIORITY = 2;
// The priority of a wait with no card.
export const NO_CARD_PRIORITY = 0;
