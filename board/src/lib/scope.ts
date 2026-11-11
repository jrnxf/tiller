import type { AgentRow, AgentsResponse, AttentionResponse, BoardRow, Project } from "./api-types";
import { ownerKey, projectKey } from "./peers";
import { visibleProjects } from "./prefs";
import type { Prefs } from "./prefs";

// A project counts as being on a machine when one of its cards runs there.
// Without a machine filter every project is on it.
export function projectsOnHost(rows: BoardRow[], host: string | undefined): Set<string> | null {
  if (!host) return null;
  return new Set(
    rows.filter((row) => row.host_id === host).map((row) => ownerKey(row.project_id, row)),
  );
}

// What the screens show: the projects the view knob lets through, narrowed to
// the machine filter.
export function scopedProjects(
  prefs: Prefs,
  projects: Project[],
  rows: BoardRow[],
  host: string | undefined,
): Project[] {
  const shown = visibleProjects(prefs, projects);
  const onHost = projectsOnHost(rows, host);
  return onHost ? shown.filter((project) => onHost.has(projectKey(project))) : shown;
}

// The agents that wait on an answer no hook asked for (`waiting_input`): a
// dialog only the screen shows, such as Claude Code's folder trust dialog
// (protocol.md 7.4, Prompt). The Inbox lists them with the permission prompts.
export function dialogWaits(agents: AgentsResponse): AgentRow[] {
  return agents.orchestrators
    .flatMap((agent) => [agent, ...agent.workers])
    .filter((agent) => agent.activity === "waiting_input");
}

// Whether a decision, a prompt or a waiting agent is in scope, by its card. A
// card that is not on the board any more still counts. An agent with no card
// (an orchestrator) goes by its project and machine.
export function scopeTest(
  rows: BoardRow[],
  scope: Set<string>,
  host: string | undefined,
): { card: (cardId: string) => boolean; agent: (agent: AgentRow) => boolean } {
  const cards = new Map(rows.map((row) => [row.id, row]));
  const card = (cardId: string) => {
    const row = cards.get(cardId);
    return !row || (scope.has(ownerKey(row.project_id, row)) && (!host || row.host_id === host));
  };
  const agent = (row: AgentRow) =>
    row.card_id !== null
      ? card(row.card_id)
      : scope.has(ownerKey(row.project_id ?? "", row)) && (!host || row.host_id === host);
  return { card, agent };
}

// The Inbox badge: open decisions, permission prompts and agents that wait on
// a dialog, when they are in scope.
export function attentionCount(
  attention: AttentionResponse,
  rows: BoardRow[],
  scope: Set<string>,
  host: string | undefined,
  waits: AgentRow[] = [],
): number {
  const inScope = scopeTest(rows, scope, host);
  return (
    attention.items.filter((item) => inScope.card(item.card_id)).length +
    attention.permission_prompts.filter((prompt) => inScope.card(prompt.card_id)).length +
    waits.filter(inScope.agent).length
  );
}
