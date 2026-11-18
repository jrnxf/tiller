import type {
  AskDelivery,
  AttentionItem,
  ReplyItem,
  ThreadContext,
  ThreadItem,
  ThreadResponse,
  ThreadStep,
} from "./api-types.ts";
import { projectReplies } from "./fixtures-replies.ts";
import { FIXTURE_HEAD_SHA, type Scenario } from "./fixtures.ts";

const MINUTE = 60_000;

const at = (now: number, minutesAgo: number) => new Date(now - minutesAgo * MINUTE).toISOString();

const cardId = (n: number) => `01JCARD${String(n).padStart(19, "0")}`;

const card = (n: number, display: string, title: string) => ({
  id: cardId(n),
  display_id: display,
  title,
});

// The orchestrator sessions of the threads. A checkpoint after the first is a
// restart.
const SESSION_OLD = "01JSESSTHREADOLD0000000000";
const SESSION = "01JSESSTHREAD0000000000000";

// A note that went to the orchestrator and how far it got.
function delivery(
  now: number,
  sentAgo: number,
  state: AskDelivery["state"],
  failure?: string,
): AskDelivery {
  const sent = at(now, sentAgo);
  const later = at(now, sentAgo - 0.1);
  return {
    state,
    sent_at: sent,
    delivered_at: state === "delivered" || state === "acked" ? later : null,
    seen_at: state === "acked" ? at(now, sentAgo - 0.5) : null,
    failed_at: state === "failed" ? later : null,
    failure_reason: failure ?? null,
  };
}

let stepSeq = 9000;

function step(
  now: number,
  minutesAgo: number,
  kind: string,
  label: string,
  extra: Partial<ThreadStep> = {},
): ThreadStep {
  stepSeq += 1;
  return {
    seq: stepSeq,
    at: at(now, minutesAgo),
    kind,
    label,
    system: false,
    payload: {},
    card: null,
    pr: null,
    ...extra,
  };
}

// One activity of the orchestrator from its first step to its last.
function activity(now: number, session: string, steps: ThreadStep[]): ThreadItem {
  const first = steps[0];
  const last = steps.at(-1);
  const start = first ? Date.parse(first.at) : now;
  const end = last ? Date.parse(last.at) : now;
  return {
    type: "activity",
    seq: first?.seq ?? 0,
    at: first?.at ?? new Date(now).toISOString(),
    session_id: session,
    ended_at: last?.at ?? new Date(now).toISOString(),
    duration_seconds: Math.round((end - start) / 1000),
    steps,
  };
}

const replyItem = (reply: ReplyItem): ThreadItem => ({
  type: "reply",
  seq: 0,
  at: reply.replied_at,
  reply,
});

// What the owner decided in fixture mode since the server started, by the
// decision's id: the chat shows it resolved on its next read.
const decided = new Map<string, { choice: string; at: string }>();

// The asks the owner sent from a chat in fixture mode, by `host/project`.
const sent = new Map<string, { id: string; text: string; at: number; live: boolean }[]>();

const threadKey = (projectId: string, host: string | undefined) => `${host ?? ""}/${projectId}`;

// Forget what the owner sent and decided, so each test starts the same.
export function resetThreadFixture(): void {
  decided.clear();
  sent.clear();
}

export function recordThreadDecision(id: string, choice: string): void {
  decided.set(id, { choice, at: new Date().toISOString() });
}

export function recordThreadAsk(
  projectId: string,
  host: string | undefined,
  ask: { id: string; text: string; live: boolean },
): void {
  const key = threadKey(projectId, host);
  sent.set(key, [...(sent.get(key) ?? []), { ...ask, at: Date.now() }]);
}

// A sent ask moves on as the stand-in outbox does: delivered after 1.2 s and
// read after 3.2 s (fixture-mutations.ts outboxStages).
function sentItems(projectId: string, host: string | undefined, now: number): ThreadItem[] {
  return (sent.get(threadKey(projectId, host)) ?? []).map((ask) => {
    const age = now - ask.at;
    const state = age < 1200 ? "pending" : age < 3200 ? "delivered" : "acked";
    const sentAt = new Date(ask.at).toISOString();
    return {
      type: "ask",
      seq: 0,
      at: sentAt,
      id: ask.id,
      text: ask.text,
      source: "board",
      disposition: "unresolved",
      cards: [],
      delivery: ask.live
        ? {
            state,
            sent_at: sentAt,
            delivered_at: state === "pending" ? null : new Date(ask.at + 1200).toISOString(),
            seen_at: state === "acked" ? new Date(ask.at + 3200).toISOString() : null,
            failed_at: null,
            failure_reason: null,
          }
        : null,
    };
  });
}

