import { Menu } from "@base-ui/react/menu";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";

import type { BoardRow } from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { LIMITS, isTooLong } from "../../lib/limits";
import { lisbonDate, lisbonInstant } from "../../lib/lisbon-input";
import { hostField, ownerOf } from "../../lib/peers";
import type { Owned } from "../../lib/peers";
import { cardQuery } from "../../lib/queries";
import { ChevronDownIcon } from "../board/icons";
import { CheckIcon } from "../shell/icons";
import { ProjectDialog } from "../shell/project-dialog";
import { Segmented } from "../ui/segmented";
import { ActionFailure } from "./action-failure";
import { sendMutation } from "./api";
import { CancelCardIcon, DotsIcon, EditIcon, PauseIcon, PlayIcon, ReopenIcon } from "./icons";
import { LimitNote } from "./limit-note";
import { useAction } from "./use-action";
import "./card-lifecycle.css";

// Where a card is in its life, as the card page and the Inbox menu act on it.
export type Phase = "open" | "held" | "closed";

export function phaseOf(card: BoardRow): Phase {
  if (card.closed_at !== null) return "closed";
  return card.parked_until !== null ? "held" : "open";
}

// What Close offers. "Done" is the outcome the card's kind ends well with
// (`state.md`, Card); the other two are the outcomes any card may end with.
const DONE_OUTCOME: Record<string, string> = {
  ship: "landed",
  scout: "reported",
  question: "answered",
  ops: "completed",
};

export type CloseChoice = "done" | "wont" | "duplicate";

export function closeOutcome(kind: string, choice: CloseChoice): string {
  if (choice === "wont") return "abandoned";
  if (choice === "duplicate") return "superseded";
  return DONE_OUTCOME[kind] ?? "completed";
}

// How a closed card's outcome reads on its page.
export function outcomeLabel(outcome: string | null): string {
  switch (outcome) {
    case "abandoned":
      return "won't do";
    case "superseded":
      return "duplicate";
    case "cancelled":
    case "failed":
      return outcome;
    default:
      return "done";
  }
}

const REOPEN_REASON = "Reopened from the board";

export type DialogName = "close" | "cancel" | "hold" | "edit";

// One card's lifecycle controls: which dialog is open, and the two moves that
// ask nothing (Release and Reopen), each a single idempotent click.
export function useCardLifecycle(card: BoardRow) {
  const [dialog, open] = useState<DialogName | null>(null);
  const release = useAction({
    binding: `${card.id}:${card.rev}:release`,
    send: useCallback(
      (idempotency_key: string) =>
        sendMutation("DELETE", `/api/v1/cards/${card.id}/hold`, {
          ...hostField(card),
          idempotency_key,
        }),
      [card],
    ),
  });
  const reopen = useAction({
    binding: `${card.id}:${card.rev}:reopen`,
    send: useCallback(
      (idempotency_key: string) =>
        sendMutation("POST", `/api/v1/cards/${card.id}/reopen`, {
          reason: REOPEN_REASON,
          ...hostField(card),
          idempotency_key,
        }),
      [card],
    ),
  });
  return { dialog, open, release, reopen };
}

export type Lifecycle = ReturnType<typeof useCardLifecycle>;

// Why a Release or Reopen did not go through, under the controls.
export function LifecycleFailure({ card, life }: { card: BoardRow; life: Lifecycle }) {
  return <ActionFailure error={life.release.error ?? life.reopen.error} host={ownerOf(card)} />;
}

// The rightmost control of a card's actions: Close for an open card, Release
// for a held one, Reopen for a closed one.
export function LifecyclePrimary({
  phase,
  life,
  off,
}: {
  phase: Phase;
  life: Lifecycle;
  off: string | null;
}) {
  if (phase === "open") {
    return (
      <button
        type="button"
        className="btn primary"
        disabled={off !== null}
        title={off ?? undefined}
        onClick={() => life.open("close")}
      >
        <CheckIcon />
        Close
      </button>
    );
  }
  const action = phase === "held" ? life.release : life.reopen;
  return (
    <button
      type="button"
      className="btn primary"
      disabled={action.pending || off !== null}
      title={off ?? undefined}
      onClick={action.trigger}
    >
      {phase === "held" ? <PlayIcon /> : <ReopenIcon />}
      {action.pending ? "Working..." : phase === "held" ? "Release" : "Reopen"}
    </button>
  );
}

