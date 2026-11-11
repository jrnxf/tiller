import { Popover } from "@base-ui/react/popover";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback, useId, useRef, useState } from "react";

import type { BoardRow, CardDep, CardDetail } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { boardQuery, cardQuery } from "../../lib/queries";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { ulid } from "../attention/ulid";
import { CloseIcon, PlusIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { depStateText, statusMark } from "./badge";
import { Status } from "./icons";

const FAILED = "Something went wrong, so this was not saved.";

function failureOf(caught: unknown): ActionError {
  return caught instanceof ActionError ? caught : new ActionError(0, "internal", FAILED);
}

function idNumber(row: BoardRow): number {
  return Number(/(\d+)$/.exec(row.display_id)?.[1] ?? 0);
}

// Display ids sort by their number, so gamma-9 comes before gamma-10.
function byNumber(a: BoardRow, b: BoardRow): number {
  return idNumber(a) - idNumber(b) || a.display_id.localeCompare(b.display_id);
}

function DepChip({
  dep,
  owner,
  onRemove,
  off,
  pending,
}: {
  dep: CardDep;
  owner?: string;
  // Unset when the chip cannot be removed: the card is closed.
  onRemove?: () => void;
  off: string | null;
  pending: boolean;
}) {
  const place = { column: dep.column ?? "queued", badge: dep.badge ?? "" };
  return (
    <span className="dep-chip" data-testid="dep-chip" title={dep.title}>
      <Status mark={statusMark(place)} />
      <Link to="/cards/$cardId" params={{ cardId: dep.display_id }} search={{ owner }}>
        {dep.display_id}
      </Link>
      <span className="s">{dep.column ? depStateText(place) : null}</span>
      {onRemove ? (
        <button
          type="button"
          className="dep-x"
          aria-label={`Remove ${dep.display_id}`}
          title={off ?? `Remove ${dep.display_id}`}
          disabled={off !== null || pending}
          onClick={onRemove}
        >
          <CloseIcon />
        </button>
      ) : null}
    </span>
  );
}

// The cards this one may wait on: open cards of the same project on its host,
// less the card itself and the cards it already waits on. A card that waits on
// this one is listed but cannot be picked, since adding it would make a loop.
function candidatesFor(detail: CardDetail, rows: BoardRow[]) {
  const { card } = detail;
  const open = new Set(
    detail.deps.filter((dep) => dep.released_at === null).map((dep) => dep.display_id),
  );
  const loops = new Set(detail.waiting_on_this ?? []);
  const kept = rows.filter(
    (row) =>
      ownerOf(row) === ownerOf(detail.column) &&
      row.project_id === card.project_id &&
      row.column !== "done" &&
      row.id !== card.id &&
      !open.has(row.display_id),
  );
  kept.sort(byNumber);
  return kept.map((row) => ({ row, loop: loops.has(row.display_id) }));
}

function DepPicker({
  detail,
  off,
  pending,
  onPick,
}: {
  detail: CardDetail;
  off: string | null;
  pending: boolean;
  onPick: (row: BoardRow) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listId = useId();
  const rows = useQuery({ ...boardQuery(), enabled: open }).data?.rows ?? [];
  const all = candidatesFor(detail, rows);
  const needle = query.trim().toLowerCase();
  const shown = all.filter(
    ({ row }) =>
      needle === "" ||
      row.display_id.toLowerCase().includes(needle) ||
      row.title.toLowerCase().includes(needle),
  );
  const pickable = shown.filter((option) => !option.loop);
  const active = pickable[Math.min(index, pickable.length - 1)];
  const close = () => {
    setOpen(false);
    setQuery("");
    setIndex(0);
  };
  const pick = (row: BoardRow) => {
    close();
    onPick(row);
  };

  return (
    <Popover.Root open={open} onOpenChange={(next) => (next ? setOpen(true) : close())}>
      {/* A disabled button takes no pointer, so the reason sits on its wrapper. */}
      <span className="dep-add" title={off ?? undefined}>
        <Popover.Trigger
          className="btn sm"
          disabled={off !== null || pending}
          aria-haspopup="listbox"
        >
          <PlusIcon />
          Add
        </Popover.Trigger>
      </span>
      <Popover.Portal>
        <Popover.Positioner className="sel-pos" side="bottom" align="end" sideOffset={4}>
          <Popover.Popup
            className="sel-popup dep-pop"
            aria-label="Pick a card"
            initialFocus={inputRef}
          >
            <input
              ref={inputRef}
              className="note-in dep-find"
              role="combobox"
              aria-label="Find a card"
              aria-expanded="true"
              aria-controls={listId}
              aria-activedescendant={active ? `${listId}-${active.row.id}` : undefined}
              placeholder="Find a card"
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setIndex(0);
              }}
              onKeyDown={(event) => {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  setIndex(Math.min(index + 1, pickable.length - 1));
                } else if (event.key === "ArrowUp") {
                  event.preventDefault();
                  setIndex(Math.max(index - 1, 0));
                } else if (event.key === "Enter") {
                  event.preventDefault();
                  if (active) pick(active.row);
                }
              }}
            />
            <ul className="dep-list" id={listId} role="listbox" aria-label="Cards">
              {shown.map(({ row, loop }) => (
                <li
                  key={row.id}
                  id={`${listId}-${row.id}`}
                  className="sel-item dep-opt"
                  role="option"
                  aria-selected={row.id === active?.row.id}
                  aria-disabled={loop || undefined}
                  data-highlighted={row.id === active?.row.id ? "" : undefined}
                  data-disabled={loop ? "" : undefined}
                  onMouseMove={() => {
                    if (!loop) setIndex(pickable.findIndex((option) => option.row.id === row.id));
                  }}
                  onClick={() => {
                    if (!loop) pick(row);
                  }}
                >
                  <span className="mono">{row.display_id}</span>
                  <span className="tt">{row.title}</span>
                </li>
              ))}
            </ul>
            {shown.length === 0 ? (
              <p className="dep-hint">
                {all.length === 0 ? "No other open card in this project." : "No card matches."}
              </p>
            ) : null}
            {shown
              .filter((option) => option.loop)
              .map(({ row }) => (
                <p key={row.id} className="dep-hint">
                  {row.display_id} depends on this card. Adding it would make a loop.
                </p>
              ))}
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
  );
}

