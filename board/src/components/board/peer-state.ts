import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";

import type { Machine, ProjectsResponse } from "../../lib/api-types";
import { boardQuery, machinesQuery } from "../../lib/queries";
import { ambiguousIds, displayProjects, isFederated, standingOf } from "../../lib/peers";
import type { Owned, Standing } from "../../lib/peers";

const NONE: Machine[] = [];
const NO_ROWS: { display_id: string; host?: string }[] = [];

// The hooks below read the cache and never fetch: the Machines and board
// reads are the screens' own, and a view that has not got them yet behaves as
// a host with no peers, which is how every screen looked before federation.
export function useMachineRows(): Machine[] {
  return useQuery({ ...machinesQuery(), enabled: false }).data?.machines ?? NONE;
}

// True once any peer is paired; only then do rows carry a host chip.
export function useFederated(): boolean {
  return isFederated(useMachineRows());
}

// The projects read with each host's `fleet` project shown as "Host · <machine>".
export function useNamedProjects(data: ProjectsResponse): ProjectsResponse;
export function useNamedProjects(data: ProjectsResponse | undefined): ProjectsResponse | undefined;
export function useNamedProjects(data: ProjectsResponse | undefined) {
  const machines = useMachineRows();
  const local = machines.find((machine) => machine.kind === "local")?.label ?? null;
  return useMemo(
    () => (data ? { ...data, projects: displayProjects(data.projects, local) } : data),
    [data, local],
  );
}

// The label of the host that owns a row: the one it names, else this host's.
export function useHostLabel(entity: Owned): string | null {
  const machines = useMachineRows();
  if (entity.host) return entity.host;
  return machines.find((machine) => machine.kind === "local")?.label ?? null;
}

// Where a row stands: live like local, or dimmed with a reason its controls
// are off (protocol.md section 10.5).
export function useStanding(entity: Owned): Standing {
  return standingOf(entity, useMachineRows());
}

// The display ids more than one host uses, which are shown host-qualified.
export function useAmbiguousIds(): Set<string> {
  const rows = useQuery({ ...boardQuery(), enabled: false }).data?.rows ?? NO_ROWS;
  return useMemo(() => ambiguousIds(rows), [rows]);
}
