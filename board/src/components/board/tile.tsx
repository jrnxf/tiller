import { Link } from "@tanstack/react-router";

import type { BoardRow } from "../../lib/api-types";
import { cardDot } from "./activity";
import { Ago } from "./ago";
import { badgeText, badgeTone, statusMark } from "./badge";
import { PrIcon, Status } from "./icons";
import type { Tile } from "./model";

const KIND_COLOR: Record<string, string> = {
  scout: "var(--blue)",
  ship: "var(--green)",
  ops: "var(--orange)",
  question: "var(--fg-4)",
};

export function KindLabel({ kind }: { kind: string }) {
  return (
    <span className="lab sm">
      <span className="dot" style={{ background: KIND_COLOR[kind] ?? "var(--fg-4)" }} />
      {kind}
    </span>
  );
}

const MODELS = new Set(["opus", "sonnet", "haiku"]);

export function ModelChip({ model }: { model: string | null | undefined }) {
  if (!model) return null;
  return <span className={`mchip${MODELS.has(model) ? ` ${model}` : ""}`}>{model}</span>;
}

function PrChip({ number }: { number: number }) {
  return (
    <span className="lab sm">
      <PrIcon />
      PR {number}
    </span>
  );
}

function CardTile({ row }: { row: BoardRow }) {
  const slim = row.column === "done";
  const dot = cardDot(row);
  const tone = badgeTone(row);
  const attention = row.column === "needs_you";
  return (
    <Link
      to="/cards/$cardId"
      params={{ cardId: row.id }}
      className={`card${slim ? " done slim" : ""}`}
      data-attn={attention}
      title={`${row.display_id}: ${row.title}`}
    >
      <div className="c-top">
        <Status mark={statusMark(row)} />
        <span className="id">{row.display_id}</span>
        {dot && row.observed_at && !slim ? (
          // The freshness moves up beside the id when the board is tight.
          <span className="c-obs">
            <i className={`ld ${dot}`} />
            <Ago at={row.observed_at} />
          </span>
        ) : null}
        {slim && row.pr_number ? <PrChip number={row.pr_number} /> : <KindLabel kind={row.kind} />}
      </div>
      <div className="c-title">
        <span>{row.title}</span>
      </div>
      {slim ? null : (
        <>
          <div className="c-chips">
            <ModelChip model={row.model} />
            {row.pr_number ? <PrChip number={row.pr_number} /> : null}
            <span className={`lab sm ${tone}`}>{badgeText(row)}</span>
          </div>
          {dot && row.observed_at ? (
            <div className="c-live">
              <i className={`ld ${dot}${dot === "live" ? " pulse" : ""}`} />
              <Ago at={row.observed_at} prefix="observed" oldAfter={300} />
            </div>
          ) : null}
        </>
      )}
    </Link>
  );
}

function AskTile({ ask }: { ask: NonNullable<Tile["ask"]> }) {
  return (
    <div className="card" title="An ask the orchestrator has not tagged yet">
      <div className="c-top">
        <Status mark="asked" />
        <span className="id">ask</span>
        <span className="lab sm">chat</span>
      </div>
      <div className="c-title">
        <span>Waiting for the orchestrator to tag this ask</span>
      </div>
      <div className="c-live">
        <Ago at={ask.received_at} prefix="asked" />
      </div>
    </div>
  );
}

export function TileView({ tile }: { tile: Tile }) {
  if (tile.row) return <CardTile row={tile.row} />;
  if (tile.ask) return <AskTile ask={tile.ask} />;
  return null;
}

// In the folded Done column a card shrinks to its number.
export function MiniTile({ row }: { row: BoardRow }) {
  return (
    <Link
      to="/cards/$cardId"
      params={{ cardId: row.id }}
      className="mini-card"
      title={`${row.display_id}: ${row.title}`}
    >
      {row.display_id.split("-").pop()}
    </Link>
  );
}