// Remove is destructive, so it asks first. Keep is focused, so Enter keeps.
function RemoveDep({
  detail,
  dep,
  onRemoved,
  onClose,
}: {
  detail: CardDetail;
  dep: CardDep;
  onRemoved: () => void;
  onClose: () => void;
}) {
  const { card, column } = detail;
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<CardDep | null>("DELETE", `/api/v1/cards/${card.id}/deps`, {
        depends_on: dep.display_id,
        ...hostField(column),
        idempotency_key,
      }),
    [card.id, column, dep.display_id],
  );
  // Each dialog is one press: its key is kept for a retry after a failure.
  const [press] = useState(() => `${card.id}:dep-rm:${dep.display_id}:${Date.now()}`);
  const action = useAction({ binding: press, send, onRecorded: onRemoved });
  const keep = useRef<HTMLButtonElement>(null);
  const released = dep.released_at !== null;
  return (
    <Modal
      className="dialog"
      labelledBy="dep-rm-title"
      describedBy="dep-rm-body"
      dismissable={!action.pending}
      onClose={onClose}
      initialFocus={keep}
    >
      <h2 id="dep-rm-title">Remove {dep.display_id}?</h2>
      <p id="dep-rm-body">
        {released
          ? `${card.display_id} no longer waits on ${dep.display_id}. Remove takes it off this card.`
          : `${card.display_id} stops waiting on ${dep.display_id}, so it can start before ${dep.display_id} is done.`}
      </p>
      <ActionFailure error={action.error} host={ownerOf(column)} />
      <div className="dialog-actions">
        <button
          ref={keep}
          type="button"
          className="btn"
          disabled={action.pending}
          onClick={onClose}
        >
          Keep
        </button>
        <button
          type="button"
          className="btn danger"
          disabled={action.pending}
          onClick={action.trigger}
        >
          Remove
        </button>
      </div>
    </Modal>
  );
}

// The Depends on row of a card's details: a chip per card it waits on, or has
// waited on, with an x that asks to remove it, and Add (state.md, `dep.add` and
// `dep.release`). A removed dependency leaves the row. The core refuses a loop,
// a closed card and an unknown one; the picker turns the loops off first.
export function DependsOn({
  detail,
  cardId,
  owner,
  off,
  closed,
}: {
  detail: CardDetail;
  // The name the page read the card by, for the read to refresh.
  cardId: string;
  owner?: string;
  // Why the controls are off, or null.
  off: string | null;
  // A closed card waits on nothing more: its chips are read-only.
  closed: boolean;
}) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);
  const [removing, setRemoving] = useState<CardDep | null>(null);
  const { card, column } = detail;
  // Open dependencies come first.
  const shown = detail.deps.filter((dep) => dep.release_reason !== "removed");
  shown.sort((a, b) => Number(a.released_at !== null) - Number(b.released_at !== null));

  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: cardQuery(cardId, owner).queryKey });
  const add = (dependsOn: string) => {
    setPending(true);
    setError(null);
    sendMutation("POST", `/api/v1/cards/${card.id}/deps`, {
      depends_on: dependsOn,
      ...hostField(column),
      idempotency_key: ulid(),
    })
      .then(refresh)
      .catch((caught: unknown) => setError(failureOf(caught)))
      .finally(() => setPending(false));
  };

  return (
    <>
      <dt className="dep-dt">Depends on</dt>
      <dd className="dep-dd" data-testid="depends-on">
        <span className="dep-chips">
          {shown.length === 0 ? <span className="muted">None</span> : null}
          {shown.map((dep) => (
            <DepChip
              key={dep.display_id}
              dep={dep}
              owner={owner}
              off={off}
              pending={pending}
              onRemove={closed ? undefined : () => setRemoving(dep)}
            />
          ))}
        </span>
        {closed ? null : (
          <DepPicker
            detail={detail}
            off={off}
            pending={pending}
            onPick={(row) => add(row.display_id)}
          />
        )}
        {error ? (
          <span className="dep-error">
            <ActionFailure error={error} host={ownerOf(column)} />
          </span>
        ) : null}
      </dd>
      {removing ? (
        <RemoveDep
          detail={detail}
          dep={removing}
          onClose={() => setRemoving(null)}
          onRemoved={() => {
            setRemoving(null);
            void refresh();
          }}
        />
      ) : null}
    </>
  );
}