// An open decision as the owner left it: still open, or resolved with their
// choice.
function settle(item: ThreadItem): ThreadItem {
  if (item.type !== "decision" || !item.open) return item;
  const choice = decided.get(item.id);
  if (!choice) return item;
  if (item.source === "approval") {
    const accepted = choice.choice === "accept";
    return {
      ...item,
      open: false,
      state: accepted ? "succeeded" : "rejected",
      choice: accepted ? "accepted" : "rejected",
      decided_at: choice.at,
      decided_by: "owner",
    };
  }
  return {
    ...item,
    open: false,
    state: "answered",
    choice: choice.choice,
    decided_at: choice.at,
    decided_by: "owner",
    attention: item.attention ? { ...item.attention, answered_at: choice.at } : item.attention,
  };
}

function alphaThread(now: number, replies: ReplyItem[], attention: AttentionItem[]): ThreadItem[] {
  const retry = card(41, "alpha-41", "Pick the retry policy for the export job");
  const merge = card(44, "alpha-44", "Retry failed exports with backoff");
  const login = card(39, "alpha-39", "Fix the flaky login test");
  const decision = attention.find((item) => item.id === "01JATTN0000000000000000000");
  const reply = replies.find((one) => one.id === "01JREPLY000000000000000001");
  return [
    { type: "checkpoint", seq: 0, at: at(now, 26 * 60), session_id: SESSION_OLD, restarted: false },
    {
      type: "ask",
      seq: 0,
      at: at(now, 25 * 60),
      id: "01JASK000000000000000000B1",
      text: "Can you pick up the flaky login test before the release?",
      source: "board",
      disposition: "new",
      cards: [{ ...login, relation: "created" }],
      delivery: delivery(now, 25 * 60, "acked"),
    },
    activity(now, SESSION_OLD, [
      step(now, 25 * 60 - 1, "card.created", "created the card", { card: login }),
      step(now, 25 * 60 - 1.2, "run.started", "started a builder run", {
        card: login,
        payload: { role: "builder", model: "sonnet" },
      }),
      step(now, 25 * 60 - 1.3, "run.placed", "placed the run on a machine", {
        card: login,
        system: true,
        payload: { host: "host-a" },
      }),
      step(now, 24 * 60, "pr.linked", "linked a pull request", {
        card: login,
        pr: {
          id: "01JPR0000000000000000049",
          number: 49,
          url: "https://example.com/alpha/pull/49",
        },
      }),
    ]),
    {
      type: "decision",
      seq: 0,
      at: at(now, 23 * 60),
      source: "approval",
      id: "01JAPPR00000000000000000B1",
      kind: "merge",
      state: "succeeded",
      open: false,
      attention: null,
      card: login,
      choice: "accepted",
      decided_at: at(now, 23 * 60 - 4),
      decided_by: "owner",
      head_sha: "4b1e9c27a0d35f8e6c2b7a9d1f4e8c3b5a7d9e10",
      pr: { id: "01JPR0000000000000000049", number: 49, url: "https://example.com/alpha/pull/49" },
      question: "Merge PR #49 at its current head?",
    },
    // The note that found the old session gone: the restart came after it.
    {
      type: "ask",
      seq: 0,
      at: at(now, 62),
      id: "01JASK000000000000000000F1",
      text: "Is the nightly export still on schedule?",
      source: "board",
      disposition: "unresolved",
      cards: [],
      delivery: delivery(now, 62, "failed", "the orchestrator's pane was gone"),
    },
    { type: "checkpoint", seq: 0, at: at(now, 60), session_id: SESSION, restarted: true },
    {
      type: "ask",
      seq: 0,
      at: at(now, 34),
      id: "01JASK0000000000000000001",
      text: "Can the export job retry on its own when storage times out?",
      source: "board",
      disposition: "new",
      cards: [{ ...retry, relation: "created" }],
      delivery: delivery(now, 34, "acked"),
    },
    activity(now, SESSION, [
      step(now, 33, "card.created", "created the card", { card: retry }),
      step(now, 32.5, "card.edited", "edited the card", {
        card: retry,
        payload: { fields: ["description", "priority"] },
      }),
      step(now, 32, "attention.notify_sent", "notified you about an item", {
        card: retry,
        system: true,
        payload: { channel: "push" },
      }),
    ]),
    ...(decision
      ? [
          {
            type: "decision" as const,
            seq: 0,
            at: at(now, 31),
            source: "attention" as const,
            id: decision.id,
            kind: decision.kind,
            state: "open",
            open: true,
            attention: decision,
            card: retry,
            choice: null,
            decided_at: null,
            decided_by: null,
            head_sha: null,
            pr: null,
            question: decision.question,
          },
        ]
      : []),
    ...(reply ? [replyItem(reply)] : []),
    {
      type: "ask",
      seq: 0,
      at: at(now, 9),
      id: "01JASK000000000000000000B2",
      text: "Merge alpha-44 once its checks are green.",
      source: "board",
      disposition: "followup",
      cards: [{ ...merge, relation: null }],
      delivery: delivery(now, 9, "delivered"),
    },
    // A second note straight after: the run shows one time, under it.
    {
      type: "ask",
      seq: 0,
      at: at(now, 8.8),
      id: "01JASK000000000000000000B3",
      text: "And tell me when it lands.",
      source: "board",
      disposition: "followup",
      cards: [],
      delivery: delivery(now, 8.8, "delivered"),
    },
    activity(now, SESSION, [
      step(now, 8.5, "run.status", "reported checks failed", {
        card: merge,
        payload: { state: "checks-failed", failed: ["lint"] },
      }),
      step(now, 8, "run.ended", "ended the run as failed", {
        card: merge,
        payload: { outcome: "failed", exit: 1 },
      }),
      step(now, 7, "run.relaunched", "relaunched the run", { card: merge }),
      step(now, 6.2, "review.recorded", "recorded a review", {
        card: merge,
        pr: {
          id: "01JPR0000000000000000052",
          number: 52,
          url: "https://example.com/alpha/pull/52",
        },
        payload: { verdict: "approve", blocking: 0 },
      }),
    ]),
    {
      type: "decision",
      seq: 0,
      at: at(now, 6),
      source: "approval",
      id: "01JAPPR0000000000000000000",
      kind: "merge",
      state: "requested",
      open: true,
      attention: null,
      card: merge,
      choice: null,
      decided_at: null,
      decided_by: null,
      head_sha: FIXTURE_HEAD_SHA,
      pr: { id: "01JPR0000000000000000052", number: 52, url: "https://example.com/alpha/pull/52" },
      question: "Merge PR #52 at its current head?",
    },
  ];
}

