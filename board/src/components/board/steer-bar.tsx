import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";

import type { Interrupted, MutationResponse, SteerBody, SteerQueued } from "../../lib/api-types";
import { LIMITS, isTooLong } from "../../lib/limits";
import { receiptView, receiptsStore } from "../../lib/receipts";
import { useStore } from "../../lib/store";
import { ActionFailure } from "../attention/action-failure";
import { sendMutation } from "../attention/api";
import { LimitNote } from "../attention/limit-note";
import { useAction } from "../attention/use-action";
import { CheckIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { formatLisbonMoment } from "./format";
import { EscIcon, QueuedIcon, SendIcon, WarnIcon } from "./icons";
import "../attention/inbox.css";
import "../shell/project-stage.css";

const IDLE_NOTE = "Typed when the pane is idle. One line per send.";
export const NOT_WORKING = "Only while the agent is working";
export const NO_RUN = "Only an agent on a card can be interrupted";

// What the bar last did: a steer line, named by its receipt, or an
// interrupt's checked result.
type Last =
  | { kind: "steer"; receiptKey: string; text: string }
  | { kind: "interrupt"; data: Interrupted; at: string };

function steerPath(sessionId: string): string {
  return `/api/v1/agents/${encodeURIComponent(sessionId)}/steer`;
}

function interruptPath(sessionId: string): string {
  return `/api/v1/agents/${encodeURIComponent(sessionId)}/interrupt`;
}

// The message bar under a local agent's live terminal (protocol.md 7.4, Steer
// and Interrupt). Send queues one line in the outbox, typed when the pane is
// idle, as `tiller send --kind steer` does. Interrupt sends Escape after a
// confirmation and says whether the board saw the step stop within 10 s, as
// `tiller agent interrupt` does.
export function SteerBar({
  sessionId,
  name,
  off,
}: {
  sessionId: string;
  name: string;
  // Why Interrupt is off, or null while the agent works on a run.
  off: string | null;
}) {
  const [text, setText] = useState("");
  const [last, setLast] = useState<Last | null>(null);
  const [asking, setAsking] = useState(false);
  const line = text.trim();
  const over = isTooLong(line, LIMITS.comment);

  const sendSteer = useCallback(
    (idempotency_key: string) =>
      sendMutation<SteerQueued>("POST", steerPath(sessionId), {
        idempotency_key,
        text: line,
      } satisfies SteerBody),
    [sessionId, line],
  );
  const onSteered = useCallback((response: MutationResponse<SteerQueued>, receiptKey: string) => {
    setLast({ kind: "steer", receiptKey, text: response.data.body });
    setText("");
  }, []);
  const steer = useAction({
    binding: `${sessionId}:steer:${line}`,
    send: sendSteer,
    onRecorded: onSteered,
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (line === "" || over || steer.pending) return;
    steer.trigger();
  };

  return (
    <div className="av-steer-w">
      <form className="av-steer" aria-label={`Message ${name}`} onSubmit={submit}>
        <input
          className="note-in"
          aria-label="Message"
          placeholder={`Message ${name}`}
          value={text}
          disabled={steer.pending}
          onChange={(event) => setText(event.target.value)}
        />
        <button
          type="button"
          className="btn"
          disabled={off !== null}
          title={off ?? "Send Escape to stop the step it is on"}
          onClick={() => setAsking(true)}
        >
          <EscIcon />
          Interrupt...
        </button>
        <button
          type="submit"
          className="btn primary"
          disabled={line === "" || over || steer.pending}
        >
          <SendIcon />
          {steer.pending ? "Sending..." : "Send"}
        </button>
      </form>
      <div className="av-steer-st">
        {over ? (
          <LimitNote what="message" text={line} limit={LIMITS.comment} />
        ) : steer.error ? (
          <ActionFailure error={steer.error} host={undefined} />
        ) : last?.kind === "interrupt" ? (
          <InterruptResult data={last.data} at={last.at} />
        ) : last?.kind === "steer" ? (
          <SteerState receiptKey={last.receiptKey} text={last.text} />
        ) : (
          <span>{IDLE_NOTE}</span>
        )}
      </div>
      {asking ? (
        <InterruptDialog
          sessionId={sessionId}
          name={name}
          onDone={(data, at) => {
            setLast({ kind: "interrupt", data, at });
            setAsking(false);
          }}
          onClose={() => setAsking(false)}
        />
      ) : null}
    </div>
  );
}

// Where the last steer line is: queued until the pane is idle, then typed.
export function SteerState({ receiptKey, text }: { receiptKey: string; text: string }) {
  const state = useStore(receiptsStore);
  const receipt = state.byKey[receiptKey];
  if (!receipt) return <span>{IDLE_NOTE}</span>;
  const view = receiptView(receipt, state);
  const pill = (
    <span className="q" title={text}>
      <QueuedIcon />
      <span>{text}</span>
    </span>
  );
  if (view.stage === "failed") {
    return (
      <span className="av-steer-line" role="alert" data-state="failed">
        <b>Not typed.</b>
        {pill}
        <span>Delivery failed, so the daemon opened a recovery item for it.</span>
      </span>
    );
  }
  if (view.stage === "recorded") {
    return (
      <span className="av-steer-line" role="status" data-state="queued">
        <b>Queued.</b>
        {pill}
        <span>Typed when the pane is idle.</span>
      </span>
    );
  }
  return (
    <span className="av-steer-line" role="status" data-state="typed">
      <b>Typed.</b>
      {pill}
      <span>Into the pane at {formatLisbonMoment(view.deliveredAt) ?? "an unknown time"}.</span>
    </span>
  );
}

// What the board saw after the Escape: the step stopped, or no sign of it in
// 10 s.
function InterruptResult({ data, at }: { data: Interrupted; at: string }) {
  if (data.cancel === "confirmed") {
    return (
      <div className="av-receipt" role="status" title={data.evidence}>
        <CheckIcon />
        <span>
          <b>Interrupt confirmed.</b> The pane stopped at {formatLisbonMoment(at) ?? at}. The agent
          waits for a message.
        </span>
      </div>
    );
  }
  return (
    <div className="av-receipt warn" role="alert" title={data.evidence}>
      <WarnIcon />
      <span>
        <b>Not confirmed after 10 s.</b> The pane still shows work. Look at the terminal before you
        press again.
      </span>
    </div>
  );
}

// How long the daemon watches the pane after the Escape before it answers.
const WATCH_S = 10;

// The whole seconds left of the daemon's watch, counted from the press at
// `since`, or null when nothing is being watched.
function useWatchLeft(pending: boolean, since: number | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!pending) return;
    const timer = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(timer);
  }, [pending]);
  if (!pending || since === null) return null;
  const elapsed = Math.max(0, now - since) / 1000;
  return Math.max(0, Math.ceil(WATCH_S - elapsed));
}

