import { useCallback } from "react";

import type { AttentionItem } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { ActionFailure } from "./action-failure";
import { markWithdrawn } from "./answered";
import { sendMutation } from "./api";
import { useAction } from "./use-action";

// Withdrawing closes a question nobody is waiting on. The board host does not
// forward it, so a peer's item is withdrawn from that host.
export function WithdrawButton({ item, off }: { item: AttentionItem; off: string | null }) {
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AttentionItem>("POST", `/api/v1/attention/${item.id}/withdraw`, {
        ...hostField(item),
        idempotency_key,
      }),
    [item],
  );
  const host = ownerOf(item);
  const blocked = off ?? (host ? `${host} holds this item: withdraw it from that host` : null);
  const onRecorded = useCallback(() => markWithdrawn(item, new Date().toISOString()), [item]);
  const action = useAction({
    binding: item.id,
    receiptKey: `withdraw:${item.id}`,
    send,
    onRecorded,
  });
  return (
    <>
      <button
        type="button"
        className="btn primary"
        disabled={action.pending || action.recorded || blocked !== null}
        title={blocked ?? undefined}
        onClick={action.trigger}
      >
        {action.recorded ? "Withdrawn" : action.pending ? "Withdrawing..." : "Withdraw"}
      </button>
      <ActionFailure error={action.error} host={ownerOf(item)} />
    </>
  );
}
