import type { BoardRow, Machine, Project } from "../../lib/api-types";
import { HOST_PROJECT, isPeerRow, peerMachines, standingOf } from "../../lib/peers";
import type { Owned } from "../../lib/peers";

// Tiller bugs (state.md, Card): `tiller bug file` gives a card this category
// and routes it to the host's bug project, or to `fleet` when it has none.
export const TILLER_BUG = "tiller-bug";

// The standing Host health card; it never moves out of `fleet`.
const HOST_HEALTH = "fleet-1";

export function isTillerBug(row: Pick<BoardRow, "category">): boolean {
  return row.category === TILLER_BUG;
}

// A Tiller bug that waits in `fleet`: no orchestrator triages it there, so the
// owner moves or closes it.
export function isUntriaged(
  row: Pick<BoardRow, "category" | "project_id" | "column" | "display_id">,
): boolean {
  return (
    isTillerBug(row) &&
    row.project_id === HOST_PROJECT &&
    row.column !== "done" &&
    row.display_id !== HOST_HEALTH
  );
}

// The Machines row of the host that owns a row: the peer it names, else this host.
export function machineOf(entity: Owned, machines: Machine[]): Machine | undefined {
  if (isPeerRow(entity)) {
    return peerMachines(machines).find((machine) => machine.label === entity.host);
  }
  return machines.find((machine) => machine.kind === "local");
}

function sameOwner(project: Owned, machine: Machine): boolean {
  return machine.kind === "local" ? !isPeerRow(project) : project.host === machine.label;
}

// The project that takes Tiller bugs on a host, when the projects read still
// lists it as open there.
export function bugProjectOf(
  machine: Machine | undefined,
  projects: Project[],
): Project | undefined {
  const id = machine?.bug_project;
  if (!id) return undefined;
  return projects.find((project) => project.id === id && sameOwner(project, machine));
}

export interface MoveTarget {
  machine: Machine;
  project: Project;
  // "host-a · tiller", the words the Move button and its preview use.
  label: string;
}

// Where the owner can move a Tiller bug: each host that has a bug project and
// takes the owner's mutations, this host first.
export function moveTargets(machines: Machine[], projects: Project[]): MoveTarget[] {
  const local = machines.filter((machine) => machine.kind === "local");
  const peers = peerMachines(machines).filter(
    (machine) => standingOf({ host: machine.label, link: machine.link }, machines).canAct,
  );
  const targets: MoveTarget[] = [];
  for (const machine of [...local, ...peers]) {
    const project = bugProjectOf(machine, projects);
    if (project) targets.push({ machine, project, label: `${machine.label} · ${project.name}` });
  }
  return targets;
}

// Both calls of one Move carry a key fixed by the card and the target, so a
// retry after a failed close replays the create instead of filing a second
// card, even when the card changed in between (protocol.md, idempotency).
export function moveKeys(row: Pick<BoardRow, "id">, target: MoveTarget) {
  const base = `move:${row.id}:${target.machine.id}:${target.project.id}`;
  return { create: `${base}:create`, close: `${base}:close` };
}

// "host-d · fleet-4", how a moved card names the card it came from.
export function cardName(row: Pick<BoardRow, "display_id">, hostLabel: string | null): string {
  return hostLabel ? `${hostLabel} · ${row.display_id}` : row.display_id;
}

// The new card's text: the bug's own text and a line that links back to it,
// with the project that filed it, which the new card has no event for.
export function movedBody(
  body: string | null | undefined,
  from: string,
  filedFrom?: string | null,
): string {
  const link = filedFrom ? `Moved from ${from}, filed from ${filedFrom}.` : `Moved from ${from}.`;
  return body ? `${body}\n\n${link}` : link;
}
