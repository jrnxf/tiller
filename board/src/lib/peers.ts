import type { LinkState, Machine, PeerScope } from "./api-types.ts";
import { formatLisbon, minutesSince } from "./clock.ts";

// How the board treats a row that another host owns (protocol.md section 10.5).
// A row a federated read took from a paired peer carries `link`; this host's
// own rows never do. The Machines row is the one source for a peer's current
// link state, scope and last frame, so a `host.changed` event moves every row
// of that host at once.

// What a row needs to say which host owns it.
export interface Owned {
  host?: string;
  as_of?: string | null;
  link?: LinkState | null;
}

export function isPeerRow(entity: Owned): boolean {
  return entity.link !== undefined && entity.link !== null;
}

// The paired peers among the machines. A herdr-only machine has no link.
export function peerMachines(machines: Machine[]): Machine[] {
  return machines.filter((machine) => machine.link !== undefined && machine.link !== null);
}

// Every row carries a host chip once any peer is paired; a host with none
// shows the board exactly as before.
export function isFederated(machines: Machine[]): boolean {
  return peerMachines(machines).length > 0;
}

// A project slug is unique only per host (`fleet` exists on every host), so a
// peer's project, and its cards and agents, get a lane of their own. This
// host's keep the plain slug, which keeps saved view choices working.
export function ownerKey(projectId: string, owner: Owned): string {
  return isPeerRow(owner) && owner.host ? `${owner.host}/${projectId}` : projectId;
}

// The board calls each host's built-in `fleet` project for its machine
// ("host / personal-mac"), so two hosts' rows read apart. The id,
// cards and CLI keep `fleet`; only the shown name changes.
export const HOST_PROJECT = "fleet";

export function hostName(label: string | null): string {
  return label ? `host / ${label}` : "host";
}

export function displayProjects<T extends Owned & { id: string; name: string }>(
  projects: T[],
  localLabel: string | null,
): T[] {
  return projects.map((project) => {
    if (project.id !== HOST_PROJECT) return project;
    const label = isPeerRow(project) && project.host ? project.host : localLabel;
    return { ...project, name: hostName(label) };
  });
}

// A project's own key, the one lanes, saved view choices and scope sets use.
export function projectKey(project: Owned & { id: string }): string {
  return ownerKey(project.id, project);
}

export interface Standing {
  peer: boolean;
  // The owning host's label; null when no host is named.
  host: string | null;
  // Rows look and act like this host's own.
  live: boolean;
  // The link is not `live`: rows are dimmed and say "as of".
  dim: boolean;
  asOf: string | null;
  // False for a peer that is not `live`, and for a `read` pairing.
  canAct: boolean;
  // Why controls are disabled, as the tooltip and the note say it.
  reason: string | null;
}

const LOCAL: Standing = {
  peer: false,
  host: null,
  live: true,
  dim: false,
  asOf: null,
  canAct: true,
  reason: null,
};

const REFUSAL: Record<string, string> = {
  revoked: "key revoked",
  unknown_key: "key unknown there",
  bad_sig: "signature rejected",
  clock_skew: "clocks differ too much",
  replayed: "it saw this request before",
  version: "version mismatch",
  accept_off: "it is not accepting boards",
  host_changed: "its database was replaced",
};

// "key revoked" for the reason code a peer's `refused` frame carries.
export function refusalLabel(reason: string | null | undefined): string | null {
  if (!reason) return null;
  return REFUSAL[reason] ?? reason.replaceAll("_", " ");
}

function refusalText(reason: string | null | undefined): string {
  const label = refusalLabel(reason);
  return label ? `: ${label}` : "";
}

// "host-b is unreachable since Tue 14:02": the sentence a disabled control
// carries as its tooltip.
export function linkReason(
  label: string,
  link: LinkState,
  since: string | null,
  refusal?: string | null,
): string {
  const when = formatLisbon(since);
  switch (link) {
    case "live":
      return "";
    case "lagging":
      return when ? `${label} is not answering since ${when}` : `${label} is not answering`;
    case "down":
      return when ? `${label} is unreachable since ${when}` : `${label} is unreachable`;
    case "refused":
      return `${label} refused the link${refusalText(refusal)}`;
    case "never":
      return `${label} has not connected yet`;
  }
}

export const VIEW_ONLY_REASON = "view only pairing";

function scopeOf(machine: Machine | undefined): PeerScope {
  return machine?.peer_scope ?? "act";
}