// The overflow menu. On the card page it is "More" beside the primary; on an
// Inbox row it is the "..." button and carries the primary's move as well.
export function LifecycleMenu({
  phase,
  life,
  off,
  onEdit,
  compact = false,
}: {
  phase: Phase;
  life: Lifecycle;
  off: string | null;
  onEdit?: () => void;
  compact?: boolean;
}) {
  const edit = onEdit ?? (() => life.open("edit"));
  return (
    <Menu.Root>
      <Menu.Trigger
        className={compact ? "btn cm-dots" : "btn"}
        aria-label="More card actions"
        disabled={off !== null}
        title={off ?? undefined}
      >
        {compact ? (
          <DotsIcon />
        ) : (
          <>
            More
            <ChevronDownIcon className="icon" />
          </>
        )}
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner className="sel-pos" align="end" sideOffset={4}>
          <Menu.Popup
            className="cm-popup"
            aria-label="More card actions"
            // The menu is portalled, but React events still reach the parents
            // of the trigger: a click here must not reach an Inbox row.
            onClick={(event) => event.stopPropagation()}
          >
            {phase === "closed" ? (
              <Item icon={<ReopenIcon />} onSelect={life.reopen.trigger}>
                Reopen
              </Item>
            ) : (
              <>
                {phase === "held" ? (
                  compact ? (
                    <Item icon={<PlayIcon />} onSelect={life.release.trigger}>
                      Release hold
                    </Item>
                  ) : null
                ) : (
                  <Item icon={<PauseIcon />} onSelect={() => life.open("hold")}>
                    Hold...
                  </Item>
                )}
                {compact || phase === "held" ? (
                  <Item icon={<CheckIcon />} onSelect={() => life.open("close")}>
                    Close card...
                  </Item>
                ) : null}
                <Item icon={<EditIcon />} onSelect={edit}>
                  Edit
                </Item>
                <Menu.Separator className="cm-sep" />
                <Item icon={<CancelCardIcon />} bad onSelect={() => life.open("cancel")}>
                  Cancel card...
                </Item>
              </>
            )}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}

function Item({
  icon,
  bad,
  onSelect,
  children,
}: {
  icon: ReactNode;
  bad?: boolean;
  onSelect: () => void;
  children: ReactNode;
}) {
  return (
    <Menu.Item className={bad ? "cm-item bad" : "cm-item"} onClick={onSelect}>
      {icon}
      {children}
    </Menu.Item>
  );
}

// The dialog the owner opened, or nothing.
export function LifecycleDialogs({ card, life }: { card: BoardRow; life: Lifecycle }) {
  const close = useCallback(() => life.open(null), [life]);
  switch (life.dialog) {
    case "close":
      return <CloseDialog card={card} onClose={close} />;
    case "cancel":
      return <CancelDialog card={card} onClose={close} />;
    case "hold":
      return <HoldDialog card={card} onClose={close} />;
    case "edit":
      return <EditDialog card={card} onClose={close} />;
    default:
      return null;
  }
}

function Field({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="dlg-f">
      {htmlFor ? (
        <label className="dlg-l" htmlFor={htmlFor}>
          {label}
        </label>
      ) : (
        <span className="dlg-l">{label}</span>
      )}
      {children}
    </div>
  );
}

function CloseDialog({ card, onClose }: { card: BoardRow; onClose: () => void }) {
  const [choice, setChoice] = useState<CloseChoice | null>(null);
  const [reason, setReason] = useState("");
  const outcome = choice ? closeOutcome(card.kind, choice) : null;
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${card.id}/close`, {
        outcome,
        reason: reason.trim(),
        ...hostField(card),
        idempotency_key,
      }),
    [card, outcome, reason],
  );
  const action = useAction({
    binding: `${card.id}:close:${outcome}:${reason.trim()}`,
    send,
    onRecorded: onClose,
  });
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <ProjectDialog name="close-card" pending={action.pending} onClose={onClose} initial={keep}>
      <h2 id="close-card-dialog-title">Close {card.display_id}</h2>
      <p id="close-card-dialog-body">
        The card moves to Done with this outcome. You can reopen it later.
      </p>
      <Field label="Outcome">
        <Segmented
          label="Outcome"
          value={choice ?? ("" as CloseChoice)}
          onChange={setChoice}
          disabled={action.pending}
          options={[
            { value: "done", label: "Done" },
            { value: "wont", label: "Won't do" },
            { value: "duplicate", label: "Duplicate" },
          ]}
        />
      </Field>
      <Field label="Reason" htmlFor="close-reason">
        <textarea
          id="close-reason"
          className="note-in area"
          placeholder="What happened, or where the work is"
          value={reason}
          disabled={action.pending}
          onChange={(event) => setReason(event.target.value)}
        />
      </Field>
      <ActionFailure error={action.error} host={ownerOf(card)} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep open
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={outcome === null || reason.trim() === "" || action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Closing..." : "Close card"}
        </button>
      </div>
    </ProjectDialog>
  );
}

// The card Cancel card acts on: a board row, or a card known only by its
// display id, as a refused project delete names it.
export type CancelTarget = Owned & Pick<BoardRow, "id" | "display_id">;

// Cancel card's confirm. `reason` fills the field to start with, and
// `onCancelled` runs once the cancel is recorded, before the dialog closes.
export function CancelDialog({
  card,
  onClose,
  reason: initial = "",
  onCancelled,
}: {
  card: CancelTarget;
  onClose: () => void;
  reason?: string;
  onCancelled?: (reason: string) => void;
}) {
  const [reason, setReason] = useState(initial);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${card.id}/cancel`, {
        reason: reason.trim(),
        ...hostField(card),
        idempotency_key,
      }),
    [card, reason],
  );
  const recorded = useCallback(() => {
    onCancelled?.(reason.trim());
    onClose();
  }, [onCancelled, onClose, reason]);
  const action = useAction({
    binding: `${card.id}:cancel:${reason.trim()}`,
    send,
    onRecorded: recorded,
  });
  const keep = useRef<HTMLButtonElement>(null);
  return (
    <ProjectDialog name="cancel-card" pending={action.pending} onClose={onClose} initial={keep}>
      <h2 id="cancel-card-dialog-title">Cancel {card.display_id}?</h2>
      <p id="cancel-card-dialog-body">
        The card moves to Cancelled, its runs stop and no new run starts on it. Reopen brings it
        back.
      </p>
      <Field label="Reason" htmlFor="cancel-reason">
        <textarea
          id="cancel-reason"
          className="note-in area"
          placeholder="Why it is cancelled"
          value={reason}
          disabled={action.pending}
          onChange={(event) => setReason(event.target.value)}
        />
      </Field>
      <ActionFailure error={action.error} host={ownerOf(card)} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep card
        </button>
        <button
          type="button"
          className="btn danger"
          disabled={reason.trim() === "" || action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Cancelling..." : "Cancel card"}
        </button>
      </div>
    </ProjectDialog>
  );
}

