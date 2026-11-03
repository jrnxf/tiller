// The `federated` fixture scenario: this host is the board host and four
// peers are paired to it (protocol.md sections 9 and 10.5). Names are
// placeholders, as everywhere in the fixtures.
//
//   host-d  live, `act`      its `gamma-12` shares a display id with this host's
//   host-e  down, `act`      rows are dimmed and say "as of" the last frame
//   host-f  live, `read`     rows show live and the controls stay off
//   host-g  refused          the pairing's key was revoked; no rows
import type {
  AgentRow,
  AttentionItem,
  AttentionResponse,
  BoardRow,
  LinkState,
  Machine,
  PeerScope,
  Project,
  SnapshotPeer,
} from "./api-types.ts";

interface PeerSpec {
  host: string;
  host_id: string;
  link: LinkState;
  scope: PeerScope;
  // Minutes before now of the last frame from the peer; null when none came.
  frameMinutes: number | null;
  refusal?: string;
}

const PEER_SPECS: readonly PeerSpec[] = [
  {
    host: "host-d",
    host_id: "01JPEERD0000000000000000D",
    link: "live",
    scope: "act",
    frameMinutes: 0.1,
  },
  {
    host: "host-e",
    host_id: "01JPEERE0000000000000000E",
    link: "down",
    scope: "act",
    frameMinutes: 6,
  },
  {
    host: "host-f",
    host_id: "01JPEERF0000000000000000F",
    link: "live",
    scope: "read",
    frameMinutes: 0.2,
  },
  {
    host: "host-g",
    host_id: "01JPEERG0000000000000000G",
    link: "refused",
    scope: "act",
    frameMinutes: null,
    refusal: "revoked",
  },
];

function at(now: number, minutesAgo: number): string {
  return new Date(now - minutesAgo * 60_000).toISOString();
}

function spec(host: string): PeerSpec {
  const found = PEER_SPECS.find((peer) => peer.host === host);
  if (!found) throw new Error(`no fixture peer ${host}`);
  return found;
}

// The federated fields a peer's row carries.
function tag(host: string, now: number) {
  const peer = spec(host);
  return {
    host: peer.host,
    as_of: peer.frameMinutes === null ? null : at(now, peer.frameMinutes),
    link: peer.link,
  };
}

export function peerMachines(now: number): Machine[] {
  return PEER_SPECS.map((peer, index) => {
    const quiet = peer.link !== "live";
    return {
      id: peer.host_id,
      label: peer.host,
      kind: "remote",
      view_only: peer.scope === "read",
      health: quiet ? "stale" : "ok",
      running_agents: quiet ? null : 2,
      capacity: null,
      free_slots: null,
      last_seen_at: peer.frameMinutes === null ? null : at(now, peer.frameMinutes),
      stale_reason:
        peer.link === "down" ? "peer_link_down" : peer.link === "refused" ? "peer_refused" : null,
      link: peer.link,
      peer_scope: peer.scope,
      link_reason: peer.refusal ?? null,
      rev: 3 + index,
    };
  });
}

export function peerSnapshot(now: number): SnapshotPeer[] {
  return PEER_SPECS.map((peer) => ({
    host: peer.host,
    host_id: peer.host_id,
    link: peer.link,
    as_of: peer.frameMinutes === null ? null : at(now, peer.frameMinutes),
    server_time_offset_ms: 0,
    scope: peer.scope,
    last_event_seq: peer.frameMinutes === null ? null : 812,
  }));
}

function peerProject(host: string, id: string, delivery: string, now: number): Project {
  return {
    id,
    name: id,
    delivery,
    capacity: 2,
    rev: 1,
    stage: "launched",
    effective_delivery: delivery,
    reviewed_through_sha: null,
    reviewed_at: null,
    stage_changed_at: null,
    next_review_due: null,
    review_overdue: false,
    review_open: null,
    ...tag(host, now),
  };
}

