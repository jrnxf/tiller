import type { QueryClient } from "@tanstack/react-query";

import type {
  AgentsResponse,
  AttentionItem,
  AttentionResponse,
  BoardResponse,
  BoardRow,
  CardDetail,
  ChangeEvent,
  LinkEvent,
  Machine,
  MachinesResponse,
  PeerEvent,
  PeerResetEvent,
  Project,
  ProjectsResponse,
  QuotaResponse,
  QuotaWindow,
  Snapshot,
} from "./api-types";
import { connectionStore } from "./connection";
import { isPeerRow, ownerKey, ownerOf } from "./peers";
import { SNAPSHOT_PATH, refreshFromSnapshot } from "./boot";
import { recordProgress } from "./receipts";
import { isSignedOut, signedOutStore } from "./session";
import {
  API_KEY_PREFIX,
  agentsPath,
  attentionPath,
  awayBriefPath,
  CONTRIBUTIONS_PREFIX,
  fetchJson,
  machinesPath,
  projectSetupPath,
  projectsPath,
  quotaPath,
  queryKeyFor,
  recapPath,
  sessionPath,
  signalsPath,
  startSettingsPath,
} from "./queries";
import type { QueryPath } from "./queries";

const EVENTS_PATH = "/api/v1/events";

const attentionKey = queryKeyFor("/api/v1/attention?state=open");

function hasRev(value: unknown): value is { rev: number } {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { rev?: unknown }).rev === "number"
  );
}

function hasState(value: unknown): value is { state: string } {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { state?: unknown }).state === "string"
  );
}

// Insert or replace by `key`, unless the cache already holds a higher `rev`
// (a later write may already be included in `entity_state`; the client keeps
// whichever rev is higher, protocol.md section 7.5).
function upsert<T>(list: T[], item: T, same: (other: T) => boolean): T[] {
  const index = list.findIndex(same);
  if (index === -1) return [...list, item];
  const current = list[index];
  if (current !== undefined && hasRev(current) && hasRev(item) && current.rev > item.rev) {
    return list;
  }
  return list.map((existing, i) => (i === index ? item : existing));
}

function upsertBoardRow(
  response: BoardResponse | undefined,
  row: BoardRow,
): BoardResponse | undefined {
  if (!response) return response;
  const index = response.rows.findIndex(
    (existing) => existing.id === row.id && ownerOf(existing) === ownerOf(row),
  );
  const rows =
    index === -1
      ? [...response.rows, row]
      : response.rows.map((existing, i) => (i === index ? row : existing));
  return { ...response, rows };
}

function writeBoard(queryClient: QueryClient, row: BoardRow) {
  // Only the unfiltered board can take a row in place: a filtered list may need
  // the card to enter or leave, which only the server can say. Those refetch
  // quietly; their old data stays on screen until the answer lands.
  const unfiltered = queryKeyFor("/api/v1/board?filter=all");
  queryClient.setQueryData<BoardResponse | undefined>(unfiltered, (old) =>
    upsertBoardRow(old, row),
  );
  queryClient.invalidateQueries({
    queryKey: ["api"],
    predicate: (query) => {
      const path = String(query.queryKey[1] ?? "");
      return path.startsWith("/api/v1/board") && path !== "/api/v1/board?filter=all";
    },
    refetchType: "active",
  });
}

// An id is unique per host, so a peer's item is another item than this host's.
function sameAttention(a: AttentionItem, b: AttentionItem): boolean {
  return a.id === b.id && ownerOf(a) === ownerOf(b);
}

function writeAttention(queryClient: QueryClient, item: AttentionItem) {
  queryClient.setQueryData<AttentionResponse | undefined>(attentionKey, (old) => {
    if (!old) return old;
    const closed = item.answered_at !== null || item.withdrawn_at !== null;
    if (closed) return { ...old, items: old.items.filter((other) => !sameAttention(other, item)) };
    return { ...old, items: upsert(old.items, item, (other) => sameAttention(other, item)) };
  });
}

function writeMachine(queryClient: QueryClient, machine: Machine) {
  queryClient.setQueryData<MachinesResponse | undefined>(queryKeyFor(machinesPath), (old) =>
    old ? { machines: upsert(old.machines, machine, (other) => other.id === machine.id) } : old,
  );
}

function writeQuota(queryClient: QueryClient, window: QuotaWindow) {
  queryClient.setQueryData<QuotaResponse | undefined>(queryKeyFor(quotaPath), (old) =>
    old
      ? { ...old, windows: upsert(old.windows, window, (other) => other.label === window.label) }
      : old,
  );
}

