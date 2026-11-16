import { useQuery } from "@tanstack/react-query";
import { Link, useSearch } from "@tanstack/react-router";

import type { AskSource, BoardRow, ReplyItem } from "../../lib/api-types";
import { repliesQuery } from "../../lib/queries";
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

// What an ask's tile says while no answer is here. An ask stays in Asked
// until the orchestrator files it or answers it (state.md, section 6).
const WAITING = "Waiting for the orchestrator";

// The first line of an answer that has text, as the tile's one line shows it.
function firstLine(text: string): string {
  return (
    text
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line !== "") ?? ""
  );
}

// The unread answer to one of this host's asks, when the orchestrator sent
// one. The Inbox and the sidebar read the same list, so this adds no request.
// A peer's replies are never federated, so a peer's ask has none here.
function useAnswer(ask: NonNullable<Tile["ask"]>, peer: boolean): ReplyItem | undefined {
  const { data } = useQuery({ ...repliesQuery(ask.project_id), enabled: !peer });
  return peer ? undefined : data?.replies.find((reply) => reply.ask_id === ask.id);
}

// An ask that is not a card yet: what was asked, and what became of it. An
// ask with an unread answer says so, shows the answer's first line, and opens
// that answer in the Inbox. A peer's ask carries no text and its answer stays
// on the peer, so its tile says where both are read. Like a card, it dims
// with its lane or its link.
function AskTile({ ask, lane }: { ask: NonNullable<Tile["ask"]>; lane: LaneHealth["kind"] }) {
  const source = ask.source ? ASK_SOURCE[ask.source] : null;
  const summary = ask.summary || null;
  const standing = useStanding(ask);
  const owner = ownerOf(ask);
  const answer = useAnswer(ask, owner !== undefined);
  // Short, since the tile cuts this line at its width; the tooltip has room.
  const state = answer
    ? firstLine(answer.body)
    : owner
      ? `Text and answer are on ${owner}`
      : summary
        ? WAITING
        : null;
  const title = summary ?? (owner ? `Ask kept on ${owner}` : answer ? answer.question : WAITING);
  const tip = answer
    ? `Answered: ${firstLine(answer.body)}\nOpen the answer in the Inbox`
    : owner
      ? `This ask's text and its answer show on the board of ${owner}, not here`
      : WAITING;
  const className = `card${lane === "disconnected" || standing.dim ? " dim" : ""}`;
  const peerState = standing.peer ? (standing.dim ? "stale" : "live") : undefined;
  const hover = title === tip ? title : `${title}\n${tip}`;
  const body = (
    <>
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
        <span>{title}</span>
      </div>
      {state ? (
        <div className="c-from">
          <span>{state}</span>
        </div>
      ) : null}
      <div className="c-chips">
        <HostChip entity={ask} />
        {answer ? (
          <span className="lab sm ok">
            <Ago at={answer.replied_at} prefix="Answered" />
          </span>
        ) : (
          <span className="lab sm">
            <Ago at={ask.received_at} prefix="asked" />
          </span>
        )}
      </div>
    </>
  );
  if (answer) {
    return (
      <Link
        to="/inbox"
        search={({ host }) => ({ host, item: answer.id })}
        className={className}
        data-peer-state={peerState}
        data-answered="true"
        title={hover}
      >
        {body}
      </Link>
    );
  }
  return (
    <div className={className} data-peer-state={peerState} title={hover}>
      {body}
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
