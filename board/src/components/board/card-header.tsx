import { useState } from "react";
import type { ReactNode } from "react";

import type { BoardRow, CardDetail } from "../../lib/api-types";
import { hostField, ownerOf } from "../../lib/peers";
import { ActionFailure } from "../attention/action-failure";
import { ActionError, sendMutation } from "../attention/api";
import {
  EditFields,
  LifecycleDialogs,
  LifecycleFailure,
  LifecycleMenu,
  LifecyclePrimary,
  outcomeLabel,
  phaseOf,
  useCardLifecycle,
} from "../attention/card-lifecycle";
import { PRIORITIES } from "../attention/format";
import { PauseIcon } from "../attention/icons";
import { ulid } from "../attention/ulid";
import { CheckIcon } from "../shell/icons";
import { Select } from "../ui/select";
import { formatLisbonMoment } from "./format";

// Priority is a field edit with its own rev, so a stale one is a readable
// conflict. Picking a level saves it at once.
function PriorityPicker({ card, rev, off }: { card: BoardRow; rev: number; off: string | null }) {
  // The level just picked stands until the card's next rev arrives.
  const [picked, setPicked] = useState<{ rev: number; level: number } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);
  const shown = picked && picked.rev === rev ? picked.level : card.priority;
  const change = (next: string) => {
    const level = Number(next);
    if (level === shown || pending) return;
    setPending(true);
    setError(null);
    setPicked({ rev, level });
    sendMutation("PATCH", `/api/v1/cards/${card.id}`, {
      rev,
      priority: level,
      ...hostField(card),
      idempotency_key: ulid(),
    })
      .catch((caught: unknown) => {
        setPicked(null);
        setError(
          caught instanceof ActionError
            ? caught
            : new ActionError(0, "internal", "Something went wrong, so this was not saved."),
        );
      })
      .finally(() => setPending(false));
  };
  return (
    <>
      <Select
        label="Priority"
        value={String(shown)}
        disabled={pending || off !== null}
        title={off ?? undefined}
        onValueChange={change}
        options={PRIORITIES.map((name, value) => ({
          value: String(value),
          label: `P${value} ${name}`,
        }))}
      />
      <ActionFailure error={error} host={ownerOf(card)} />
    </>
  );
}

// The card at the top of the card page: its priority and actions, its title
// and body, and the details that belong to it. A held or closed card says so
// here, and swaps its primary action for the way back.
export function CardHeader({
  detail,
  off,
  actions,
  children,
}: {
  detail: CardDetail;
  // Why the actions are off, or null.
  off: string | null;
  // False for the standing Host health card, which cannot be closed or held.
  actions: boolean;
  children: ReactNode;
}) {
  const { card, column } = detail;
  const life = useCardLifecycle(column);
  const [editing, setEditing] = useState(false);
  const phase = phaseOf(column);
  const hold = detail.holds[0];

  if (editing) {
    return (
      <section className="ch-card" aria-label="Edit card">
        <EditFields
          card={column}
          rev={card.rev}
          title={card.title}
          body={card.body ?? ""}
          onDone={() => setEditing(false)}
        />
      </section>
    );
  }
  return (
    <>
      <section className="ch-card" aria-label="Card" data-testid="card-header">
        <div className="ch-top">
          <div className="ch-meta">
            {actions && phase !== "closed" ? (
              <PriorityPicker card={column} rev={card.rev} off={off} />
            ) : null}
          </div>
          {actions ? (
            <div className="ch-acts" data-testid="card-actions">
              {phase === "closed" ? null : (
                <LifecycleMenu
                  phase={phase}
                  life={life}
                  off={off}
                  onEdit={() => setEditing(true)}
                />
              )}
              <LifecyclePrimary phase={phase} life={life} off={off} />
            </div>
          ) : null}
          <h4>{card.title}</h4>
        </div>
        <LifecycleFailure card={column} life={life} />
        {card.body ? <p className="desc">{card.body}</p> : null}
        {phase === "held" ? (
          <div className="ch-note hold" data-testid="card-hold">
            <PauseIcon />
            <div>
              <b>Held{hold?.until_at ? ` until ${formatLisbonMoment(hold.until_at)}` : ""}.</b>
              {hold?.reason ? ` ${hold.reason}` : ""}
              {hold?.until_at ? (
                <div className="sub">It releases by itself at that time.</div>
              ) : null}
            </div>
          </div>
        ) : null}
        {phase === "closed" ? (
          <div className="ch-note closed" data-testid="card-closed">
            <CheckIcon />
            <div>
              <b>
                {card.outcome === "cancelled"
                  ? "Cancelled."
                  : `Closed: ${outcomeLabel(card.outcome)}.`}
              </b>
              {column.closed_at ? (
                <div className="sub">{formatLisbonMoment(column.closed_at)} (Lisbon).</div>
              ) : null}
            </div>
          </div>
        ) : null}
        {children}
      </section>
      <LifecycleDialogs card={column} life={life} />
    </>
  );
}