function writeProject(queryClient: QueryClient, project: Project) {
  queryClient.setQueryData<ProjectsResponse | undefined>(queryKeyFor(projectsPath), (old) =>
    old
      ? {
          projects: upsert(
            old.projects,
            project,
            (other) => ownerKey(other.id, other) === ownerKey(project.id, project),
          ),
        }
      : old,
  );
}

// A deleted or archived (retired) project leaves this host's list at once; a
// peer's row of the same id is another project and stays. Its cards, runs,
// sessions and items went or were stopped with it, so every read that may
// still show one is fetched again.
export function onProjectGone(queryClient: QueryClient, projectId: string) {
  queryClient.setQueryData<ProjectsResponse | undefined>(queryKeyFor(projectsPath), (old) =>
    old
      ? {
          projects: old.projects.filter((other) => other.id !== projectId || isPeerRow(other)),
        }
      : old,
  );
  void queryClient.invalidateQueries({ queryKey: API_KEY_PREFIX, refetchType: "active" });
}

// Mark the reads at `paths` stale: the ones on screen fetch again, the rest
// when they next mount.
function refetchPaths(queryClient: QueryClient, ...paths: QueryPath[]) {
  for (const path of paths) {
    void queryClient.invalidateQueries({ queryKey: queryKeyFor(path), refetchType: "active" });
  }
}

// Mark every read whose path satisfies `matches` stale.
function refetchMatching(queryClient: QueryClient, matches: (path: string) => boolean) {
  void queryClient.invalidateQueries({
    queryKey: API_KEY_PREFIX,
    predicate: (query) => matches(String(query.queryKey[1] ?? "")),
    refetchType: "active",
  });
}

// `/api/v1/projects/<id>/setup`, whatever the project.
const PROJECT_SETUP_PATH = /^\/api\/v1\/projects\/[^/?]+\/setup$/;

// The list read of each entity the board writes in place from `entity_state`.
// The daemon sends no state when it could not read the entity (one bad row
// never stalls the stream), and then only a fetch can say what it is.
const ENTITY_LISTS: Record<string, QueryPath> = {
  attention: attentionPath,
  host: machinesPath,
  quota: quotaPath,
  project: projectsPath,
};

// What the Health signals count: open and answered items, ended runs, merged
// PRs, closed cards, interventions, breached guards and live hosts.
const SIGNAL_ENTITIES = new Set([
  "attention",
  "run",
  "pr",
  "card",
  "intervention",
  "guard",
  "host",
]);

// Entities whose API shape is a join of several tables: the event says
// something moved, and the quiet refetch fetches the joined answer.
const REFETCH_ENTITIES = new Set(["session", "run", "pane", "observation", "worktree"]);

// The events a recap counts, and the mark that resets it.
const RECAP_KINDS = new Set([
  "card.closed",
  "attention.opened",
  "run.ended",
  "proposal.created",
  "recap.seen",
]);

