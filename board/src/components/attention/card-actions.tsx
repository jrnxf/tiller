import { Menu } from "@base-ui/react/menu";
import { useSuspenseQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import type { BoardRow, MutationResponse } from "../../lib/api-types";
import { LIMITS, isTooLong } from "../../lib/limits";
import { focusReason, hostField, ownerKey, ownerOf, projectGroups } from "../../lib/peers";
import { projectsQuery } from "../../lib/queries";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { FOCUS_TOOLTIP, focusOutcome, focusPath } from "../board/terminal-open";
import type { FocusData } from "../board/terminal-open";
import { CopyCommand } from "../board/toast";
import { PeerNote } from "../board/host-chip";
import { useHostLabel, useMachineRows, useStanding, useNamedProjects } from "../board/peer-state";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { PRIORITIES } from "./format";
import { DotsIcon, PaneIcon, SendIcon } from "./icons";
import { LimitNote } from "./limit-note";
import { InlineReceipt, ReceiptPanel } from "./receipt";
import { Select } from "../ui/select";
import { useAction } from "./use-action";
import "./card-lifecycle.css";

// Reprioritize is a field edit, so it carries the card's `rev` and a stale
// one is a readable conflict (state.md principle 5).
function PriorityControl({ card, off }: { card: BoardRow; off: string | null }) {
  const [priority, setPriority] = useState(card.priority);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("PATCH", `/api/v1/cards/${card.id}`, {
        rev: card.rev,
        priority,
        ...hostField(card),
        idempotency_key,
      }),
    [card, priority],
  );
  const action = useAction({ binding: `${card.id}:${card.rev}:${priority}`, send });
  const unchanged = priority === card.priority;
  return (
    <div className="ca-row">
      <label className="ca-l" htmlFor={`pri-${card.id}`}>
        Priority
      </label>
      <Select
        id={`pri-${card.id}`}
        value={String(priority)}
        disabled={action.pending || off !== null}
        title={off ?? undefined}
        onValueChange={(next) => setPriority(Number(next))}
        options={PRIORITIES.map((name, value) => ({
          value: String(value),
          label: `P${value} ${name}`,
        }))}
      />
      {action.recorded ? (
        <InlineReceipt receiptKey={action.receiptKey} text={`Saved P${priority}`} />
      ) : (
        <button
          type="button"
          className="btn"
          disabled={unchanged || action.pending || off !== null}
          title={off ?? undefined}
          onClick={action.trigger}
        >
          {action.pending ? "Saving..." : "Save"}
        </button>
      )}
      <ActionFailure error={action.error} host={ownerOf(card)} />
    </div>
  );
}

