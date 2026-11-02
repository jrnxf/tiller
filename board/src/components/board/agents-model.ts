import type { AgentRow, AgentsResponse, BoardRow, Project } from "../../lib/api-types";

// One project's block on the Agents view: the orchestrator first, its workers
// under it as `├` rows with the last one `└`, the way herdr nests workspaces.
export interface AgentLine {
  agent: AgentRow;
  orchestrator: boolean;
  // The herdr workspace label as drawn: the project id for an orchestrator,
  // `├ alpha-42 build` for a worker.
  label: string;
  card: BoardRow | null;
}

export interface AgentGroup {
  project: Project;
  lines: AgentLine[];
}

function workerLabel(glyph: string, agent: AgentRow, card: BoardRow | null): string {
  const id = card?.display_id ?? agent.card_id ?? "no card";
  return `${glyph} ${id} ${agent.role}`;
}

// Groups the roster by project, keeping only the projects the view knob shows
// and, with a machine filter, only the agents running on that machine.
export function agentGroups(
  roster: AgentsResponse,
  projects: Project[],
  rows: BoardRow[],
  host: string | undefined,
): AgentGroup[] {
  const cards = new Map(rows.map((row) => [row.id, row]));
  const groups: AgentGroup[] = [];
  for (const project of projects) {
    for (const orchestrator of roster.orchestrators) {
      if (orchestrator.project_id !== project.id) continue;
      const workers = orchestrator.workers.filter((worker) => !host || worker.host_id === host);
      if (host && orchestrator.host_id !== host && workers.length === 0) continue;
      const lines: AgentLine[] = [];
      if (!host || orchestrator.host_id === host) {
        lines.push({ agent: orchestrator, orchestrator: true, label: project.id, card: null });
      }
      workers.forEach((worker, index) => {
        const card = worker.card_id ? (cards.get(worker.card_id) ?? null) : null;
        const glyph = index === workers.length - 1 ? "└" : "├";
        lines.push({
          agent: worker,
          orchestrator: false,
          label: workerLabel(glyph, worker, card),
          card,
        });
      });
      groups.push({ project, lines });
    }
  }
  return groups;
}

export function findLine(groups: AgentGroup[], sessionId: string | undefined) {
  const all = groups.flatMap((group) => group.lines.map((line) => ({ group, line })));
  return all.find(({ line }) => line.agent.session_id === sessionId) ?? all[0] ?? null;
}