export function applyChange(queryClient: QueryClient, event: ChangeEvent): void {
  const state = event.entity_state;

  if (RECAP_KINDS.has(event.kind)) {
    void queryClient.invalidateQueries({ queryKey: queryKeyFor(recapPath), refetchType: "active" });
  }

  const gone = event.kind === "project.deleted" || event.kind === "project.retired";
  const list = ENTITY_LISTS[event.entity];
  if (!state && list !== undefined && !gone) refetchPaths(queryClient, list);

  if (SIGNAL_ENTITIES.has(event.entity)) refetchPaths(queryClient, signalsPath);
  if (event.entity === "pr" || event.entity === "card") {
    // The contributions list names each PR with its card's title.
    refetchMatching(queryClient, (path) => path.startsWith(CONTRIBUTIONS_PREFIX));
  }

  switch (event.entity) {
    case "config":
      // `tiller config set` in a terminal moves what Settings and the project
      // detail show, and no screen of the board sent it.
      refetchPaths(queryClient, startSettingsPath);
      refetchMatching(queryClient, (path) => PROJECT_SETUP_PATH.test(path));
      break;
    case "attention":
      if (state) writeAttention(queryClient, state as AttentionItem);
      break;
    case "host":
      if (state) writeMachine(queryClient, state as Machine);
      break;
    case "quota":
      if (state) writeQuota(queryClient, state as QuotaWindow);
      break;
    case "project":
      if (gone) {
        onProjectGone(queryClient, event.entity_id);
      } else if (state) writeProject(queryClient, state as Project);
      queryClient.invalidateQueries({
        queryKey: queryKeyFor(projectSetupPath(event.entity_id)),
        refetchType: "active",
      });
      break;
    case "review_pass":
    case "obligation":
      // A project row carries its open review pass and its next weekly due
      // date, which these entities move.
      queryClient.invalidateQueries({
        queryKey: queryKeyFor(projectsPath),
        refetchType: "active",
      });
      break;
    case "outbox":
      // Moves the board's receipts on to delivered and done (section 7.2).
      if (hasState(state)) recordProgress(event.entity_id, state.state, event.at);
      break;
    case "ask":
      // The Asked column lists the unresolved asks, which only the server can
      // say: an ask the orchestrator answers in chat leaves it with no card.
      queryClient.invalidateQueries({
        queryKey: API_KEY_PREFIX,
        predicate: (query) => String(query.queryKey[1] ?? "").startsWith("/api/v1/board"),
        refetchType: "active",
      });
      break;
    case "approval":
      if (hasState(state)) recordProgress(event.entity_id, state.state, event.at);
      // An attention item embeds its approval's state and head, so a stale or
      // re-bound approval must reach the inbox before the owner clicks.
      queryClient.invalidateQueries({
        queryKey: queryKeyFor(attentionPath),
        refetchType: "active",
      });
      // An away contract is an approval: starting, editing and ending it
      // moves the header's status and the brief.
      queryClient.invalidateQueries({
        queryKey: queryKeyFor(awayBriefPath),
        refetchType: "active",
      });
      break;
    case "proposal":
    case "memory":
    case "memory_revision":
    case "eval_run":
    case "change_set":
      // The memory view joins the file at `HEAD` with its rows, open
      // proposals and their eval results, so any of them moving refetches
      // every memory read.
      queryClient.invalidateQueries({
        queryKey: API_KEY_PREFIX,
        predicate: (query) => String(query.queryKey[1] ?? "").startsWith("/api/v1/memory"),
        refetchType: "active",
      });
      break;
    default:
      if (REFETCH_ENTITIES.has(event.entity)) {
        queryClient.invalidateQueries({
          queryKey: ["api"],
          predicate: (query) => {
            const path = String(query.queryKey[1] ?? "");
            return path.startsWith("/api/v1/agents") || path.startsWith("/api/v1/runs/");
          },
          refetchType: "active",
        });
      }
  }

  if (event.card) {
    writeBoard(queryClient, event.card);
  }
  if (event.card_id) {
    // The detail page and its timeline read the card by the name in the page
    // URL, which from the board is the display id. The event names the ULID,
    // so a name is also the card's when its cached detail holds that card.
    const names = new Set([event.card_id]);
    for (const [key, detail] of queryClient.getQueriesData<CardDetail>({
      queryKey: API_KEY_PREFIX,
    })) {
      const name = cardName(String(key[1] ?? ""));
      if (name && detail?.card?.id === event.card_id) names.add(name);
    }
    queryClient.invalidateQueries({
      queryKey: API_KEY_PREFIX,
      predicate: (query) => {
        const name = cardName(String(query.queryKey[1] ?? ""));
        return name !== null && names.has(name);
      },
      refetchType: "active",
    });
  }
}

// A peer's project is gone: only its row leaves, a project of the same id on
// this host or another stays. What it owned is read again.
function onPeerProjectGone(queryClient: QueryClient, host: string, projectId: string) {
  queryClient.setQueryData<ProjectsResponse | undefined>(queryKeyFor(projectsPath), (old) =>
    old
      ? {
          projects: old.projects.filter(
            (other) => other.id !== projectId || ownerOf(other) !== host,
          ),
        }
      : old,
  );
  void queryClient.invalidateQueries({ queryKey: API_KEY_PREFIX, refetchType: "active" });
  void queryClient.invalidateQueries({
    queryKey: queryKeyFor(projectSetupPath(projectId)),
    refetchType: "active",
  });
}

// `event: peer`. The daemon tagged the change as the peer's, so the entity
// writers key a row by its owner as well as its id.
function applyPeerChange(queryClient: QueryClient, event: PeerEvent): void {
  const { change } = event;
  if (
    change.entity === "project" &&
    (change.kind === "project.deleted" || change.kind === "project.retired")
  ) {
    onPeerProjectGone(queryClient, event.host, change.entity_id);
    return;
  }
  // Quota windows and outbox receipts are not tagged by owner: a peer's would
  // replace this host's window of the same label, or a receipt of this board.
  if (change.entity === "quota" || change.entity === "outbox") return;
  if (change.entity === "host") {
    // The daemon sends the peer's Machines row with no state: read it again.
    void queryClient.invalidateQueries({
      queryKey: queryKeyFor(machinesPath),
      refetchType: "active",
    });
    return;
  }
  applyChange(queryClient, change);
}

