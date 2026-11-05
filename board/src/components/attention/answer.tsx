import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { AttentionItem, OwnerAction } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { useStore } from "../../lib/store";
import { PeerNote } from "../board/host-chip";
import { useStanding } from "../board/peer-state";
import { CheckIcon } from "../shell/icons";
import { ActionFailure } from "./action-failure";
import { answeredStore, markAnswered } from "./answered";
import { sendMutation } from "./api";
import { SendIcon } from "./icons";
import { ReceiptPanel } from "./receipt";
import { useAction } from "./use-action";

// The words sent are exactly what the owner picked and typed (state.md,
// Attention: `answer` holds the owner's exact words).
function answerText(option: string | null, note: string): string {
  const typed = note.trim();
  if (option && typed) return `${option}\n\n${typed}`;
  return option ?? typed;
}

// The owner-only change an option applies, in the owner's words.
function applies(action: OwnerAction): string {
  return `${action.project} capacity from ${action.from} to ${action.to}`;
}

// Keys 1-3 answer, unless the owner is typing or holds a modifier.
function typing(target: EventTarget | null): boolean {
  return (
    target instanceof Element &&
    target.closest("input, textarea, select, [contenteditable]") !== null
  );
}

export function AnswerForm({ item }: { item: AttentionItem }) {
  const options = useMemo(() => item.options ?? [], [item.options]);
  const sent = useStore(answeredStore)[item.id];
  const [pick, setPick] = useState<string | null>(sent ? sent.option : null);
  const fired = useRef(false);
  const [note, setNote] = useState("");
  const answer = answerText(pick, note);
  const standing = useStanding(item);
  const stale = !item.still_current;
  const off = standing.canAct ? null : standing.reason;
  // An owner action applies only where the owner answers on their own host:
  // a forwarded answer never edits a peer's project (protocol.md 7.4).
  const host = ownerOf(item);

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
  const locked = action.recorded || action.pending || off !== null || stale;
  const { trigger } = action;

  // A new pick sends once the answer holding it has rendered, so the request
  // carries its own idempotency key. The same pick again is a retry.
  useEffect(() => {
    if (!fired.current) return;
    fired.current = false;
    trigger();
  }, [trigger]);

  const choose = useCallback(
    (label: string) => {
      if (label === pick) {
        trigger();
        return;
      }
      fired.current = true;
      setPick(label);
    },
    [pick, trigger],
  );

  useEffect(() => {
    if (locked || options.length === 0) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.repeat || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (typing(event.target)) return;
      const option = options[Number(event.key) - 1];
      if (option && !(host && option.action) && /^[1-9]$/.test(event.key)) choose(option.label);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [locked, options, choose, host]);

  const receipt = (
    <ReceiptPanel
      receiptKey={action.receiptKey}
      label={sent ? (sent.option ?? sent.answer) : (pick ?? "Answer")}
      text={{
        recorded: "Saved your exact words on the item",
        delivered: "Rang the orchestrator's pane",
        done: "The orchestrator read your answer",
      }}
    />
  );

  if (action.recorded) {
    const said = sent ? (sent.option ?? sent.answer) : (pick ?? answer);
    const picked = options.find((option) => option.label === (sent ? sent.option : pick));
    return (
      <>
        <div className="answered">
          <CheckIcon />
          <div className="t">
            <b>Answered {said}.</b>
            <div className="sub">
              {picked?.action ? `Applied ${applies(picked.action)}. ` : null}
              Your exact words are saved on the item.
            </div>
          </div>
        </div>
        {receipt}
      </>
    );
  }

  return (
    <>
      <div className="blk-h">
        Your answer
        {options.length && !locked ? (
          <span className="hint">click to send, or press 1-{Math.min(options.length, 9)}</span>
        ) : null}
      </div>
      <PeerNote standing={standing} />
      {options.length ? (
        <>
          <input
            className="note-in"
            name={`note-${item.id}`}
            placeholder="Add a note for the orchestrator (optional)"
            aria-label="Note"
            value={note}
            disabled={locked}
            onChange={(event) => setNote(event.target.value)}
          />
          <div className="opts" role="group" aria-label="Answer" aria-disabled={locked}>
            {options.map((option, index) => (
              <button
                key={option.label}
                type="button"
                className={`opt${option.recommended ? " rec" : ""}`}
                disabled={locked || (host !== undefined && option.action !== undefined)}
                aria-keyshortcuts={index < 9 ? String(index + 1) : undefined}
                onClick={() => choose(option.label)}
              >
                <span className="o-top">
                  <span className="o-name">{option.label}</span>
                  {option.recommended ? <span className="rec-tag">Recommended</span> : null}
                  {index < 9 ? <kbd>{index + 1}</kbd> : null}
                </span>
                {option.consequence ? <span className="what">{option.consequence}</span> : null}
                {option.action ? (
                  <span className="applies">Sets {applies(option.action)}</span>
                ) : null}
              </button>
            ))}
          </div>
          {host && options.some((option) => option.action) ? (
            <p className="opts-note">
              An option that changes a project is the owner's own: pick it on {host}'s board.
            </p>
          ) : null}
        </>
      ) : (
        <>
          <textarea
            className="note-in area"
            name={`answer-${item.id}`}
            placeholder="Your answer, in your own words"
            aria-label="Answer"
            value={note}
            disabled={locked}
            onChange={(event) => setNote(event.target.value)}
          />
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
      {stale ? (
        <p className="opts-note">
          This no longer applies, so answering is off. Withdraw it instead.
        </p>
      ) : null}
      <ActionFailure error={action.error} host={ownerOf(item)} />
      {receipt}
    </>
  );
}