// A comment is an outbox note to the card's orchestrator, as `tiller card
// comment` sends. The Inbox and the card page both carry it.
export function CommentControl({ card, off }: { card: BoardRow; off: string | null }) {
  const [text, setText] = useState("");
  const [lastReceipt, setLastReceipt] = useState<string | null>(null);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${card.id}/comment`, {
        text: text.trim(),
        ...hostField(card),
        idempotency_key,
      }),
    [card, text],
  );
  const onRecorded = useCallback((_: unknown, key: string) => {
    setLastReceipt(key);
    setText("");
  }, []);
  const action = useAction({ binding: `${card.id}:${text.trim()}`, send, onRecorded });
  const over = isTooLong(text.trim(), LIMITS.comment);
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
          disabled={action.pending || off !== null}
          title={off ?? undefined}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && text.trim() !== "" && !over && off === null) {
              action.trigger();
            }
          }}
        />
        <button
          type="button"
          className="btn"
          disabled={text.trim() === "" || over || action.pending || off !== null}
          title={off ?? undefined}
          onClick={action.trigger}
        >
          <SendIcon />
          {action.pending ? "Sending..." : "Send"}
        </button>
      </div>
      <LimitNote what="comment" text={text.trim()} limit={LIMITS.comment} />
      <ActionFailure error={action.error} host={ownerOf(card)} />
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
function AddCardControl({
  card,
  extra,
  off,
}: {
  card: BoardRow;
  extra: ReactNode;
  off: string | null;
}) {
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const machines = useMachineRows();
  const localLabel = useHostLabel({});
  const groups = projectGroups(projects.projects, machines, localLabel);
  const grouped = groups.some((group) => group.peer);
  const [open, setOpen] = useState(false);
  // A project slug is unique only per host, so the choice is the owner-qualified key.
  const [project, setProject] = useState(ownerKey(card.project_id, card));
  const chosen = projects.projects.find((p) => ownerKey(p.id, p) === project);
  const chosenGroup = groups.find(
    (group) => chosen !== undefined && group.projects.includes(chosen),
  );
  const chosenOff =
    chosenGroup && !chosenGroup.standing.canAct ? chosenGroup.standing.reason : null;
  const blocked = off ?? chosenOff;
  const [kind, setKind] = useState<string>("ship");
  const [title, setTitle] = useState("");
  const [lastReceipt, setLastReceipt] = useState<string | null>(null);
  const targetProject = chosen?.id ?? card.project_id;
  const targetHost = ownerOf(chosen ?? card);
  const send = (idempotency_key: string) =>
    sendMutation("POST", "/api/v1/cards", {
      project: targetProject,
      kind,
      title: title.trim(),
      body: `Follow-up to ${card.display_id}.`,
      ...(targetHost ? { host: targetHost } : {}),
      idempotency_key,
    });
  const onRecorded = useCallback((_: unknown, key: string) => {
    setLastReceipt(key);
    setTitle("");
    setOpen(false);
  }, []);
  const action = useAction({ binding: `${project}:${kind}:${title.trim()}`, send, onRecorded });
  const over = isTooLong(title.trim(), LIMITS.cardTitle);

  return (
    <div className="ca-block">
      {open ? (
        <form
          className="ca-form"
          onSubmit={(event) => {
            event.preventDefault();
            if (title.trim() !== "" && !over && blocked === null) action.trigger();
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
            <Select
              label="Project"
              value={project}
              onValueChange={setProject}
              {...(grouped
                ? {
                    groups: groups.map((group) => ({
                      label: group.standing.reason
                        ? `${group.host} - ${group.standing.reason}`
                        : (group.host ?? ""),
                      disabled: !group.standing.canAct,
                      options: group.projects.map((p) => ({
                        value: ownerKey(p.id, p),
                        label: p.name,
                      })),
                    })),
                  }
                : {
                    options: groups.flatMap((group) =>
                      group.projects.map((p) => ({
                        value: ownerKey(p.id, p),
                        label: p.name,
                        disabled: !group.standing.canAct,
                      })),
                    ),
                  })}
            />
            <Select
              label="Kind"
              value={kind}
              onValueChange={setKind}
              options={KINDS.map((k) => ({ value: k, label: k }))}
            />
            <span className="sp" />
            <button type="button" className="btn" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              type="submit"
              className="btn primary"
              disabled={title.trim() === "" || over || action.pending || blocked !== null}
              title={blocked ?? undefined}
            >
              {action.pending ? "Adding..." : "Add card"}
            </button>
          </div>
          <LimitNote what="title" text={title.trim()} limit={LIMITS.cardTitle} />
          <ActionFailure error={action.error} host={targetHost} />
        </form>
      ) : (
        <div className="acts">
          <button
            type="button"
            className="btn"
            disabled={off !== null}
            title={off ?? undefined}
            onClick={() => setOpen(true)}
          >
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

// Shows the agent's pane in a terminal on this host (Open in terminal).
// `O` presses it while the element `shown` holds is on screen.
function useTerminalFocus(sessionId: string, off: string | null | undefined) {
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<FocusData>("POST", focusPath(sessionId), { idempotency_key }),
    [sessionId],
  );
  // Focusing again later is a new click; only a double click is folded.
  const [round, setRound] = useState(0);
  const [last, setLast] = useState<{ receipt: string; data: FocusData } | null>(null);
  const onRecorded = useCallback((response: MutationResponse<FocusData>, key: string) => {
    setLast({ receipt: key, data: response.data });
    setRound((n) => n + 1);
  }, []);
  const outcome = last ? focusOutcome(last.data, sessionId) : null;
  const action = useAction({ binding: `${sessionId}:${round}`, send, onRecorded });
  const { trigger } = action;
  const shown = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "o" || event.repeat) return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (shortcutsBlocked(event)) return;
      // On a narrow screen the detail is hidden behind the list; the key only
      // acts on a pane the owner can see.
      if (!shown.current || !isShown(shown.current)) return;
      trigger();
    };
    if (off) return;
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [trigger, off]);

  const feedback = (
    <>
      {last && outcome && !action.pending ? (
        outcome.tone === "ok" ? (
          <InlineReceipt receiptKey={last.receipt} text={outcome.title} />
        ) : (
          <p className="refusal" role="alert">
            {outcome.detail}
            {outcome.command ? (
              <>
                {" "}
                <CopyCommand command={outcome.command} />
              </>
            ) : null}
          </p>
        )
      ) : null}
      <ActionFailure error={action.error} host={undefined} />
    </>
  );
  return { action, shown, feedback };
}

function OpenInTerminal({
  sessionId,
  off,
}: {
  sessionId: string;
  // Why the button is off: pane focus is not forwarded to a peer.
  off?: string | null;
}) {
  const { action, shown, feedback } = useTerminalFocus(sessionId, off);
  return (
    <span className="ca-term">
      <button
        type="button"
        ref={shown}
        className="btn"
        disabled={action.pending || Boolean(off)}
        title={off ?? FOCUS_TOOLTIP}
        onClick={action.trigger}
      >
        <PaneIcon />
        Open in terminal
        <kbd>O</kbd>
      </button>
      {feedback}
    </span>
  );
}

// Open in terminal as an item of a ··· menu, for a detail whose primary action
// is on the board. Its receipt or refusal shows to the menu's left.
export function TerminalMenu({
  sessionId,
  off,
  children,
}: {
  sessionId: string;
  off?: string | null;
  // More items, above Open in terminal.
  children?: ReactNode;
}) {
  const { action, shown, feedback } = useTerminalFocus(sessionId, off);
  return (
    <span className="ca-term">
      {feedback}
      <Menu.Root>
        <Menu.Trigger ref={shown} className="btn sm cm-dots" aria-label="More actions">
          <DotsIcon />
        </Menu.Trigger>
        <Menu.Portal>
          <Menu.Positioner className="sel-pos" align="end" sideOffset={4}>
            <Menu.Popup className="cm-popup" aria-label="More actions">
              {children}
              <Menu.Item
                className="cm-item"
                disabled={action.pending || Boolean(off)}
                title={off ?? FOCUS_TOOLTIP}
                onClick={action.trigger}
              >
                <PaneIcon />
                Open in terminal
                <kbd>O</kbd>
              </Menu.Item>
            </Menu.Popup>
          </Menu.Positioner>
        </Menu.Portal>
      </Menu.Root>
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
  const standing = useStanding(card ?? {});
  const off = standing.canAct ? null : standing.reason;
  const open = sessionId ? (
    <OpenInTerminal sessionId={sessionId} off={card ? focusReason(ownerOf(card)) : null} />
  ) : null;
  return (
    <div className="blk card-acts" data-testid="card-actions">
      <div className="blk-h">
        Card
        {card ? <span className="c">{card.display_id}</span> : null}
      </div>
      <PeerNote standing={standing} />
      {card ? (
        <>
          <PriorityControl key={`${card.id}:${card.rev}`} card={card} off={off} />
          <CommentControl card={card} off={off} />
        </>
      ) : (
        <p className="ca-none">This card is not on the board any more.</p>
      )}
      {card ? (
        <AddCardControl card={card} extra={open} off={off} />
      ) : open ? (
        <div className="acts">{open}</div>
      ) : null}
    </div>
  );
}