function betaThread(now: number, replies: ReplyItem[]): ThreadItem[] {
  const cutover = card(8, "beta-8", "Document the cutover steps");
  const importer = card(7, "beta-7", "Rewrite the importer's dry run");
  const reply = replies.find((one) => one.id === "01JREPLY000000000000000002");
  return [
    { type: "checkpoint", seq: 0, at: at(now, 4 * 60), session_id: SESSION, restarted: false },
    {
      type: "ask",
      seq: 0,
      at: at(now, 3 * 60 + 20),
      id: "01JASK000000000000000000C1",
      text: "Pause the importer until the cutover doc is done.",
      source: "board",
      disposition: "unresolved",
      cards: [],
      delivery: delivery(
        now,
        3 * 60 + 20,
        "failed",
        "the orchestrator's pane closed before the note arrived",
      ),
    },
    {
      type: "decision",
      seq: 0,
      at: at(now, 3 * 60 + 5),
      source: "attention",
      id: "01JATTN00000000000000000C1",
      kind: "decision",
      state: "withdrawn",
      open: false,
      attention: null,
      card: importer,
      choice: null,
      decided_at: null,
      decided_by: null,
      head_sha: null,
      pr: null,
      question: "Run the importer dry run against last night's snapshot?",
    },
    {
      type: "ask",
      seq: 0,
      at: at(now, 3 * 60),
      id: "01JASK000000000000000000C2",
      text: "Does the cutover doc cover how to roll back?",
      source: "board",
      disposition: "followup",
      cards: [{ ...cutover, relation: null }],
      delivery: delivery(now, 3 * 60, "acked"),
    },
    activity(now, SESSION, [
      step(now, 2 * 60 + 40, "card.edited", "edited the card", {
        card: cutover,
        payload: { fields: ["description"] },
      }),
    ]),
    {
      type: "decision",
      seq: 0,
      at: at(now, 2 * 60 + 30),
      source: "attention",
      id: "01JATTN00000000000000000C2",
      kind: "decision",
      state: "answered",
      open: false,
      attention: null,
      card: cutover,
      choice: "Give the scout the doc as well",
      decided_at: at(now, 2 * 60 + 20),
      decided_by: "owner",
      head_sha: null,
      pr: null,
      question: "Should the scout write the rollback step, or the builder?",
    },
    ...(reply ? [replyItem(reply)] : []),
  ];
}