function ofHost<T extends { host?: string; link?: Machine["link"] }>(host: string) {
  return (row: T) => isPeerRow(row) && row.host === host;
}

// This host's rows and other peers' stay; the peer's rows become `incoming`.
function replaceHost<T extends { host?: string; link?: Machine["link"] }>(
  rows: T[],
  host: string,
  incoming: unknown,
): T[] {
  // A path the peer did not send leaves its rows as they are.
  if (!Array.isArray(incoming)) return rows;
  const next = incoming as T[];
  return [...rows.filter((row) => !ofHost<T>(host)(row)), ...next];
}

// `event: peer_reset`: the peer's rows replace what the board holds for it.
function applyPeerReset(queryClient: QueryClient, event: PeerResetEvent): void {
  const { host, queries } = event;
  const at = (path: string) => (queries?.[path] ?? {}) as Record<string, unknown>;

  queryClient.setQueryData<BoardResponse | undefined>(
    queryKeyFor("/api/v1/board?filter=all"),
    (old) => {
      if (!old) return old;
      return {
        ...old,
        rows: replaceHost(old.rows, host, at("/api/v1/board?filter=all").rows),
      };
    },
  );
  queryClient.setQueryData<AttentionResponse | undefined>(attentionKey, (old) =>
    old
      ? { ...old, items: replaceHost(old.items, host, at("/api/v1/attention?state=open").items) }
      : old,
  );
  queryClient.setQueryData<AgentsResponse | undefined>(queryKeyFor(agentsPath()), (old) =>
    old
      ? {
          ...old,
          orchestrators: replaceHost(old.orchestrators, host, at("/api/v1/agents").orchestrators),
        }
      : old,
  );
  queryClient.setQueryData<ProjectsResponse | undefined>(queryKeyFor(projectsPath), (old) =>
    old ? { projects: replaceHost(old.projects, host, at("/api/v1/projects").projects) } : old,
  );
  // A filtered board list may need a card to enter or leave.
  queryClient.invalidateQueries({
    queryKey: API_KEY_PREFIX,
    predicate: (query) => {
      const path = String(query.queryKey[1] ?? "");
      return path.startsWith("/api/v1/board") && path !== "/api/v1/board?filter=all";
    },
    refetchType: "active",
  });
  applyLink(queryClient, {
    host,
    link: event.link,
    as_of: event.as_of,
    stale_reason: event.link === "live" ? null : undefined,
  });
}

// `event: link`: the Machines row of that peer moves, and with it the standing
// of every row it owns (`standingOf`).
function applyLink(
  queryClient: QueryClient,
  event: Omit<LinkEvent, "stale_reason"> & { stale_reason?: LinkEvent["stale_reason"] },
): void {
  queryClient.setQueryData<MachinesResponse | undefined>(queryKeyFor(machinesPath), (old) =>
    old
      ? {
          machines: old.machines.map((machine) =>
            machine.label === event.host && machine.link !== undefined && machine.link !== null
              ? {
                  ...machine,
                  link: event.link,
                  last_seen_at: event.as_of ?? machine.last_seen_at,
                  stale_reason:
                    event.stale_reason === undefined ? machine.stale_reason : event.stale_reason,
                  link_reason: event.link === "live" ? null : machine.link_reason,
                }
              : machine,
          ),
        }
      : old,
  );
}

// The card a card read names: `<name>` in `/api/v1/cards/<name>`, with or
// without a sub-path or query.
function cardName(path: string): string | null {
  return /^\/api\/v1\/cards\/([^/?]+)/.exec(path)?.[1] ?? null;
}

export interface EventSourceLike {
  readyState: number;
  addEventListener(type: "open" | "error", listener: () => void): void;
  addEventListener(type: string, listener: (event: MessageEvent<string>) => void): void;
  close(): void;
}

export interface EventsOptions {
  queryClient: QueryClient;
  // The boot snapshot's seq, or null when the page booted without one.
  lastEventSeq: number | null;
  createEventSource?: (url: string) => EventSourceLike;
  fetchSnapshot?: () => Promise<Snapshot>;
  // Asked when the stream is refused or cut: a `signed_out` answer means this
  // browser's cookie is gone, so the board gives way to the signed-out screen.
  fetchSession?: () => Promise<unknown>;
}

const CLOSED = 2;

