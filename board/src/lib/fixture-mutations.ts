// Stands in for the daemon's mutations (protocol.md section 7.4) on the dev and
// preview servers until T2.8 lands: a keyed replay table, the stale-head
// refusal, and the later receipt stages as change events.
import type { ChangeEvent } from "./api-types.ts";
import type { AttentionResponse } from "./api-types.ts";
import { FIXTURE_HEAD_SHA, fixtureResponses } from "./fixtures.ts";
import { attentionPath } from "./queries.ts";

interface Reply {
  status: number;
  body: unknown;
}

// A change event the server sends `delayMs` after the reply.
interface FollowUp {
  delayMs: number;
  event: Omit<ChangeEvent, "seq" | "at">;
}

export interface MutationResult {
  reply: Reply;
  followUps: FollowUp[];
}

const replies = new Map<string, MutationResult>();
let counter = 0;

function fakeId(prefix: string): string {
  counter += 1;
  return `${prefix}${String(counter).padStart(26 - prefix.length, "0")}`;
}

function refused(status: number, code: string, message: string, current?: object): Reply {
  return { status, body: { ok: false, error: { code, message, current } } };
}

function recorded(data: unknown, outboxId: string | null, approvalId: string | null): Reply {
  return {
    status: 200,
    body: {
      ok: true,
      replayed: false,
      events: [],
      receipt: {
        recorded_at: new Date().toISOString(),
        outbox_id: outboxId,
        approval_id: approvalId,
      },
      data,
    },
  };
}

function outboxStages(id: string): FollowUp[] {
  const event = (state: string) => ({
    kind: `outbox.${state}`,
    txn: `outbox.${state === "delivered" ? "deliver" : "ack"}`,
    entity: "outbox",
    entity_id: id,
    rev_after: null,
    card_id: null,
    entity_state: { id, state },
  });
  return [
    { delayMs: 1200, event: event("delivered") },
    { delayMs: 3200, event: event("acked") },
  ];
}

function approvalStages(id: string, decision: string): FollowUp[] {
  const event = (state: string) => ({
    kind: `approval.${state}`,
    txn: `approval.${state}`,
    entity: "approval",
    entity_id: id,
    rev_after: null,
    card_id: null,
    entity_state: { id, state },
  });
  if (decision === "reject") return [{ delayMs: 400, event: event("rejected") }];
  return [
    { delayMs: 1500, event: event("executing") },
    { delayMs: 4000, event: event("succeeded") },
  ];
}

// The item leaves the open queue once it is answered.
function answered(attentionId: string): FollowUp {
  const at = new Date().toISOString();
  return {
    delayMs: 300,
    event: {
      kind: "attention.answered",
      txn: "attention.answer",
      entity: "attention",
      entity_id: attentionId,
      rev_after: null,
      card_id: null,
      entity_state: { id: attentionId, answered_at: at, withdrawn_at: null },
    },
  };
}

function attentionFor(approvalId: string): string | null {
  const queue = fixtureResponses()[attentionPath] as AttentionResponse;
  return queue.items.find((item) => item.approval?.id === approvalId)?.id ?? null;
}

function handle(method: string, path: string, body: Record<string, unknown>): MutationResult {
  const answer = /^\/api\/v1\/attention\/([^/]+)\/answer$/.exec(path);
  if (method === "POST" && answer) {
    const outbox = fakeId("01JOUTB");
    return {
      reply: recorded({ id: answer[1], answer: body.answer }, outbox, null),
      followUps: [answered(answer[1] ?? ""), ...outboxStages(outbox)],
    };
  }
  const decide = /^\/api\/v1\/approvals\/([^/]+)\/decide$/.exec(path);
  if (method === "POST" && decide?.[1]) {
    if (body.head_sha !== FIXTURE_HEAD_SHA) {
      return {
        reply: refused(
          422,
          "refused",
          `approval ${decide[1]}: the PR head is ${FIXTURE_HEAD_SHA}, you approved ${String(body.head_sha)}`,
          { head_sha: FIXTURE_HEAD_SHA },
        ),
        followUps: [],
      };
    }
    const item = attentionFor(decide[1]);
    return {
      reply: recorded({ id: decide[1], state: "accepted" }, null, decide[1]),
      followUps: [
        ...(item ? [answered(item)] : []),
        ...approvalStages(decide[1], String(body.decision)),
      ],
    };
  }
  if (method === "PATCH" && /^\/api\/v1\/cards\/[^/]+$/.test(path)) {
    return { reply: recorded({ priority: body.priority }, null, null), followUps: [] };
  }
  if (method === "POST" && /^\/api\/v1\/(cards\/[^/]+\/comment|cards)$/.test(path)) {
    const outbox = fakeId("01JOUTB");
    return { reply: recorded({}, outbox, null), followUps: outboxStages(outbox) };
  }
  if (method === "POST" && /^\/api\/v1\/agents\/[^/]+\/focus$/.test(path)) {
    return { reply: recorded({}, null, null), followUps: [] };
  }
  return { reply: refused(404, "not_found", `no mutation at ${method} ${path}`), followUps: [] };
}

// A known idempotency key returns the stored reply and does nothing again.
export function fixtureMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
): MutationResult {
  const key = typeof body.idempotency_key === "string" ? body.idempotency_key : null;
  const known = key ? replies.get(key) : undefined;
  if (known) {
    const replayed =
      known.reply.status === 200
        ? { ...known.reply, body: { ...(known.reply.body as object), replayed: true } }
        : known.reply;
    return { reply: replayed, followUps: [] };
  }
  const result = handle(method, path, body);
  if (key) replies.set(key, result);
  return result;
}
