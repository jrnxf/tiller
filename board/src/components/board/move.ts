import type { BoardRow } from "../../lib/api-types";
import { hostField } from "../../lib/peers";
import { ActionError, sendMutation } from "../attention/api";
import { TILLER_BUG, cardName, moveKeys, movedBody } from "./bugs";
import type { MoveTarget } from "./bugs";

// What `POST /api/v1/cards` returns: the card it filed (protocol.md 2.2).
type Made = { card?: { display_id?: string } } | null;

// How a Move ended. A close that failed after the create leaves both cards
// open, and the retry replays the create by its key before it closes again.
export type MoveResult =
  | { state: "moved"; made: string }
  | { state: "create-failed"; error: ActionError }
  | { state: "close-failed"; made: string; error: ActionError };

export interface MovingBug {
  row: BoardRow;
  // The card's text, which the new card carries with a link back.
  body: string | null;
  // The label of the host that owns the card.
  hostLabel: string | null;
}

function asActionError(caught: unknown): ActionError {
  return caught instanceof ActionError
    ? caught
    : new ActionError(0, "internal", "Something went wrong, so this was not saved.");
}

// A Move: a create on the target host, then a close of the card as
// superseded by it. Both carry keys from `moveKeys`, so pressing Move again
// after either step failed is safe.
export async function moveBug(
  { row, body, hostLabel }: MovingBug,
  target: MoveTarget,
): Promise<MoveResult> {
  const keys = moveKeys(row, target);
  let made: string;
  try {
    const created = await sendMutation<Made>("POST", "/api/v1/cards", {
      project: target.project.id,
      kind: row.kind,
      title: row.title,
      body: movedBody(body, cardName(row, hostLabel), row.filed_from),
      priority: row.priority,
      category: TILLER_BUG,
      ...(target.machine.kind === "local" ? {} : { host: target.machine.label }),
      idempotency_key: keys.create,
    });
    made = cardName(
      { display_id: created.data?.card?.display_id ?? `a ${target.project.name} card` },
      target.machine.label,
    );
  } catch (caught) {
    return { state: "create-failed", error: asActionError(caught) };
  }
  try {
    await sendMutation("POST", `/api/v1/cards/${row.id}/close`, {
      outcome: "superseded",
      reason: `Moved to ${made}`,
      ...hostField(row),
      idempotency_key: keys.close,
    });
  } catch (caught) {
    return { state: "close-failed", made, error: asActionError(caught) };
  }
  return { state: "moved", made };
}
