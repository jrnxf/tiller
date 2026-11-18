import { expect, test } from "vitest";

import type {
  AgentRow,
  AskDelivery,
  AttentionItem,
  BoardRow,
  Project,
  ReplyItem,
  ThreadAsk,
  ThreadDecision,
  ThreadItem,
} from "../../lib/api-types";
import {
  activityText,
  chatRows,
  chatText,
  decisionText,
  firstRow,
  firstUnread,
  groupRows,
  modelName,
  queueChip,
  queueOf,
  quoteOf,
  undelivered,
  unreadReplies,
  waitingByProject,
  workingStatus,
} from "./model";

const NOW = Date.parse("2026-11-17T18:00:00.000Z");
const recent = new Date(NOW - 10_000).toISOString();

const project = (id: string, host?: string): Project =>
  ({ id, name: id, ...(host ? { host, link: "live" } : {}) }) as Project;

function orchestrator(projectId: string, activity: string, host?: string): AgentRow {
  return {
    session_id: `orch-${projectId}-${host ?? "here"}`,
    role: "orchestrator",
    model: "claude-opus-5-5",
    activity,
    activity_observed_at: recent,
    last_status_line: `${projectId} status`,
    last_status_at: recent,
    name: `${projectId}-orch`,
    card_id: null,
    host_id: host ?? "h1",
    workers: [],
    project_id: projectId,
    ...(host ? { host, link: "live" as const } : {}),
  };
}

function row(id: string, column: BoardRow["column"], priority = 2, closedAt?: string): BoardRow {
  return {
    id,
    display_id: id,
    project_id: "alpha",
    host_id: "h1",
    kind: "ship",
    title: `${id} title`,
    column,
    badge: column,
    needs_you: false,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: closedAt ?? null,
    priority,
    rev: 1,
  };
}

const reply = (id: string, projectId: string, repliedAt: string, read = false): ReplyItem => ({
  id,
  project: projectId,
  to: "ask",
  question: "q",
  asked_at: repliedAt,
  body: `${id} body`,
  replied_at: repliedAt,
  read_at: read ? repliedAt : null,
  rev: 1,
});

const question = (id: string, cardId: string, answered = false): AttentionItem =>
  ({
    id,
    card_id: cardId,
    answered_at: answered ? recent : null,
    withdrawn_at: null,
  }) as AttentionItem;

test("the list puts a project under Needs you, Working or Idle by what it needs", () => {
  const projects = [project("alpha"), project("beta"), project("gamma"), project("delta")];
  const agents = [
    orchestrator("beta", "working"),
    orchestrator("gamma", "waiting_input"),
    orchestrator("delta", "idle"),
  ];
  const rows = [{ ...row("a-1", "working"), project_id: "alpha" }];
  // alpha has no orchestrator but a question waits on the owner.
  const waiting = waitingByProject([question("q1", "a-1"), question("q2", "a-1", true)], rows);
  const list = chatRows(projects, agents, waiting, [], NOW);
  expect(list.map((entry) => [entry.project.id, entry.group, entry.waiting])).toEqual([
    ["alpha", "needs", 1],
    ["beta", "working", 0],
    ["gamma", "needs", 0],
    ["delta", "idle", 0],
  ]);
  expect(groupRows(list).map(({ group, rows: members }) => [group, members.length])).toEqual([
    ["needs", 2],
    ["working", 1],
    ["idle", 1],
  ]);
  expect(firstRow(list)?.project.id).toBe("alpha");
  expect(firstRow([])).toBeNull();
});

test("a peer's orchestrator of the same project id belongs to the peer's row only", () => {
  const list = chatRows(
    [project("fleet"), project("fleet", "host-b")],
    [orchestrator("fleet", "working", "host-b")],
    new Map(),
    [],
    NOW,
  );
  expect(list.map((entry) => [entry.group, entry.orchestrator?.host ?? null])).toEqual([
    ["idle", null],
    ["working", "host-b"],
  ]);
  expect(list[0]?.snippet).toBe("No orchestrator running");
});

test("a row shows its newest unread reply, else the orchestrator's status", () => {
  const list = chatRows(
    [project("alpha"), project("beta")],
    [orchestrator("alpha", "idle"), orchestrator("beta", "idle")],
    new Map(),
    [
      reply("r1", "alpha", "2026-11-17T17:00:00.000Z"),
      reply("r2", "alpha", "2026-11-17T17:30:00.000Z"),
      reply("r3", "alpha", "2026-11-17T17:45:00.000Z", true),
      reply("r4", "beta", "2026-11-17T17:50:00.000Z", true),
    ],
    NOW,
  );
  expect(list.map((entry) => [entry.unread, entry.snippet, entry.at])).toEqual([
    [2, "r2 body", "2026-11-17T17:30:00.000Z"],
    [0, "beta status", recent],
  ]);
});

function ask(delivery: AskDelivery | null): ThreadAsk {
  return {
    seq: 1,
    at: recent,
    id: "a1",
    text: "hello",
    source: "board",
    disposition: "unresolved",
    cards: [],
    delivery,
  };
}

const sent = (state: AskDelivery["state"], extra: Partial<AskDelivery> = {}): AskDelivery => ({
  state,
  sent_at: recent,
  delivered_at: null,
  seen_at: null,
  failed_at: null,
  failure_reason: null,
  ...extra,
});

test("a sent note says more only when it never arrived", () => {
  expect(undelivered(ask(null))).toBeNull();
  expect(undelivered(ask(sent("pending")))).toBeNull();
  expect(undelivered(ask(sent("acked", { seen_at: recent })))).toBeNull();
  expect(undelivered(ask(sent("failed", { failure_reason: "the pane is gone" })))).toBe(
    "Not delivered: the pane is gone",
  );
  expect(undelivered(ask(sent("failed")))).toBe("Not delivered");
});

