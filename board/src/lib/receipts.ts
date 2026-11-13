import type { Receipt } from "./api-types";
import { createStore } from "./store";

// Receipts for the board's own mutations (protocol.md section 7.2). Recorded
// is the mutation's response; delivered and done come later over SSE, from the
// outbox row or the approval the receipt names. Progress is kept by entity id,
// so an event that arrives before the response is not lost. A forwarded
// mutation's row is the peer's, so its progress comes from that peer's events
// and is kept under the peer's label as well: an id is unique per host only.
type Stage = "recorded" | "delivered" | "done" | "failed";

interface Progress {
  state: string;
  // When each state was reached, so a done receipt still shows its delivery.
  reached: Record<string, string>;
}

export interface ActionReceipt extends Receipt {
  replayed: boolean;
  // The peer that recorded it, for a forwarded mutation; absent for this host.
  host?: string;
}

interface ReceiptsState {
  byKey: Record<string, ActionReceipt>;
  progress: Record<string, Progress>;
}

export const receiptsStore = createStore<ReceiptsState>({ byKey: {}, progress: {} });

export function saveReceipt(key: string, receipt: ActionReceipt) {
  receiptsStore.update((state) => ({ ...state, byKey: { ...state.byKey, [key]: receipt } }));
}

function progressKey(entityId: string, host: string | undefined): string {
  return host ? `${host}/${entityId}` : entityId;
}

// Called by the SSE stream for `outbox` and `approval` changes, with `host`
// for a peer's change.
export function recordProgress(entityId: string, state: string, at: string, host?: string) {
  const key = progressKey(entityId, host);
  receiptsStore.update((current) => {
    const reached = { ...current.progress[key]?.reached, [state]: at };
    return { ...current, progress: { ...current.progress, [key]: { state, reached } } };
  });
}

const OUTBOX_STAGES: Record<string, Stage> = {
  pending: "recorded",
  delivered: "delivered",
  acked: "done",
  failed: "failed",
};

const APPROVAL_STAGES: Record<string, Stage> = {
  requested: "recorded",
  accepted: "recorded",
  executing: "delivered",
  succeeded: "done",
  // A rejection executes nothing: once it is recorded there is nothing left.
  rejected: "done",
  failed: "failed",
  stale: "failed",
  uncertain: "failed",
};

export interface ReceiptView {
  stage: Stage;
  // The record's own state when it failed (`stale`, `uncertain`, `failed`).
  failedState: string | null;
  // The step that failed: delivered (1), or done (2) when delivery had
  // already happened, as with a merge that started and then went uncertain.
  failedStep: 1 | 2;
  // Whether anything is delivered at all; a plain edit is done when recorded.
  delivers: boolean;
  deliveredAt: string | null;
  doneAt: string | null;
}

export function receiptView(receipt: ActionReceipt, state: ReceiptsState): ReceiptView {
  const id = receipt.approval_id ?? receipt.outbox_id;
  if (!id) {
    return {
      stage: "done",
      failedState: null,
      failedStep: 1,
      delivers: false,
      deliveredAt: receipt.recorded_at,
      doneAt: receipt.recorded_at,
    };
  }
  const progress = state.progress[progressKey(id, receipt.host)];
  const table = receipt.approval_id ? APPROVAL_STAGES : OUTBOX_STAGES;
  const stage = progress ? (table[progress.state] ?? "recorded") : "recorded";
  const reached = progress?.reached ?? {};
  const doneAt = stage === "done" && progress ? (reached[progress.state] ?? null) : null;
  const deliveredAt = reached.delivered ?? reached.executing ?? doneAt;
  return {
    stage,
    failedState: stage === "failed" ? (progress?.state ?? null) : null,
    failedStep: reached.delivered || reached.executing ? 2 : 1,
    delivers: true,
    deliveredAt: stage === "recorded" ? null : deliveredAt,
    doneAt,
  };
}
