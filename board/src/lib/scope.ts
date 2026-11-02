import type { AttentionResponse, BoardRow, Project } from "./api-types";
import { visibleProjects } from "./prefs";
import type { Prefs } from "./prefs";

// A project counts as being on a machine when one of its cards runs there.
// Without a machine filter every project is on it.
export function projectsOnHost(rows: BoardRow[], host: string | undefined): Set<string> | null {
  if (!host) return null;
  return new Set(rows.filter((row) => row.host_id === host).map((row) => row.project_id));
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
  return onHost ? shown.filter((project) => onHost.has(project.id)) : shown;
}

// The Inbox badge: open decisions and permission prompts whose card is in
// scope. A card that is not on the board any more still counts.
export function attentionCount(
  attention: AttentionResponse,
  rows: BoardRow[],
  scope: Set<string>,
  host: string | undefined,
): number {
  const cards = new Map(rows.map((row) => [row.id, row]));
  const inScope = (cardId: string) => {
    const row = cards.get(cardId);
    return !row || (scope.has(row.project_id) && (!host || row.host_id === host));
  };
  return (
    attention.items.filter((item) => inScope(item.card_id)).length +
    attention.permission_prompts.filter((prompt) => inScope(prompt.card_id)).length
  );
}
