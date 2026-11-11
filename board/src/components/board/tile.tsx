import { Link, useSearch } from "@tanstack/react-router";

import type { AskSource, BoardRow } from "../../lib/api-types";
import { cardDot } from "./activity";
import type { DotKind } from "./activity";
import { HOST_PROJECT, hostName, ownerOf, shownId } from "../../lib/peers";
import { Ago } from "./ago";
import { badgeText, badgeTone, isPending, statusMark } from "./badge";
import { cardNumber } from "./format";
import { Glide } from "./glide";
import { AsOf, HostChip } from "./host-chip";
import { isTillerBug, isUntriaged } from "./bugs";
import { HelpIcon, PrIcon, RouteIcon, Status } from "./icons";
import { useAmbiguousIds, useHostLabel, useStanding } from "./peer-state";
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

// Where a routed Tiller bug came from: a project on the same host, or that
// host's own `fleet` project.
function FiledFrom({ row }: { row: BoardRow }) {
  const label = useHostLabel(row);
  const from = row.filed_from;
  if (!from || from === row.project_id || isUntriaged(row)) return null;
  return (
    <div className="c-from">
      <RouteIcon />
      <span>from {from === HOST_PROJECT ? hostName(label) : from}</span>
    </div>
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
  const untriaged = isUntriaged(row);
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
      data-attn={row.needs_you}
      data-pending={isPending(row)}
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
      <FiledFrom row={row} />
      <div className="c-chips">
        <HostChip entity={row} />
        {row.pr_number ? <PrChip number={row.pr_number} /> : null}
        {untriaged ? <span className="lab sm yel">Untriaged</span> : null}
        {isTillerBug(row) ? <span className="lab sm bad">Bug</span> : null}
        {untriaged && row.badge === "filed" ? null : (
          <span className={`lab sm ${tone}`}>
            {dot === "unknown" ? <HelpIcon /> : null}
            {badgeText(row)}
          </span>
        )}
      </div>
    </Link>
  );
}

// The chip an ask shows for where it came from. A prompt is what the owner
// typed in an agent's pane, which reads as chat.
const ASK_SOURCE: Record<AskSource, string> = {
  prompt: "chat",
  board: "board",
  script: "script",
  import: "import",
};

const UNTAGGED = "Waiting for the orchestrator to tag this ask";

// An ask that is not a card yet: what was asked, and why it is not a card. A
// peer's ask carries no text, so its tile says only that it waits, and its
// host chip says whose it is. Like a card, it dims with its lane or its link.
function AskTile({ ask, lane }: { ask: NonNullable<Tile["ask"]>; lane: LaneHealth["kind"] }) {
  const source = ask.source ? ASK_SOURCE[ask.source] : null;
  const summary = ask.summary || null;
  const standing = useStanding(ask);
  return (
    <div
      className={`card${lane === "disconnected" || standing.dim ? " dim" : ""}`}
      data-peer-state={standing.peer ? (standing.dim ? "stale" : "live") : undefined}
      title={summary ? `${summary}\n${UNTAGGED}` : UNTAGGED}
    >
      <div className="c-top">
        <Status mark="asked" />
        <span className="id">ask</span>
        {source ? <span className="lab sm">{source}</span> : null}
        {standing.dim ? (
          <span className="c-age">
            <AsOf asOf={standing.asOf} />
          </span>
        ) : null}
      </div>
      <div className="c-title">
        <span>{summary ?? UNTAGGED}</span>
      </div>
      {summary ? (
        <div className="c-from">
          <span>Waiting for the orchestrator to tag it</span>
        </div>
      ) : null}
      <div className="c-chips">
        <HostChip entity={ask} />
        <span className="lab sm">
          <Ago at={ask.received_at} prefix="asked" />
        </span>
      </div>
    </div>
  );
}

export function TileView({ tile, lane = "ok" }: { tile: Tile; lane?: LaneHealth["kind"] }) {
  if (tile.row) {
    return (
      <Glide row={tile.row}>
        <CardTile row={tile.row} lane={lane} />
      </Glide>
    );
  }
  if (tile.ask) return <AskTile ask={tile.ask} lane={lane} />;
  return null;
}

// In the folded Done column a card shrinks to its number.
export function MiniTile({ row }: { row: BoardRow }) {
  return (
    <Glide row={row}>
      <Link
        to="/"
        search={(prev) => ({ ...prev, card: row.id, owner: ownerOf(row) })}
        className="mini-card"
        title={`${row.host && row.link ? `${row.host} \u00b7 ` : ""}${row.display_id}: ${row.title}`}
      >
        {cardNumber(row.display_id)}
      </Link>
    </Glide>
  );
}
