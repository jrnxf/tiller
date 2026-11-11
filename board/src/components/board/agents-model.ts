import type { AgentRow, AgentsResponse, BoardRow, Project } from "../../lib/api-types";
import { ownerKey, projectKey, shownId } from "../../lib/peers";

// One project's block on the Agents view: the orchestrator first, its workers
// under it as `├` rows with the last one `└`, the way herdr nests workspaces.
export interface AgentLine {
  agent: AgentRow;
  orchestrator: boolean;
  // The herdr workspace label as drawn: the project id for an orchestrator,
  // `├ alpha-42 build` for a worker.
  label: string;
  // The short name the rail shows: the project id for an orchestrator, the
  // card id for a worker.
  name: string;
  card: BoardRow | null;
}

export interface AgentGroup {
  project: Project;
  lines: AgentLine[];
}

function workerName(agent: AgentRow, card: BoardRow | null, ambiguous: Set<string>): string {
  return card ? shownId(card, ambiguous) : (agent.card_id ?? "no card");
}

// Groups the roster by project, keeping only the projects the view knob shows
// and, with a machine filter, only the agents running on that machine.
export function agentGroups(
  roster: AgentsResponse,
  projects: Project[],
  rows: BoardRow[],
  host: string | undefined,
  ambiguous: Set<string> = new Set(),
): AgentGroup[] {
  const cards = new Map(rows.map((row) => [row.id, row]));
  // The label's tree glyph is set once the group is whole (below).
  const workerLine = (worker: AgentRow): AgentLine => {
    const card = worker.card_id ? (cards.get(worker.card_id) ?? null) : null;
    const name = workerName(worker, card, ambiguous);
    return { agent: worker, orchestrator: false, label: `${name} ${worker.role}`, name, card };
  };
  const groups: AgentGroup[] = [];
  for (const project of projects) {
    const lines: AgentLine[] = [];
    for (const top of roster.orchestrators) {
      if (top.project_id === undefined || ownerKey(top.project_id, top) !== projectKey(project)) {
        continue;
      }
      // A worker whose orchestrator has ended is listed on its own.
      if (top.role !== "orchestrator") {
        if (!host || top.host_id === host) lines.push(workerLine(top));
        continue;
      }
      const workers = top.workers.filter((worker) => !host || worker.host_id === host);
      if (host && top.host_id !== host && workers.length === 0) continue;
      if (!host || top.host_id === host) {
        lines.push({
          agent: top,
          orchestrator: true,
          label: project.id,
          name: project.id,
          card: null,
        });
      }
      for (const worker of workers) lines.push(workerLine(worker));
    }
    if (lines.length === 0) continue;
    // Only the group's last worker line closes the tree, so a worker
    // listed on its own after another worker still reads as a branch.
    let last = -1;
    for (const [index, line] of lines.entries()) if (!line.orchestrator) last = index;
    for (const [index, line] of lines.entries()) {
      if (!line.orchestrator) line.label = `${index === last ? "└" : "├"} ${line.label}`;
    }
    groups.push({ project, lines });
  }
  return groups;
}

function flatLines(groups: AgentGroup[]) {
  return groups.flatMap((group) => group.lines.map((line) => ({ group, line })));
}

export function findLine(groups: AgentGroup[], sessionId: string | undefined) {
  const all = flatLines(groups);
  return all.find(({ line }) => line.agent.session_id === sessionId) ?? all[0] ?? null;
}

// The line `by` rows from the current one in rail order, wrapping at the ends,
// so J on the last agent lands on the first.
export function stepLine(groups: AgentGroup[], sessionId: string | undefined, by: 1 | -1) {
  const all = flatLines(groups);
  if (all.length === 0) return null;
  const at = all.findIndex(({ line }) => line.agent.session_id === sessionId);
  if (at < 0) return all[0] ?? null;
  return all[(at + by + all.length) % all.length] ?? null;
}

// The first line after the current one, in rail order and wrapping, whose agent
// needs the owner. The current line comes last, so N on the only agent that
// needs you stays put instead of finding nothing.
export function nextNeeding(
  groups: AgentGroup[],
  sessionId: string | undefined,
  needs: (agent: AgentRow) => boolean,
) {
  const all = flatLines(groups);
  const at = all.findIndex(({ line }) => line.agent.session_id === sessionId);
  for (let step = 1; step <= all.length; step += 1) {
    const candidate = all[(at + step + all.length) % all.length];
    if (candidate && needs(candidate.line.agent)) return candidate;
  }
  return null;
}
