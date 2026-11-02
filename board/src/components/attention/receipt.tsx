import { receiptView, receiptsStore } from "../../lib/receipts";
import { useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { formatWhen } from "./format";
import { OkIcon, SendIcon } from "./icons";

export interface ReceiptText {
  recorded: string;
  delivered: string;
  done: string;
}

const ORDER = ["recorded", "delivered", "done"] as const;
const NAMES = { recorded: "Recorded", delivered: "Delivered", done: "Done" } as const;

const FAILED: Record<string, string> = {
  failed: "Delivery failed. The daemon opened a recovery item for it.",
  stale: "The head moved before the merge started, so the approval no longer applies.",
  uncertain: "The merge was sent but its result could not be read. Reconcile resolves it.",
};

// The three stages of a receipt (point 36): recorded is the commit, delivered
// and done follow from the records over SSE.
export function ReceiptPanel({
  receiptKey,
  label,
  text,
}: {
  receiptKey: string;
  label: string;
  text: ReceiptText;
}) {
  const state = useStore(receiptsStore);
  const receipt = state.byKey[receiptKey];
  if (!receipt) return null;
  const view = receiptView(receipt, state);
  const reached = view.stage === "failed" ? view.failedStep : ORDER.indexOf(view.stage) + 1;
  const times = [receipt.recorded_at, view.deliveredAt, view.doneAt];

  return (
    <div className="receipt" role="status" aria-live="polite" data-stage={view.stage}>
      <div className="rc-h">
        {view.stage === "done" ? <OkIcon /> : <SendIcon />}
        Receipt
        <span className="lab sm">{label}</span>
        {receipt.replayed ? <span className="lab sm mute">already recorded</span> : null}
        <span className="sp" />
      </div>
      <div className="rc-steps">
        {ORDER.map((stage, index) => {
          const status =
            index < reached
              ? "done"
              : index === reached && view.stage !== "failed"
                ? "active"
                : view.stage === "failed" && index === reached
                  ? "failed"
                  : "todo";
          const time = times[index];
          return (
            <div key={stage} className={`rs ${status}`} data-step={stage}>
              <div className="rs-top">
                <span className="ic">
                  <CheckIcon />
                </span>
              </div>
              <div className="rt">
                <b>{NAMES[stage]}</b>
                <small>
                  {status === "done" ? (
                    <>
                      {stage === "delivered" && !view.delivers ? "Nothing to deliver" : text[stage]}
                      {time ? (
                        <>
                          <br />
                          <span className="mono tnum">{formatWhen(time)}</span>
                        </>
                      ) : null}
                    </>
                  ) : status === "failed" ? (
                    (FAILED[view.failedState ?? "failed"] ?? FAILED.failed)
                  ) : status === "active" ? (
                    "in progress..."
                  ) : (
                    "waiting"
                  )}
                </small>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// A one-line receipt for small edits (priority, focus).
export function InlineReceipt({ receiptKey, text }: { receiptKey: string; text: string }) {
  const state = useStore(receiptsStore);
  const receipt = state.byKey[receiptKey];
  if (!receipt) return null;
  const view = receiptView(receipt, state);
  return (
    <span className="rc-inline" role="status" data-stage={view.stage}>
      <CheckIcon />
      {text}, {formatWhen(receipt.recorded_at)}
      {view.delivers && view.stage !== "done" ? ", delivering" : ""}
    </span>
  );
}