// Where a row stands: the Machines row of its host decides the link, and falls
// back to what the row itself carried when the machine is not known yet.
export function standingOf(entity: Owned, machines: Machine[]): Standing {
  if (!isPeerRow(entity)) return LOCAL;
  const host = entity.host ?? null;
  const machine = peerMachines(machines).find((m) => m.label === host);
  const link = machine?.link ?? entity.link ?? "never";
  const asOf = entity.as_of ?? machine?.last_seen_at ?? null;
  if (link === "live") {
    const readOnly = scopeOf(machine) === "read";
    return {
      peer: true,
      host,
      live: true,
      dim: false,
      asOf,
      canAct: !readOnly,
      reason: readOnly ? VIEW_ONLY_REASON : null,
    };
  }
  return {
    peer: true,
    host,
    live: false,
    dim: true,
    asOf,
    canAct: false,
    reason: linkReason(host ?? "that host", link, asOf, machine?.link_reason),
  };
}

// "as of Tue 14:02, 6 min ago", the weekday and time in Europe/Lisbon.
export function asOfText(asOf: string | null, now?: number): string {
  const when = formatLisbon(asOf);
  if (!when) return "as of never";
  const minutes = minutesSince(asOf, now);
  if (minutes === null || minutes < 1) return `as of ${when}, just now`;
  if (minutes < 60) return `as of ${when}, ${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `as of ${when}, ${hours} h ago`;
  return `as of ${when}, ${Math.floor(hours / 24)} d ago`;
}

// The display ids that more than one host uses; those are shown host-qualified.
export function ambiguousIds(rows: { display_id: string; host?: string }[]): Set<string> {
  const hosts = new Map<string, Set<string>>();
  for (const row of rows) {
    const set = hosts.get(row.display_id) ?? new Set<string>();
    set.add(row.host ?? "");
    hosts.set(row.display_id, set);
  }
  return new Set([...hosts].filter(([, owners]) => owners.size > 1).map(([id]) => id));
}

// `host-b · fleet-1` when the id is ambiguous, the plain id otherwise.
export function shownId(
  row: { display_id: string; host?: string },
  ambiguous: Set<string>,
): string {
  return row.host && ambiguous.has(row.display_id)
    ? `${row.host} · ${row.display_id}`
    : row.display_id;
}

// The peer's label for a row a peer owns, or undefined for this host's own. It
// is the `host` a mutation carries and the `?host=` a single-entity read takes.
export function ownerOf(entity: Owned): string | undefined {
  return isPeerRow(entity) && entity.host ? entity.host : undefined;
}

// The `host` field a mutation carries for a row a peer owns; nothing for this
// host's own, so a host with no peers sends the bodies it always sent.
export function hostField(entity: Owned): { host?: string } {
  const host = ownerOf(entity);
  return host ? { host } : {};
}

// Pane focus is a local act: the board host does not forward it (protocol.md
// section 7.4), so the sentence a peer's pane gives is the same on every screen.
export function focusReason(host: string | undefined): string | null {
  return host ? `${host} owns this pane: open it from that host` : null;
}

export interface ProjectGroup<T> {
  // The owning host's label; null when no peer is paired and no groups show.
  host: string | null;
  peer: boolean;
  standing: Standing;
  projects: T[];
}

// The new-card picker groups projects by owner host and turns off a group whose
// link is not `live` or whose pairing only reads (protocol.md section 10.5).
export function projectGroups<T extends Owned>(
  projects: T[],
  machines: Machine[],
  localLabel: string | null,
): ProjectGroup<T>[] {
  const groups = new Map<string, ProjectGroup<T>>();
  for (const project of projects) {
    const peer = isPeerRow(project);
    const host = peer ? (project.host ?? null) : localLabel;
    const key = `${peer ? "peer" : "local"}:${host ?? ""}`;
    const group = groups.get(key) ?? {
      host,
      peer,
      standing: standingOf(project, machines),
      projects: [],
    };
    group.projects.push(project);
    groups.set(key, group);
  }
  // This host's projects first, then each peer's in the order they arrived.
  const all = [...groups.values()];
  return [...all.filter((group) => !group.peer), ...all.filter((group) => group.peer)];
}

// Where a row's project sits in a project list: slugs are unique only per
// host, so the match is on the owner-qualified key.
export function projectIndex(
  projects: (Owned & { id: string })[],
  row: Owned & { project_id: string },
): number {
  const key = ownerKey(row.project_id, row);
  return projects.findIndex((project) => ownerKey(project.id, project) === key);
}