// `gamma` exists on this host too: a project slug is unique only per host.
export function peerProjects(now: number): Project[] {
  return [
    peerProject("host-d", "gamma", "direct-pr", now),
    peerProject("host-d", "fleet", "direct-pr", now),
    peerProject("host-e", "ledger", "direct-push", now),
    peerProject("host-f", "atlas", "direct-pr", now),
  ];
}

function peerCard(
  host: string,
  n: number,
  project: string,
  column: BoardRow["column"],
  badge: string,
  title: string,
  extras: { kind?: string; model?: string; pr?: number; observedMinutes?: number } = {},
  now: number = Date.now(),
): BoardRow {
  return {
    id: `01JPCARD${host.slice(-1).toUpperCase()}${String(n).padStart(17, "0")}`,
    display_id: `${project}-${n}`,
    project_id: project,
    host_id: spec(host).host_id,
    kind: extras.kind ?? "ship",
    title,
    priority: 2,
    column,
    badge,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: column === "done" ? "2026-11-01T18:20:00.000Z" : null,
    rev: 2,
    model: extras.model ?? null,
    pr_number: extras.pr ?? null,
    observed_at: extras.observedMinutes === undefined ? null : at(now, extras.observedMinutes),
    ...tag(host, now),
  };
}

export function peerRows(now: number): BoardRow[] {
  return [
    peerCard(
      "host-d",
      12,
      "gamma",
      "needs_you",
      "decision",
      "Rotate the signing key",
      {
        model: "opus",
        observedMinutes: 0.5,
      },
      now,
    ),
    peerCard(
      "host-d",
      14,
      "gamma",
      "working",
      "working",
      "Cache the release manifest",
      {
        model: "sonnet",
        observedMinutes: 0.1,
      },
      now,
    ),
    peerCard(
      "host-d",
      3,
      "fleet",
      "landing",
      "checks",
      "Batch the heartbeat writes",
      {
        model: "opus",
        pr: 31,
        observedMinutes: 0.8,
      },
      now,
    ),
    peerCard(
      "host-d",
      4,
      "fleet",
      "queued",
      "filed",
      "Document the pairing flow",
      {
        kind: "scout",
      },
      now,
    ),
    peerCard(
      "host-e",
      5,
      "ledger",
      "working",
      "working",
      "Reconcile the monthly totals",
      {
        model: "sonnet",
        observedMinutes: 7,
      },
      now,
    ),
    peerCard(
      "host-e",
      6,
      "ledger",
      "needs_you",
      "decision",
      "Pick the rounding rule",
      {
        model: "opus",
        observedMinutes: 8,
      },
      now,
    ),
    peerCard("host-e", 7, "ledger", "queued", "filed", "Export the audit trail", {}, now),
    peerCard(
      "host-f",
      2,
      "atlas",
      "working",
      "working",
      "Index the tile cache",
      {
        model: "sonnet",
        observedMinutes: 0.3,
      },
      now,
    ),
    peerCard(
      "host-f",
      3,
      "atlas",
      "needs_you",
      "decision",
      "Choose the tile size",
      {
        model: "opus",
        observedMinutes: 1,
      },
      now,
    ),
    peerCard("host-f", 4, "atlas", "done", "landed", "Pin the map library", { pr: 18 }, now),
  ];
}

function rowId(rows: BoardRow[], host: string, displayId: string): string {
  return rows.find((row) => row.host === host && row.display_id === displayId)?.id ?? displayId;
}

function decision(
  rows: BoardRow[],
  host: string,
  displayId: string,
  id: string,
  question: string,
  now: number,
  more: Partial<AttentionItem> = {},
): AttentionItem {
  return {
    id,
    card_id: rowId(rows, host, displayId),
    kind: "decision",
    question,
    evidence: null,
    recommendation: null,
    consequence: null,
    options: [
      { label: "Go ahead", consequence: null, recommended: true },
      { label: "Hold off", consequence: null, recommended: false },
    ],
    approval: null,
    opened_by_session_id: null,
    opened_at: at(now, 20),
    still_current: true,
    answered_at: null,
    withdrawn_at: null,
    rev: 1,
    ...tag(host, now),
    ...more,
  };
}

