import { useSuspenseQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import type { BoardRow } from "../../lib/api-types";
import { projectsQuery } from "../../lib/queries";
import { sendMutation } from "./api";
import { PRIORITIES } from "./format";
import { PaneIcon, SendIcon } from "./icons";
import { InlineReceipt, ReceiptPanel } from "./receipt";
import { useAction } from "./use-action";

function Refusal({ message }: { message: string | undefined }) {
  return message ? (
    <p className="refusal" role="alert">
      {message}
    </p>
  ) : null;
}

// Reprioritize is a field edit, so it carries the card's `rev` and a stale
// one is a readable conflict (state.md principle 5).
function PriorityControl({ card }: { card: BoardRow }) {
  const [priority, setPriority] = useState(card.priority);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("PATCH", `/api/v1/cards/${card.id}`, {
        rev: card.rev,
        priority,
        idempotency_key,
      }),
    [card.id, card.rev, priority],
  );
  const action = useAction({ binding: `${card.id}:${card.rev}:${priority}`, send });
  const unchanged = priority === card.priority;
  return (
    <div className="ca-row">
      <label className="ca-l" htmlFor={`pri-${card.id}`}>
        Priority
      </label>
      <select
        id={`pri-${card.id}`}
        className="ca-select"
        value={priority}
        disabled={action.pending}
        onChange={(event) => setPriority(Number(event.target.value))}
      >
        {PRIORITIES.map((name, value) => (
          <option key={name} value={value}>
            P{value} {name}
          </option>
        ))}
      </select>
      {action.recorded ? (
        <InlineReceipt receiptKey={action.receiptKey} text={`Saved P${priority}`} />
      ) : (
        <button
          type="button"
          className="btn"
          disabled={unchanged || action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Saving..." : "Save"}
        </button>
      )}
      <Refusal message={action.error?.message} />
    </div>
  );
}

// A comment is an outbox note to the card's orchestrator.
function CommentControl({ card }: { card: BoardRow }) {
  const [text, setText] = useState("");
  const [lastReceipt, setLastReceipt] = useState<string | null>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${card.id}/comment`, {
        text: text.trim(),
        idempotency_key,
      }),
    [card.id, text],
  );
  const onRecorded = useCallback((_: unknown, key: string) => {
    setLastReceipt(key);
    setText("");
  }, []);
  const action = useAction({ binding: `${card.id}:${text.trim()}`, send, onRecorded });
  return (
    <div className="ca-block">
      <label className="ca-l" htmlFor={`comment-${card.id}`}>
        Comment to the orchestrator
      </label>
      <div className="ca-line">
        <input
          id={`comment-${card.id}`}
          className="note-in"
          placeholder={`A note on ${card.display_id}`}
          value={text}
          disabled={action.pending}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && text.trim() !== "") action.trigger();
          }}
        />
        <button
          type="button"
          className="btn"
          disabled={text.trim() === "" || action.pending}
          onClick={action.trigger}
        >
          <SendIcon />
          {action.pending ? "Sending..." : "Send"}
        </button>
      </div>
      <Refusal message={action.error?.message} />
      {lastReceipt ? (
        <ReceiptPanel
          receiptKey={lastReceipt}
          label="Comment"
          text={{
            recorded: "Saved your comment in the outbox",
            delivered: "Rang the orchestrator's pane",
            done: "The orchestrator read it",
          }}
        />
      ) : null}
    </div>
  );
}

const KINDS = ["ship", "scout", "question", "ops"] as const;

// Files a new card in the same project, through the same path as a typed ask.
function AddCardControl({ card, extra }: { card: BoardRow; extra: ReactNode }) {
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const [open, setOpen] = useState(false);
  const [project, setProject] = useState(card.project_id);
  const [kind, setKind] = useState<string>("ship");
  const [title, setTitle] = useState("");
  const [lastReceipt, setLastReceipt] = useState<string | null>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", "/api/v1/cards", {
        project,
        kind,
        title: title.trim(),
        body: `Follow-up to ${card.display_id}.`,
        idempotency_key,
      }),
    [card.display_id, kind, project, title],
  );
  const onRecorded = useCallback((_: unknown, key: string) => {
    setLastReceipt(key);
    setTitle("");
    setOpen(false);
  }, []);
  const action = useAction({ binding: `${project}:${kind}:${title.trim()}`, send, onRecorded });

  return (
    <div className="ca-block">
      {open ? (
        <form
          className="ca-form"
          onSubmit={(event) => {
            event.preventDefault();
            if (title.trim() !== "") action.trigger();
          }}
        >
          <label className="ca-l" htmlFor={`add-title-${card.id}`}>
            New card
          </label>
          <input
            id={`add-title-${card.id}`}
            className="note-in"
            placeholder="Title"
            value={title}
            disabled={action.pending}
            onChange={(event) => setTitle(event.target.value)}
          />
          <div className="ca-line">
            <select
              aria-label="Project"
              className="ca-select"
              value={project}
              onChange={(event) => setProject(event.target.value)}
            >
              {projects.projects.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
            <select
              aria-label="Kind"
              className="ca-select"
              value={kind}
              onChange={(event) => setKind(event.target.value)}
            >
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </select>
            <span className="sp" />
            <button type="button" className="btn ghost" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              type="submit"
              className="btn primary"
              disabled={title.trim() === "" || action.pending}
            >
              {action.pending ? "Adding..." : "Add card"}
            </button>
          </div>
          <Refusal message={action.error?.message} />
        </form>
      ) : (
        <div className="acts">
          <button type="button" className="btn" onClick={() => setOpen(true)}>
            Add card
          </button>
          {extra}
        </div>
      )}
      {lastReceipt ? (
        <ReceiptPanel
          receiptKey={lastReceipt}
          label="New card"
          text={{
            recorded: "Filed the card",
            delivered: "Rang the project's orchestrator",
            done: "The orchestrator picked it up",
          }}
        />
      ) : null}
    </div>
  );
}

function isShown(element: Element): boolean {
  for (let node: Element | null = element; node; node = node.parentElement) {
    if (getComputedStyle(node).display === "none") return false;
  }
  return true;
}

// Focuses the agent's pane in herdr, with the saved machine for a remote one.
export function OpenInHerdr({ sessionId, label }: { sessionId: string; label?: string }) {
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/agents/${sessionId}/focus`, { idempotency_key }),
    [sessionId],
  );
  // Focusing again later is a new click; only a double click is folded.
  const [round, setRound] = useState(0);
  const [lastReceipt, setLastReceipt] = useState<string | null>(null);
  const onRecorded = useCallback((_: unknown, key: string) => {
    setLastReceipt(key);
    setRound((n) => n + 1);
  }, []);
  const action = useAction({ binding: `${sessionId}:${round}`, send, onRecorded });
  const { trigger } = action;
  const button = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "o" || event.repeat) return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      const target = event.target;
      if (
        target instanceof Element &&
        target.closest("input, textarea, select, [contenteditable]")
      ) {
        return;
      }
      // On a narrow screen the detail is hidden behind the list; the key only
      // acts on a pane the owner can see.
      if (!button.current || !isShown(button.current)) return;
      trigger();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [trigger]);

  return (
    <span className="ca-herdr">
      <button
        type="button"
        ref={button}
        className="btn"
        disabled={action.pending}
        title="Focus this agent's pane in herdr"
        onClick={action.trigger}
      >
        <PaneIcon />
        {label ?? "Open in herdr"}
        <kbd>O</kbd>
      </button>
      {lastReceipt && !action.pending ? (
        <InlineReceipt receiptKey={lastReceipt} text="Focused in herdr" />
      ) : null}
      <Refusal message={action.error?.message} />
    </span>
  );
}

export function CardActions({
  card,
  sessionId,
}: {
  card: BoardRow | null;
  sessionId: string | null;
}) {
  const herdr = sessionId ? <OpenInHerdr sessionId={sessionId} /> : null;
  return (
    <div className="blk card-acts" data-testid="card-actions">
      <div className="blk-h">
        Card
        {card ? <span className="c">{card.display_id}</span> : null}
      </div>
      {card ? (
        <>
          <PriorityControl key={`${card.id}:${card.rev}`} card={card} />
          <CommentControl card={card} />
        </>
      ) : (
        <p className="ca-none">This card is not on the board any more.</p>
      )}
      {card ? (
        <AddCardControl card={card} extra={herdr} />
      ) : herdr ? (
        <div className="acts">{herdr}</div>
      ) : null}
    </div>
  );
}