// Connects the stream and writes each event into the Query cache. Returns a
// function that stops it. The browser reconnects by itself and sends
// `Last-Event-ID`; a stream it gave up on reopens after the last applied
// seq. Either way a short gap replays and a long one arrives as `reset`.
// Without a boot snapshot the stream starts live and the snapshot is fetched
// once it opens, so nothing is replayed from the start of the log.
export function connectEvents(options: EventsOptions): () => void {
  const { queryClient } = options;
  const createEventSource =
    options.createEventSource ?? ((url) => new EventSource(url) as EventSourceLike);
  const fetchSnapshot = options.fetchSnapshot ?? (() => fetchJson<Snapshot>(SNAPSHOT_PATH));
  const fetchSession = options.fetchSession ?? (() => fetchJson(sessionPath));

  // The resume cursor: the newest seq this page holds a consistent view of.
  // It moves only with an applied change while in sync, and with a snapshot
  // that landed. While a resync is pending it stays where it was, so a stream
  // that drops now resumes after the old cursor and the daemon sends `reset`
  // again, which is a second reason to try.
  let lastSeq = options.lastEventSeq;
  // Set from a `reset` (or a start with no snapshot) until a snapshot lands.
  let pending = lastSeq === null;
  // While pending: changes at or below `floor` are old. It follows the daemon's
  // newest seq from the `reset` on, so a change that arrives before the new
  // snapshot lands is applied and not dropped as old, even when a restored
  // database put the seq below the cursor. Null accepts any change.
  let floor: number | null = null;
  let source: EventSourceLike | null = null;
  let stopped = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let resyncTimer: ReturnType<typeof setTimeout> | undefined;
  let resyncing = false;
  // A `reset` came while a snapshot was in flight: that snapshot may be older
  // than the reset, so it is fetched again.
  let again = false;
  let failures = 0;

  // Refresh every query from a new snapshot while the UI stays put: only the
  // degraded pill shows, never a spinner or an empty state. A snapshot that
  // fails is tried again with a growing wait, and when the stream opens.
  async function resync() {
    clearTimeout(resyncTimer);
    if (stopped) return;
    if (resyncing) {
      again = true;
      return;
    }
    resyncing = true;
    again = false;
    try {
      const snapshot = await fetchSnapshot();
      if (stopped) return;
      // Exact, not max: after a restored database the newest seq goes down.
      // Changes applied meanwhile can be newer than the snapshot, and those
      // are not replayed.
      lastSeq = Math.max(refreshFromSnapshot(queryClient, snapshot), floor ?? -1);
      pending = false;
      floor = null;
      failures = 0;
    } catch {
      if (!stopped) {
        failures += 1;
        resyncTimer = setTimeout(() => void resync(), Math.min(1000 * 2 ** (failures - 1), 30_000));
      }
    } finally {
      resyncing = false;
    }
    if (again && !stopped) void resync();
  }

  function open() {
    if (stopped) return;
    // `since` resumes after the last applied event. The browser's own
    // reconnects send `Last-Event-ID`, which the daemon prefers, so a stale
    // `since` in the URL never replays twice.
    const next = createEventSource(
      lastSeq === null ? EVENTS_PATH : `${EVENTS_PATH}?since=${lastSeq}`,
    );
    source = next;

    next.addEventListener("open", () => {
      connectionStore.set("live");
      if (pending) void resync();
    });
    next.addEventListener("error", () => {
      connectionStore.set("reconnecting");
      if (next.readyState === CLOSED && !stopped) {
        next.close();
        retryTimer = setTimeout(open, 1000);
        fetchSession().catch((error: unknown) => {
          if (stopped || !isSignedOut(error)) return;
          stop();
          signedOutStore.set(true);
        });
      }
    });
    next.addEventListener("change", (message) => {
      const event = JSON.parse(message.data) as ChangeEvent;
      const seen = pending ? floor : lastSeq;
      if (seen !== null && event.seq <= seen) return;
      if (pending) floor = event.seq;
      else lastSeq = event.seq;
      applyChange(queryClient, event);
    });
    // A peer's messages carry no `id`: its `seq` is not this host's, so they
    // never touch `lastSeq` and are not replayed. A reconnect sends `peer_reset`.
    next.addEventListener("peer", (message) => {
      applyPeerChange(queryClient, JSON.parse(message.data) as PeerEvent);
    });
    next.addEventListener("peer_reset", (message) => {
      applyPeerReset(queryClient, JSON.parse(message.data) as PeerResetEvent);
    });
    next.addEventListener("link", (message) => {
      applyLink(queryClient, JSON.parse(message.data) as LinkEvent);
    });
    next.addEventListener("reset", (message) => {
      pending = true;
      floor = (JSON.parse(message.data) as { seq: number }).seq;
      void resync();
    });
  }

  function stop() {
    stopped = true;
    clearTimeout(retryTimer);
    clearTimeout(resyncTimer);
    source?.close();
  }

  open();

  return stop;
}
