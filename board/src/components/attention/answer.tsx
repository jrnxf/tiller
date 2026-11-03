import { useCallback, useState } from "react";

import type { AttentionItem } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { PeerNote } from "../board/host-chip";
import { useStanding } from "../board/peer-state";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { ReceiptPanel } from "./receipt";
import { useStore } from "../../lib/store";
import { answeredStore, markAnswered } from "./answered";
import { SendIcon } from "./icons";
import { useAction } from "./use-action";

// The words sent are exactly what the owner picked and typed (state.md,
// Attention: `answer` holds the owner's exact words).
function answerText(option: string | null, note: string): string {
  const typed = note.trim();
  if (option && typed) return `${option}\n\n${typed}`;
  return option ?? typed;
}

export function AnswerForm({ item }: { item: AttentionItem }) {
  const options = item.options ?? [];
  const recommended = options.find((option) => option.recommended)?.label ?? null;
  const sent = useStore(answeredStore)[item.id];
  const [pick, setPick] = useState<string | null>(
    sent ? sent.option : (recommended ?? options[0]?.label ?? null),
  );
  const [note, setNote] = useState("");
  const answer = answerText(pick, note);
  const standing = useStanding(item);
  const off = standing.canAct ? null : standing.reason;

  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<AttentionItem>("POST", `/api/v1/attention/${item.id}/answer`, {
        answer,
        rev: item.rev,
        ...hostField(item),
        idempotency_key,
      }),
    [answer, item],
  );
  const onRecorded = useCallback(() => markAnswered(item, answer, pick), [item, answer, pick]);
  const action = useAction({
    // The rev is part of the binding: after a refusal the item is fetched
    // again, and the same words on the newer item are a new request.
    binding: `${item.id}:${item.rev}:${answer}`,
    receiptKey: `attention:${item.id}`,
    send,
    onRecorded,
  });
  const locked = action.recorded || action.pending || off !== null;

  return (
    <>
      <div className="blk">
        <div className="blk-h">
          {options.length ? "Your answer, and what each one does" : "Your answer"}
        </div>
        <PeerNote standing={standing} />
        {options.map((option) => (
          <label key={option.label} className={`optrow${locked ? " dis" : ""}`}>
            <input
              type="radio"
              name={`opt-${item.id}`}
              value={option.label}
              checked={pick === option.label}
              disabled={locked}
              onChange={() => setPick(option.label)}
            />
            <span className="ol">{option.label}</span>
            {option.recommended ? <span className="lab sm acc">Recommended</span> : <span />}
            {option.consequence ? <span className="oc">{option.consequence}</span> : null}
          </label>
        ))}
        {action.recorded ? null : (
          <>
            {options.length ? (
              <input
                className="note-in"
                name={`note-${item.id}`}
                placeholder="Optional note for the orchestrator"
                aria-label="Note"
                value={note}
                disabled={locked}
                onChange={(event) => setNote(event.target.value)}
              />
            ) : (
              <textarea
                className="note-in area"
                name={`answer-${item.id}`}
                placeholder="Your answer, in your own words"
                aria-label="Answer"
                value={note}
                disabled={locked}
                onChange={(event) => setNote(event.target.value)}
              />
            )}
            <div className="acts">
              <button
                type="button"
                className="btn primary lg"
                disabled={locked || answer === ""}
                title={off ?? undefined}
                onClick={action.trigger}
              >
                <SendIcon />
                {action.pending ? "Sending..." : "Answer"}
              </button>
            </div>
          </>
        )}
        <ActionFailure error={action.error} host={ownerOf(item)} />
      </div>
      <ReceiptPanel
        receiptKey={action.receiptKey}
        label={sent ? (sent.option ?? sent.answer) : (pick ?? "Answer")}
        text={{
          recorded: "Saved your exact words on the item",
          delivered: "Rang the orchestrator's pane",
          done: "The orchestrator read your answer",
        }}
      />
    </>
  );
}