test("an activity under a second gives its steps alone", () => {
  expect(activityText(0, 1)).toBe("1 step");
  expect(activityText(0.4, 3)).toBe("3 steps");
  expect(activityText(12, 3)).toBe("Worked for 12 seconds · 3 steps");
  expect(activityText(150, 2)).toBe("Worked for 3 minutes · 2 steps");
});

test("the tag and the project name do not reach the list's snippet", () => {
  const replied = { ...reply("r1", "alpha", recent), body: "[chat] alpha: tests pass" };
  const [entry] = chatRows([project("alpha")], [], new Map(), [replied], NOW);
  expect(entry?.snippet).toBe("tests pass");
});

test("a model id reads as its name", () => {
  expect(modelName("opus")).toBe("Opus");
  expect(modelName("claude-opus-5-5")).toBe("Opus 5.5");
  expect(modelName("claude-sonnet-4-5-20250929")).toBe("Sonnet 4.5");
});

test("only the unread replies of a thread are marked read", () => {
  const items: ThreadItem[] = [
    { type: "ask", ...ask(null) },
    { type: "reply", seq: 2, at: recent, reply: reply("r1", "alpha", recent) },
    { type: "reply", seq: 3, at: recent, reply: reply("r2", "alpha", recent, true) },
    { type: "reply", seq: 4, at: recent, reply: reply("r3", "alpha", recent) },
  ];
  expect(unreadReplies(items)).toEqual(["r1", "r3"]);
  expect(unreadReplies(items.slice(0, 1))).toEqual([]);
});

const ids = (entries: { row: BoardRow }[]) => entries.map((entry) => entry.row.id);

test("the queue sorts running and next by priority and keeps the newest done cards", () => {
  const agent = orchestrator("alpha", "working");
  agent.workers = [{ ...orchestrator("alpha", "working"), card_id: "w2", name: "w2-builder" }];
  const rows = [
    row("w1", "working", 3),
    row("w2", "landing", 1),
    row("q1", "queued", 2),
    row("q2", "queued", 0),
    row("d1", "done", 2, "2026-11-17T10:00:00.000Z"),
    row("d2", "done", 2, "2026-11-17T12:00:00.000Z"),
    row("d3", "done", 2, "2026-11-17T11:00:00.000Z"),
    row("d4", "done", 2, "2026-11-17T13:00:00.000Z"),
    { ...row("o1", "working"), project_id: "beta" },
  ];
  const queue = queueOf(project("alpha"), rows, agent);
  expect(ids(queue.running)).toEqual(["w2", "w1"]);
  expect(queue.running.map((entry) => entry.worker)).toEqual(["w2-builder", null]);
  expect(ids(queue.next)).toEqual(["q2", "q1"]);
  expect(ids(queue.done)).toEqual(["d4", "d2", "d3"]);
  expect(queueChip(queue)).toBe("2 running · 2 next");
  expect(queueChip(queueOf(project("gamma"), rows, null))).toBeNull();
});

test("a card chip reads as its id in the text the orchestrator gets", () => {
  expect(
    chatText([
      { kind: "text", text: "Is " },
      { kind: "mention", key: "01JCARD1", name: "alpha-41" },
      { kind: "text", text: "   next? " },
    ]),
  ).toBe("Is alpha-41 next?");
});

test("a quoted reply marks each line and cuts a long one", () => {
  expect(quoteOf("one\ntwo")).toBe("> one\n> two\n\n");
  const quote = quoteOf("x".repeat(400));
  expect(quote).toBe(`> ${"x".repeat(280)}…\n\n`);
});

test("a closed decision says when, and a day word never follows at", () => {
  const decision = {
    state: "decided",
    choice: "Retry",
    source: "attention",
    decided_at: "2026-11-17T18:02:00.000Z",
  } as unknown as ThreadDecision;
  expect(decisionText(decision, () => "18:02")).toBe("You chose Retry at 18:02");
  expect(decisionText(decision, () => "Yesterday 18:02")).toBe("You chose Retry · Yesterday 18:02");
});

const replyAt = (seq: number, read: boolean) =>
  ({ type: "reply", seq, reply: { read_at: read ? recent : null } }) as unknown as ThreadItem;
const askAt = (seq: number) => ({ type: "ask", seq }) as unknown as ThreadItem;

test("the New line goes above the first unread reply, unless nothing comes before it", () => {
  expect(
    firstUnread([askAt(1), replyAt(2, true), askAt(3), replyAt(4, false), replyAt(5, false)]),
  ).toBe(4);
  expect(firstUnread([replyAt(1, false), askAt(2)])).toBeNull();
  expect(firstUnread([askAt(1), replyAt(2, true)])).toBeNull();
});

const statusOf = (line: string, at: string) =>
  ({ last_status_line: line, last_status_at: at }) as unknown as AgentRow;

test("the Working line shows a status set after the owner's message, without its tag", () => {
  const askedAt = "2026-11-17T18:00:00.000Z";
  expect(
    workingStatus(
      statusOf("[chat] alpha: Reading `cutover.md`", "2026-11-17T18:00:05.000Z"),
      askedAt,
    ),
  ).toBe("Reading cutover.md");
  expect(
    workingStatus(statusOf("Idle since lunch", "2026-11-17T17:59:00.000Z"), askedAt),
  ).toBeNull();
  expect(workingStatus(null, askedAt)).toBeNull();
});
