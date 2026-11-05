import { Link, useSearch } from "@tanstack/react-router";

import type { BoardRow } from "../../lib/api-types";
import { cardDot } from "./activity";
import type { DotKind } from "./activity";
import { ownerOf, shownId } from "../../lib/peers";
import { Ago } from "./ago";
import { badgeText, badgeTone, statusMark } from "./badge";
import { cardNumber } from "./format";
import { AsOf, HostChip } from "./host-chip";
import { HelpIcon, PrIcon, Status } from "./icons";
import { useAmbiguousIds, useStanding } from "./peer-state";
import type { Tile } from "./model";
import type { LaneHealth } from "../states/degraded";

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

// What a card's live line says when its lane cannot be trusted: the dot and
// wording drop to the lane's state (mocks, "Board, degraded").
function laneDot(dot: DotKind | null, lane: LaneHealth["kind"]): DotKind | null {
  if (!dot || lane === "ok") return dot;
  return lane === "stale" ? "stale" : "off";
}

const LANE_PREFIX = { ok: "", stale: "stale, observed", disconnected: "last known" };

// The card page reads a peer's card from that peer (`?host=` on the read).
export function ownerSearch(row: BoardRow) {
  const owner = ownerOf(row);
  return owner ? { owner } : undefined;
}

function CardTile({ row, lane }: { row: BoardRow; lane: LaneHealth["kind"] }) {
  const dot = laneDot(cardDot(row), lane);
  const tone = badgeTone(row);
  const attention = row.column === "needs_you";
  const standing = useStanding(row);
  const id = shownId(row, useAmbiguousIds());
  const owner = ownerOf(row);
  const { card: openId, owner: openOwner } = useSearch({ strict: false });
  const selected = openId === row.id && (openOwner ?? undefined) === owner;
  return (
    <Link
      to="/"
      search={(prev) => ({ ...prev, card: row.id, owner })}
      className={`card${lane === "disconnected" || standing.dim ? " dim" : ""}`}
      data-attn={attention}
      aria-current={selected ? "true" : undefined}
      data-peer-state={standing.peer ? (standing.dim ? "stale" : "live") : undefined}
      title={`${id}: ${row.title}`}
    >
      <div className="c-top">
        <Status mark={statusMark(row)} />
        <span className="id">{id}</span>
        {standing.dim ? (
          <span className="c-age">
            <AsOf asOf={standing.asOf} />
          </span>
        ) : dot && row.observed_at ? (
          <span className="c-age">
            <Ago
              at={row.observed_at}
              prefix={dot === "unknown" ? "unknown, seen" : LANE_PREFIX[lane]}
              oldAfter={300}
            />
          </span>
        ) : null}
      </div>
      <div className="c-title">
        <span>{row.title}</span>
      </div>
      <div className="c-chips">
        <HostChip entity={row} />
        {row.pr_number ? <PrChip number={row.pr_number} /> : null}
        <span className={`lab sm ${tone}`}>
          {dot === "unknown" ? <HelpIcon /> : null}
          {badgeText(row)}
        </span>
      </div>
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
      <div className="c-chips">
        <span className="lab sm">
          <Ago at={ask.received_at} prefix="asked" />
        </span>
      </div>
    </div>
  );
}

export function TileView({ tile, lane = "ok" }: { tile: Tile; lane?: LaneHealth["kind"] }) {
  if (tile.row) return <CardTile row={tile.row} lane={lane} />;
  if (tile.ask) return <AskTile ask={tile.ask} />;
  return null;
}

// In the folded Done column a card shrinks to its number.
export function MiniTile({ row }: { row: BoardRow }) {
  return (
    <Link
      to="/"
      search={(prev) => ({ ...prev, card: row.id, owner: ownerOf(row) })}
      className="mini-card"
      title={`${row.host && row.link ? `${row.host} \u00b7 ` : ""}${row.display_id}: ${row.title}`}
    >
      {cardNumber(row.display_id)}
    </Link>
  );
}
