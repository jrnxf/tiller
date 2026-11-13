import { useCallback } from "react";

import type { AttentionItem } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { ActionFailure } from "./action-failure";
import { markWithdrawn } from "./answered";
import { sendMutation } from "./api";
import { useAction } from "./use-action";

// Withdrawing closes a question nobody is waiting on. A peer's item is
// withdrawn through the board host, which forwards it with `host` (protocol.md
// 7.4, D20); `off` says why it cannot be, for a peer not live or paired read-only.
export function WithdrawButton({ item, off }: { item: AttentionItem; off: string | null }) {
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AttentionItem>("POST", `/api/v1/attention/${item.id}/withdraw`, {
        ...hostField(item),
        idempotency_key,
      }),
    [item],
  );
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
        disabled={action.pending || action.recorded || off !== null}
        title={off ?? undefined}
        onClick={action.trigger}
      >
        {action.recorded ? "Withdrawn" : action.pending ? "Withdrawing..." : "Withdraw"}
      </button>
      <ActionFailure error={action.error} host={ownerOf(item)} />
    </>
  );
}