export function peerAttention(now: number, rows: BoardRow[]): AttentionResponse {
  return {
    items: [
      decision(
        rows,
        "host-d",
        "gamma-12",
        "01JPATTN00000000000000000D",
        "Rotate the signing key now or after the release?",
        now,
      ),
      decision(
        rows,
        "host-d",
        "fleet-3",
        "01JPATTN00000000000000001D",
        "Merge PR #31 at its current head?",
        now,
        {
          kind: "merge",
          options: null,
          recommendation: "Approve. Every check passed.",
          approval: {
            id: "01JPAPPR00000000000000000D",
            kind: "merge",
            head_sha: "3e8a1c5d7b9f204a6c8e1b3d5f7a9c2e4b6d8f01",
            pr_number: 31,
            state: "requested",
          },
        },
      ),
      decision(
        rows,
        "host-e",
        "ledger-6",
        "01JPATTN00000000000000000E",
        "Round half up or to even?",
        now,
      ),
      decision(
        rows,
        "host-f",
        "atlas-3",
        "01JPATTN00000000000000000F",
        "Which tile size should the index use?",
        now,
      ),
    ],
    permission_prompts: [
      {
        session_id: "01JSESSPEERD000000000000G",
        card_id: rowId(rows, "host-d", "gamma-14"),
        tool: "Bash",
        detail: "cargo build -p release-manifest",
        since: at(now, 3),
        ...tag("host-d", now),
      },
    ],
  };
}

function peerSession(key: string): string {
  return `01JSESS${key.replace(/\W/g, "").padEnd(19, "0").slice(0, 19).toUpperCase()}`;
}

// One orchestrator per peer project, with a worker for each card in flight.
export function peerAgents(now: number, rows: BoardRow[], projects: Project[]): AgentRow[] {
  return projects.map((project, index) => {
    const host = project.host ?? "";
    const live = rows.filter(
      (row) =>
        row.host === host &&
        row.project_id === project.id &&
        (row.column === "working" || row.column === "landing" || row.column === "needs_you"),
    );
    const base = {
      host_id: spec(host).host_id,
      ...tag(host, now),
    };
    return {
      session_id: peerSession(`PO${host}${project.id}`),
      role: "orchestrator",
      model: "opus",
      activity: "working",
      activity_observed_at: at(now, spec(host).frameMinutes ?? 1),
      last_status_line: `Waiting on ${live[0]?.display_id ?? "the next ask"}`,
      herdr_name: `${project.id}-orch`,
      card_id: null,
      workers: live.map((row, workerIndex) => ({
        session_id: peerSession(`PW${host}${row.display_id}`),
        role: row.kind === "scout" ? "scout" : "build",
        model: row.model ?? "sonnet",
        activity: row.column === "needs_you" ? "waiting_permission" : "working",
        activity_observed_at: at(now, (spec(host).frameMinutes ?? 1) + workerIndex / 10),
        last_status_line: `Running tests for ${row.display_id}`,
        herdr_name: `${row.display_id}-build`,
        card_id: row.id,
        workers: [],
        project_id: row.project_id,
        run_id: `01JPRUN${row.id.slice(-18)}1`,
        pane_id: `pane-${row.display_id}`,
        workspace_label: `${row.display_id} build`,
        state_since: at(now, 8 + workerIndex * 3),
        ...base,
      })),
      project_id: project.id,
      run_id: `01JPRUN${peerSession(`O${host}${project.id}`).slice(-18)}1`,
      pane_id: `pane-${project.id}`,
      workspace_label: project.id,
      state_since: at(now, 40 + index * 9),
      ...base,
    };
  });
}
