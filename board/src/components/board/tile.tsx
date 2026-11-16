import { useQuery } from "@tanstack/react-query";
import { Link, useSearch } from "@tanstack/react-router";

import type { AskSource, BoardRow, ReplyItem } from "../../lib/api-types";
import { repliesQuery } from "../../lib/queries";
import { cardDot } from "./activity";
import type { DotKind } from "./activity";
import { HOST_PROJECT, hostName, ownerOf, shownId } from "../../lib/peers";
import { Ago } from "./ago";
import { badgeText, badgeTone, isPending, statusMark } from "./badge";
import { cardNumber, formatLisbonMoment } from "./format";
import { Glide } from "./glide";
import { AsOf, HostChip } from "./host-chip";
import { isTillerBug, isUntriaged } from "./bugs";
import { HelpIcon, PrIcon, RouteIcon, Status } from "./icons";
import { useAmbiguousIds, useHostLabel, useStanding } from "./peer-state";
import {
  ANSWERED_LABEL,
  ANSWERED_TONE,
  isActivityBadge,
  overridesActivity,
  reportLabel,
  reportRole,
  reportTone,
  useAnsweredSince,
  useReport,
} from "./report";
import { STOPPED_LABEL, useAgentStopped } from "./stopped";
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

// Seconds after which a worker's report reads as old: the daemon's own wait
// before it asks the owner about a report nobody answered (R23, 5 min).
const REPORT_OLD_AFTER = 300;

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
  // What the card's worker last said. A report that the work stopped, or that
  // it is done, replaces a badge that only says what the terminal does.
  const report = useReport(row.id);
  const said =
    report && !row.needs_you && isActivityBadge(row.badge) && overridesActivity(report)
      ? report
      : null;
  // The owner answered after the worker stopped to ask: the ask is no longer
  // the owner's to act on, whatever the worker last said.
  const answered = useAnsweredSince(row.id, owner, said, row.needs_you);
  // An active run with no live agent: "Unknown" would hide that it stopped.
  const stopped = useAgentStopped(row, lane !== "disconnected" && !standing.dim);
  return (
    <Link
      to="/"
      search={(prev) => ({ ...prev, card: row.id, owner })}
      className={`card${lane === "disconnected" || standing.dim ? " dim" : ""}`}
      data-attn={row.needs_you}
      data-pending={isPending(row)}
      aria-current={selected ? "true" : undefined}
      data-peer-state={standing.peer ? (standing.dim ? "stale" : "live") : undefined}
      title={
        report
          ? `${id}: ${row.title}\n${reportRole(report)}, ${reportLabel(report).toLowerCase()}: ${report.text}${answered ? `\nYou answered at ${formatLisbonMoment(answered)}. The ${reportRole(report).toLowerCase()} has not reported since.` : ""}`
          : stopped
            ? `${id}: ${row.title}\nNo agent works on this card: its agent stopped before the work ended.`
            : `${id}: ${row.title}`
      }
      data-report={report?.state}
      data-answered={answered ? "true" : undefined}
      data-stopped={stopped || undefined}
    >
      <div className="c-top">
        <Status mark={statusMark(row)} />
        <span className="id">{id}</span>
        {standing.dim ? (
          <span className="c-age">
            <AsOf asOf={standing.asOf} />
          </span>
        ) : dot && row.observed_at && !stopped ? (
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
      {report ? (
        <div className="c-from c-report">
          <span>{report.text}</span>
          {report.at ? (
            // How old the report is, amber once it has waited as long as the
            // daemon waits before it asks the owner (recovery.md, R23).
            <span className="c-report-age">
              <Ago at={report.at} oldAfter={REPORT_OLD_AFTER} />
            </span>
          ) : null}
        </div>
      ) : null}
      <div className="c-chips">
        <HostChip entity={row} />
        {row.pr_number ? <PrChip number={row.pr_number} /> : null}
        {untriaged ? <span className="lab sm yel">Untriaged</span> : null}
        {isTillerBug(row) ? <span className="lab sm bad">Bug</span> : null}
        {stopped ? (
          <span className="lab sm bad">{STOPPED_LABEL}</span>
        ) : said && answered ? (
          <span className={`lab sm ${ANSWERED_TONE}`}>{ANSWERED_LABEL}</span>
        ) : said ? (
          <span className={`lab sm ${reportTone(said)}`}>{reportLabel(said)}</span>
        ) : untriaged && row.badge === "filed" ? null : (
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

// What an ask's tile says once its answer was read.
const READ_ANSWER = "Answered, and read";

// The first line of an answer that has text, as the tile's one line shows it.
function firstLine(text: string): string {
  return (
    text
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line !== "") ?? ""
  );
}

// The unread answer to an ask, when the orchestrator sent one. The Inbox and
// the sidebar read the same list, so this adds no request. A peer's comes from
// what its `act` link carries, as its ask's first line does.
function useAnswer(
  ask: NonNullable<Tile["ask"]>,
  owner: string | undefined,
): ReplyItem | undefined {
  const { data } = useQuery(repliesQuery(ask.project_id, owner));
  return data?.replies.find((reply) => reply.ask_id === ask.id);
}

// An ask that is not a card yet: what was asked, and what became of it. An
// ask with an unread answer says so and shows the answer's first line; one
// whose answer was read says when it came. Every ask tile opens the ask's
// panel, with its text, delivery and answer. A peer's ask reads the same as
// this host's on an `act` link; a `read` link carries no text, so its tile
// says where the text is. Like a card, it dims with its lane or its link.
function AskTile({ ask, lane }: { ask: NonNullable<Tile["ask"]>; lane: LaneHealth["kind"] }) {
  const source = ask.source ? ASK_SOURCE[ask.source] : null;
  const summary = ask.summary || null;
  const standing = useStanding(ask);
  const owner = ownerOf(ask);
  const answer = useAnswer(ask, owner);
  const answeredAt = answer?.replied_at ?? ask.answered_at ?? null;
  const textless = owner !== undefined && !summary && !answer;
  // Short, since the tile cuts this line at its width; the tooltip has room.
  const state = answer
    ? firstLine(answer.body)
    : answeredAt
      ? READ_ANSWER
      : textless
        ? `Its text is on ${owner}`
        : summary
          ? WAITING
          : null;
  const title = summary ?? answer?.question ?? (textless ? `Ask kept on ${owner}` : WAITING);
  const tip = answer
    ? `Answered: ${firstLine(answer.body)}\nOpen the ask to read the answer`
    : answeredAt
      ? `${READ_ANSWER}\nOpen the ask to read it again`
      : textless
        ? `${owner} is paired view only, so its ask text stays there`
        : WAITING;
  const selected = useSearch({ strict: false });
  const open = selected.ask === ask.id && (selected.owner ?? undefined) === owner;
  const className = `card${lane === "disconnected" || standing.dim ? " dim" : ""}`;
  const peerState = standing.peer ? (standing.dim ? "stale" : "live") : undefined;
  const hover = title === tip ? title : `${title}\n${tip}`;
  return (
    <Link
      to="/"
      search={(prev) => ({ ...prev, card: undefined, ask: ask.id, owner })}
      className={className}
      data-kind="ask"
      data-peer-state={peerState}
      data-answered={answeredAt ? "true" : undefined}
      data-unread={answer ? "true" : undefined}
      aria-current={open ? "true" : undefined}
      title={hover}
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
        <span>{title}</span>
      </div>
      {state ? (
        <div className="c-from">
          <span>{state}</span>
        </div>
      ) : null}
      <div className="c-chips">
        <HostChip entity={ask} />
        {answeredAt ? (
          <span className={`lab sm${answer ? " ok" : ""}`}>
            <Ago at={answeredAt} prefix="Answered" />
          </span>
        ) : (
          <span className="lab sm">
            <Ago at={ask.received_at} prefix="asked" />
          </span>
        )}
      </div>
    </Link>
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