function gammaThread(now: number, replies: ReplyItem[], attention: AttentionItem[]): ThreadItem[] {
  const trim = card(13, "gamma-13", "Trim the build cache");
  const decision = attention.find((item) => item.id === "01JATTN0000000000000000003");
  const reply = replies.find((one) => one.id === "01JREPLY000000000000000003");
  return [
    { type: "checkpoint", seq: 0, at: at(now, 27 * 60), session_id: SESSION, restarted: false },
    {
      type: "ask",
      seq: 0,
      at: at(now, 26 * 60),
      id: "01JASK0000000000000000003",
      text: "How much disk does the cache use now?",
      source: "prompt",
      disposition: "chat",
      cards: [],
      delivery: null,
    },
    ...(reply ? [replyItem(reply)] : []),
    ...(decision
      ? [
          {
            type: "decision" as const,
            seq: 0,
            at: at(now, 20),
            source: "attention" as const,
            id: decision.id,
            kind: decision.kind,
            state: "open",
            open: true,
            attention: decision,
            card: trim,
            choice: null,
            decided_at: null,
            decided_by: null,
            head_sha: null,
            pr: null,
            question: decision.question,
          },
        ]
      : []),
    {
      type: "ask",
      seq: 0,
      at: at(now, 0.1),
      id: "01JASK000000000000000000D1",
      text: "Start the trim on host-a if it has room.",
      source: "board",
      disposition: "unresolved",
      cards: [],
      delivery: { ...delivery(now, 0.1, "pending") },
    },
  ];
}

function peerThread(now: number, replies: ReplyItem[]): ThreadItem[] {
  const reply = replies[0];
  return [
    {
      type: "ask",
      seq: 0,
      at: reply?.asked_at ?? at(now, 3),
      id: reply?.ask_id ?? "01JPASKD00000000000000000D",
      text: reply?.question ?? "",
      source: "board",
      disposition: "chat",
      cards: [],
      delivery: delivery(now, 3, "acked"),
    },
    ...(reply ? [replyItem(reply)] : []),
  ];
}

const CONTEXT: Record<string, ThreadContext> = {
  alpha: { used: 62_000, window: 200_000 },
  beta: { used: 164_000, window: 200_000 },
  gamma: { used: 31_000, window: null },
  fleet: { used: 18_000, window: 200_000 },
};

function hasThreads(scenario: Scenario): boolean {
  return scenario !== "empty" && scenario !== "new-install";
}

// `GET /api/v1/projects/{id}/thread`, on this host or on the peer `host`.
// `attention` is the open queue, so a decision carries the item the Inbox
// answers. `undefined` for a project the scenario does not know (a 404).
// The daemon's page size (`views::THREAD_PAGE`).
const THREAD_PAGE = 50;

export function threadFor(
  scenario: Scenario,
  projectId: string,
  host: string | undefined,
  known: boolean,
  attention: AttentionItem[],
  now: number,
  page: { before?: number; limit?: number } = {},
): ThreadResponse | undefined {
  if (!known) return undefined;
  if (!hasThreads(scenario)) return { items: [], context: null, next_before: null };
  const replies = projectReplies(projectId, host, now);
  const local = attention.filter((item) => item.host === undefined);
  const base =
    host === "host-d"
      ? peerThread(now, replies)
      : host
        ? []
        : projectId === "alpha"
          ? alphaThread(now, replies, local)
          : projectId === "beta"
            ? betaThread(now, replies)
            : projectId === "gamma"
              ? gammaThread(now, replies, local)
              : [];
  const settled = [...base, ...sentItems(projectId, host, now)].map(settle);
  settled.sort((a, b) => a.at.localeCompare(b.at));
  const all = settled.map((item, index) => ({ ...item, seq: 4000 + index }));
  // A page as the daemon cuts it: the newest `limit` items older than
  // `before`, and the seq to ask for the page before them.
  const { before } = page;
  const older = before === undefined ? all : all.filter((item) => item.seq < before);
  const items = older.slice(-(page.limit ?? THREAD_PAGE));
  const more = older.length > items.length;
  return {
    items,
    context: all.length > 0 ? (CONTEXT[projectId] ?? null) : null,
    next_before: more ? (items[0]?.seq ?? null) : null,
  };
}