function InterruptDialog({
  sessionId,
  name,
  onDone,
  onClose,
}: {
  sessionId: string;
  name: string;
  onDone: (data: Interrupted, at: string) => void;
  onClose: () => void;
}) {
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<Interrupted>("POST", interruptPath(sessionId), { idempotency_key }),
    [sessionId],
  );
  const onRecorded = useCallback(
    (response: MutationResponse<Interrupted>) =>
      onDone(response.data, response.receipt.recorded_at),
    [onDone],
  );
  // Each dialog is one press: its key is kept for a retry after a failure,
  // and a new dialog is a new press.
  const [press] = useState(() => `${sessionId}:interrupt:${Date.now()}`);
  const action = useAction({ binding: press, send, onRecorded });
  const [pressedAt, setPressedAt] = useState<number | null>(null);
  const left = useWatchLeft(action.pending, pressedAt);
  const interrupt = useCallback(() => {
    setPressedAt(Date.now());
    action.trigger();
  }, [action]);
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <Modal
      className="dialog av-int"
      labelledBy="interrupt-dialog-title"
      describedBy="interrupt-dialog-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="interrupt-dialog-title">Interrupt {name}?</h2>
      <p id="interrupt-dialog-body">
        This sends Escape to its pane, so it stops the step it is on. The run stays open, and the
        agent waits for you.
      </p>
      <dl className="av-facts">
        <dt>Works</dt>
        <dd>{NOT_WORKING}</dd>
        <dt>Check</dt>
        <dd>
          {left === null
            ? `The board watches the pane for ${WATCH_S} s`
            : left > 0
              ? `Watching the pane: ${left} s left`
              : "Watching the pane: reading the result"}
        </dd>
        <dt>Twice</dt>
        <dd>A second press is not the same as one. It can cancel a dialog the agent opens next.</dd>
      </dl>
      <ActionFailure error={action.error} host={undefined} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep working
        </button>
        <button
          type="button"
          className="btn danger"
          disabled={action.pending || action.recorded}
          onClick={interrupt}
        >
          <EscIcon />
          {action.pending ? "Checking..." : "Interrupt"}
        </button>
      </div>
    </Modal>
  );
}
