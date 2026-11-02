import { useCallback, useRef, useState } from "react";

import type { MutationResponse } from "../../lib/api-types";
import { receiptsStore, saveReceipt } from "../../lib/receipts";
import { useStore } from "../../lib/store";
import { ActionError } from "./api";
import { ulid } from "./ulid";

interface ActionState {
  pending: boolean;
  error: ActionError | null;
  // Where this click's receipt is kept once it is recorded.
  receiptKey: string;
  recorded: boolean;
  trigger: () => void;
}

interface ActionOptions<T> {
  // What this click is about. The idempotency key is minted once per binding
  // and reused on every retry of it; a different binding is a different click.
  binding: string;
  // A fixed receipt key for a one-time action (an answer, an approval), so
  // its receipt is still there after moving away and back. Without one, each
  // binding gets its own receipt and the same control can act again.
  receiptKey?: string;
  send: (idempotencyKey: string) => Promise<MutationResponse<T>>;
  onRecorded?: (response: MutationResponse<T>, receiptKey: string) => void;
}

// One board action: a second click while it is in flight, or after it is
// recorded, sends nothing, and a retry after a failure reuses the same key.
export function useAction<T>({
  binding,
  receiptKey,
  send,
  onRecorded,
}: ActionOptions<T>): ActionState {
  const [minted, setMinted] = useState(() => ({ binding, key: ulid() }));
  let key = minted.key;
  if (minted.binding !== binding) {
    key = ulid();
    setMinted({ binding, key });
  }
  const slot = receiptKey ?? `key:${key}`;
  const inFlight = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);
  const recorded = useStore(receiptsStore).byKey[slot] !== undefined;

  const trigger = useCallback(() => {
    if (inFlight.current || receiptsStore.get().byKey[slot]) return;
    inFlight.current = true;
    setPending(true);
    setError(null);
    send(key)
      .then((response) => {
        saveReceipt(slot, { ...response.receipt, replayed: response.replayed });
        onRecorded?.(response, slot);
      })
      .catch((caught: unknown) => {
        setError(
          caught instanceof ActionError
            ? caught
            : new ActionError(0, "internal", "Something went wrong, so this was not saved."),
        );
      })
      .finally(() => {
        inFlight.current = false;
        setPending(false);
      });
  }, [key, onRecorded, send, slot]);

  return { pending, error, receiptKey: slot, recorded, trigger };
}