function HoldDialog({ card, onClose }: { card: BoardRow; onClose: () => void }) {
  // Tomorrow morning, in Lisbon time, is the usual hold.
  const [date, setDate] = useState(() => lisbonDate(serverNow(), 1));
  const [time, setTime] = useState("09:00");
  const [reason, setReason] = useState("");
  const until = lisbonInstant(date, time);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("POST", `/api/v1/cards/${card.id}/hold`, {
        until,
        reason: reason.trim(),
        rev: card.rev,
        ...hostField(card),
        idempotency_key,
      }),
    [card, until, reason],
  );
  const action = useAction({
    binding: `${card.id}:hold:${until}:${reason.trim()}`,
    send,
    onRecorded: onClose,
  });
  const keep = useRef<HTMLButtonElement>(null);
  const past = until !== null && Date.parse(until) <= serverNow();
  return (
    <ProjectDialog name="hold-card" pending={action.pending} onClose={onClose} initial={keep}>
      <h2 id="hold-card-dialog-title">Hold {card.display_id}</h2>
      <p id="hold-card-dialog-body">
        A held card starts no new run until the time below. Release brings it back sooner.
      </p>
      <Field label="Until (Lisbon time)" htmlFor="hold-date">
        <div className="dlg-when">
          <input
            id="hold-date"
            type="date"
            className="note-in"
            value={date}
            disabled={action.pending}
            onChange={(event) => setDate(event.target.value)}
          />
          <input
            id="hold-time"
            type="time"
            aria-label="Time"
            className="note-in"
            value={time}
            disabled={action.pending}
            onChange={(event) => setTime(event.target.value)}
          />
        </div>
        {past ? <div className="dlg-hint">That time has passed. Pick a later one.</div> : null}
      </Field>
      <Field label="Reason" htmlFor="hold-reason">
        <textarea
          id="hold-reason"
          className="note-in area"
          placeholder="What the card waits for"
          value={reason}
          disabled={action.pending}
          onChange={(event) => setReason(event.target.value)}
        />
      </Field>
      <ActionFailure error={action.error} host={ownerOf(card)} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Not now
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={until === null || past || reason.trim() === "" || action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Holding..." : "Hold card"}
        </button>
      </div>
    </ProjectDialog>
  );
}

// Title and body as fields, Cancel on the left and Save on the right. Save
// sends the rev the page read: a card someone changed first answers with a
// conflict and the text stays as typed.
export function EditFields({
  card,
  rev,
  title: firstTitle,
  body: firstBody,
  titleRef,
  onDone,
}: {
  card: BoardRow;
  rev: number;
  title: string;
  body: string;
  titleRef?: RefObject<HTMLInputElement | null>;
  onDone: () => void;
}) {
  const [title, setTitle] = useState(firstTitle);
  const [body, setBody] = useState(firstBody);
  const titleChanged = title.trim() !== firstTitle;
  const bodyChanged = body.trim() !== firstBody.trim();
  const changed = titleChanged || bodyChanged;
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation("PATCH", `/api/v1/cards/${card.id}`, {
        rev,
        ...(titleChanged ? { title: title.trim() } : {}),
        ...(bodyChanged ? { body: body.trim() } : {}),
        ...hostField(card),
        idempotency_key,
      }),
    [card, rev, title, body, titleChanged, bodyChanged],
  );
  const action = useAction({
    binding: `${card.id}:${rev}:edit:${title.trim()}:${body.trim()}`,
    send,
    onRecorded: onDone,
  });
  // Only a field that changed is sent, so only its length can stop the save.
  // A card from before the limits keeps its long body while its title is
  // edited.
  const over =
    (titleChanged && isTooLong(title.trim(), LIMITS.cardTitle)) ||
    (bodyChanged && isTooLong(body.trim(), LIMITS.cardBody));
  return (
    <>
      <Field label="Title" htmlFor={`edit-title-${card.id}`}>
        <input
          id={`edit-title-${card.id}`}
          ref={titleRef}
          className="note-in edit-title"
          value={title}
          disabled={action.pending}
          onChange={(event) => setTitle(event.target.value)}
        />
      </Field>
      <Field label="Body" htmlFor={`edit-body-${card.id}`}>
        <textarea
          id={`edit-body-${card.id}`}
          className="note-in area edit-body"
          value={body}
          disabled={action.pending}
          onChange={(event) => setBody(event.target.value)}
        />
      </Field>
      {titleChanged ? <LimitNote what="title" text={title.trim()} limit={LIMITS.cardTitle} /> : null}
      {bodyChanged ? <LimitNote what="body" text={body.trim()} limit={LIMITS.cardBody} /> : null}
      <ActionFailure error={action.error} host={ownerOf(card)} />
      <div className="edit-acts">
        <button type="button" className="btn" disabled={action.pending} onClick={onDone}>
          Cancel
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={!changed || title.trim() === "" || over || action.pending}
          onClick={action.trigger}
        >
          {action.pending ? "Saving..." : "Save"}
        </button>
      </div>
    </>
  );
}

// Edit from the Inbox, which has no card page of its own to turn into
// fields: the same fields in a dialog, filled from the card's detail.
function EditDialog({ card, onClose }: { card: BoardRow; onClose: () => void }) {
  const detail = useQuery(cardQuery(card.id, ownerOf(card))).data;
  const title = useRef<HTMLInputElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  return (
    <ProjectDialog name="edit-card" pending={false} onClose={onClose} initial={title}>
      <h2 id="edit-card-dialog-title">Edit {card.display_id}</h2>
      {detail ? (
        <div id="edit-card-dialog-body">
          <EditFields
            card={card}
            rev={detail.card.rev}
            title={detail.card.title}
            body={detail.card.body ?? ""}
            titleRef={title}
            onDone={onClose}
          />
        </div>
      ) : (
        <>
          <p id="edit-card-dialog-body" role="status">
            Reading the card...
          </p>
          <div className="dialog-actions">
            <button ref={cancel} type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
          </div>
        </>
      )}
    </ProjectDialog>
  );
}

// An Inbox row's "..." menu: the card page's actions for the card the row is
// about, with the primary's move in the menu as well.
export function CardOverflow({ card, off }: { card: BoardRow; off: string | null }) {
  const life = useCardLifecycle(card);
  return (
    <>
      <LifecycleMenu compact phase={phaseOf(card)} life={life} off={off} />
      <LifecycleDialogs card={card} life={life} />
    </>
  );
}
