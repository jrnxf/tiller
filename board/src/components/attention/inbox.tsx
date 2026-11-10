import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useMemo } from "react";
import type { CSSProperties, ReactNode } from "react";

import type { AttentionItem, BoardRow, PermissionPrompt, Project } from "../../lib/api-types";
import { focusReason, ownerKey, ownerOf, projectIndex, shownId } from "../../lib/peers";
import type { Owned } from "../../lib/peers";
import { prefsStore } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import { attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { HostChip } from "../board/host-chip";
import { useAmbiguousIds, useStanding, useNamedProjects } from "../board/peer-state";
import { answeredStore, withdrawnStore } from "./answered";
import type { Withdrawn } from "./answered";
import { AnswerForm } from "./answer";
import { ApproveBlock } from "./approve";
import { CardActions, OpenInHerdr } from "./card-actions";
import { CardOverflow } from "./card-lifecycle";
import { tidy } from "./ansi";
import { RecapCard } from "./recap";
import { KIND_LABELS, PRIORITIES, formatAge, formatWhen, parseEvidence, staleText } from "./format";
import type { ParsedEvidence } from "./format";
import {
  BackIcon,
  ChevronIcon,
  FileIcon,
  InfoIcon,
  LockIcon,
  NeedsIcon,
  OkIcon,
  RetiredIcon,
} from "./icons";
import { Terminal } from "./terminal";
import { WithdrawButton } from "./withdraw";
import "./inbox.css";

// One row of the inbox: a decision (an attention item) or a permission prompt.
// They are different kinds of thing and never share a list (state.md).
interface DecisionEntry {
  type: "decision";
  id: string;
  item: AttentionItem;
  answered: string | null;
  withdrawn: Withdrawn | null;
}

type Entry = DecisionEntry | { type: "permission"; id: string; prompt: PermissionPrompt };

function promptId(prompt: PermissionPrompt): string {
  return `perm-${prompt.session_id}`;
}

interface Context {
  cards: Map<string, BoardRow>;
  projects: Project[];
}

// The row that owns an entry: its card when the board still has it, else the
// entry itself, which carries the same host fields.
function ownerOfEntry(entry: Entry, ctx: Context): Owned {
  const own = entry.type === "decision" ? entry.item : entry.prompt;
  return ctx.cards.get(own.card_id) ?? own;
}

function Meta({
  cardId,
  owner,
  ctx,
  chip,
  when,
}: {
  cardId: string;
  owner: Owned;
  ctx: Context;
  chip: ReactNode;
  when: string;
}) {
  const card = ctx.cards.get(cardId);
  const ambiguous = useAmbiguousIds();
  const index = card ? projectIndex(ctx.projects, card) : -1;
  const project = ctx.projects[index];
  return (
    <div className="d-meta">
      {chip}
      <HostChip entity={owner} />
      {project ? (
        <>
          <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
          <span>{project.name}</span>
          <span className="sep">/</span>
        </>
      ) : null}
      <span>{card ? shownId(card, ambiguous) : cardId.slice(-7)}</span>
      <span className="sep">/</span>
      <span>{when}</span>
    </div>
  );
}

function BackLink() {
  return (
    <Link
      to="/inbox"
      search={({ host }) => ({ host })}
      className="btn ghost sm in-back"
      aria-label="Back to the inbox"
    >
      <BackIcon />
      Inbox
    </Link>
  );
}

// The question as the heading, with project names and ids set in mono.
function Question({ text, projects }: { text: string; projects: Project[] }) {
  const names = projects
    .map((project) => project.name)
    .filter((name) => name.length >= 3)
    .map((name) => `(?<![\\w-])${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w-])`);
  const pattern = new RegExp(`(${[...names, "\\b[0-9A-HJKMNP-TV-Z]{26}\\b"].join("|")})`, "g");
  return (
    <>
      {text.split(pattern).map((part, index) =>
        index % 2 === 1 ? (
          <code className="proj" key={index}>
            {part}
          </code>
        ) : (
          part
        ),
      )}
    </>
  );
}

function Evidence({
  item,
  evidence,
  ctx,
}: {
  item: AttentionItem;
  evidence: ParsedEvidence;
  ctx: Context;
}) {
  const sessionId = item.subject_session_id ?? evidence.sessionId;
  const card = ctx.cards.get(item.card_id);
  // The pane is only there to open while the session is live.
  const herdr =
    sessionId && item.still_current ? (
      <OpenInHerdr sessionId={sessionId} off={focusReason(ownerOf(card ?? item))} />
    ) : null;
  if (!evidence.paneTail && evidence.lines.length === 0) return null;
  return (
    <div className="blk">
      <div className="blk-h">
        Evidence
        {evidence.paneTail && evidence.lastOutputAt ? (
          <span className="hint">
            orchestrator screen, last seen {formatWhen(evidence.lastOutputAt)}
          </span>
        ) : null}
      </div>
      {evidence.paneTail ? (
        <Terminal tail={evidence.paneTail} sessionId={sessionId} herdr={herdr} />
      ) : null}
      {evidence.lines.length ? (
        <div className="evi">
          {evidence.lines.map((line, index) => (
            <div className="evi-row" key={index}>
              <FileIcon />
              {line.href ? (
                <a className="mono" href={line.href} target="_blank" rel="noreferrer noopener">
                  {line.text}
                </a>
              ) : (
                <span>{line.text}</span>
              )}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function Status({
  item,
  evidence,
  settled,
}: {
  item: AttentionItem;
  evidence: ParsedEvidence;
  settled: boolean;
}) {
  const standing = useStanding(item);
  if (settled) return null;
  if (!item.still_current) {
    const text = staleText(item.stale);
    return (
      <div className="stale" data-testid="not-current">
        <InfoIcon />
        <div className="txt">
          <div>
            <b>No longer applies.</b> {text.head}
          </div>
          <div className="sub">{text.sub}</div>
        </div>
        <WithdrawButton item={item} off={standing.canAct ? null : standing.reason} />
      </div>
    );
  }
  const last = evidence.lastOutputAt;
  return (
    <div className="waiting" data-testid="current">
      <span className="dot" />
      <span>Still waiting{last ? `, last output ${formatWhen(last)}` : ""}</span>
    </div>
  );
}

function DecisionDetail({
  item,
  answered,
  withdrawn,
  ctx,
}: {
  item: AttentionItem;
  answered: string | null;
  withdrawn: Withdrawn | null;
  ctx: Context;
}) {
  const evidence = useMemo(() => parseEvidence(item.evidence), [item.evidence]);
  const chip = (
    <span className="lab sm">
      <NeedsIcon />
      {KIND_LABELS[item.kind] ?? item.kind}
    </span>
  );
  const card = ctx.cards.get(item.card_id) ?? null;
  return (
    <>
      <BackLink />
      <Meta
        cardId={item.card_id}
        owner={item}
        ctx={ctx}
        chip={chip}
        when={`asked ${formatWhen(item.opened_at)} - ${formatAge(item.opened_at)} ago`}
      />
      <h2 className="d-q">
        <Question text={item.question} projects={ctx.projects} />
      </h2>
      <Status item={item} evidence={evidence} settled={answered !== null || withdrawn !== null} />
      {withdrawn ? (
        <div className="answered" data-testid="withdrawn">
          <CheckIcon />
          <div className="t">
            <b>Withdrawn.</b> {staleText(item.stale).head}
            <div className="sub">
              Withdrawn from the board by the owner, {formatWhen(withdrawn.at)}. Nobody is waiting
              on it.
            </div>
          </div>
        </div>
      ) : null}
      {item.consequence ? <p className="d-ctx">{item.consequence}</p> : null}
      <Evidence item={item} evidence={evidence} ctx={ctx} />
      {item.recommendation ? (
        <div className="blk">
          <div className="blk-h">Recommendation</div>
          <div className="rec">
            <span className="av lg">C</span>
            <div className="tx">
              <span className="who">The orchestrator recommends</span>
              {item.recommendation}
            </div>
          </div>
        </div>
      ) : null}
      <div className="blk">
        {withdrawn ? null : item.approval ? (
          item.still_current ? (
            <ApproveBlock item={item} />
          ) : null
        ) : (
          <AnswerForm item={item} />
        )}
      </div>
      <details className="card-det">
        <summary>
          <ChevronIcon />
          Card {card ? <span className="id">{card.display_id}</span> : null}
          {card ? <span className="ct">{card.title}</span> : null}
          {card ? (
            <span className="pill">
              P{card.priority} {PRIORITIES[card.priority]}
            </span>
          ) : null}
        </summary>
        <CardActions
          card={card}
          sessionId={tidy(evidence.paneTail ?? "") ? null : item.opened_by_session_id}
        />
      </details>
    </>
  );
}

function PermissionDetail({ prompt, ctx }: { prompt: PermissionPrompt; ctx: Context }) {
  const chip = (
    <span className="lab sm warn">
      <LockIcon />
      Permission prompt
    </span>
  );
  return (
    <>
      <BackLink />
      <Meta
        cardId={prompt.card_id}
        owner={ctx.cards.get(prompt.card_id) ?? prompt}
        ctx={ctx}
        chip={chip}
        when={`waiting since ${formatWhen(prompt.since)}, ${formatAge(prompt.since)}`}
      />
      <h2 className="d-q">Allow {prompt.tool}?</h2>
      <p className="d-ctx">
        A worker is stopped on a permission dialog in its pane. This is a keypress, not a decision:
        answer it in the pane.
      </p>
      <div className="blk">
        <div className="blk-h">What the pane is asking</div>
        <div className="pane-box">
          <div className="pb-h">
            <LockIcon />
            <span>{prompt.tool}</span>
            <span className="sp" />
            <span className="lab sm warn">waiting {formatAge(prompt.since)}</span>
          </div>
          <pre>{prompt.detail ?? "The command is not recorded yet."}</pre>
        </div>
      </div>
      <div className="acts">
        <OpenInHerdr
          sessionId={prompt.session_id}
          label="Open the pane in herdr"
          off={focusReason(ownerOf(ctx.cards.get(prompt.card_id) ?? prompt))}
        />
      </div>
      <CardActions card={ctx.cards.get(prompt.card_id) ?? null} sessionId={null} />
    </>
  );
}

function Row({ entry, selected, ctx }: { entry: Entry; selected: boolean; ctx: Context }) {
  const cardId = entry.type === "decision" ? entry.item.card_id : entry.prompt.card_id;
  const card = ctx.cards.get(cardId);
  const owner = ownerOfEntry(entry, ctx);
  const standing = useStanding(owner);
  const ambiguous = useAmbiguousIds();
  const index = card ? projectIndex(ctx.projects, card) : -1;
  const project = ctx.projects[index];
  const since = entry.type === "decision" ? entry.item.opened_at : entry.prompt.since;
  const answered = entry.type === "decision" && entry.answered !== null;
  const withdrawn = entry.type === "decision" && entry.withdrawn !== null;
  const gone = entry.type === "decision" && !entry.item.still_current && !answered;
  const title =
    entry.type === "decision"
      ? entry.item.question
      : `${entry.prompt.tool}: ${entry.prompt.detail ?? "waiting for a keypress"}`;
  const link = (
    <Link
      to="/inbox"
      search={({ host }) => ({ host, item: entry.id })}
      className={`irow${gone ? " gone" : ""}${answered ? " answered" : ""}${standing.dim ? " dim" : ""}`}
      // The router sets aria-current on the link that matches the URL exactly;
      // the highlight also covers the item shown by default.
      data-selected={selected}
      data-testid={`inbox-row-${entry.id}`}
    >
      {entry.type === "permission" ? (
        <span className="ii permic">
          <LockIcon />
        </span>
      ) : (
        <span className="ii">{answered ? <OkIcon /> : gone ? <RetiredIcon /> : <NeedsIcon />}</span>
      )}
      <span className="bd">
        <span className={`t${entry.type === "permission" ? " mono" : ""}`}>{title}</span>
        <span className="s">
          <HostChip entity={owner} />
          {project ? (
            <>
              <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
              {project.name}
              <span className="sl">/</span>
            </>
          ) : null}
          {card ? shownId(card, ambiguous) : cardId.slice(-7)}
          <span className="sl">/</span>
          {formatAge(since)}
          {answered ? <span className="lab sm ok">answered</span> : null}
          {withdrawn ? <span className="lab sm">withdrawn</span> : null}
          {entry.type === "decision" && entry.item.kind === "merge" && !answered ? (
            <span className="lab sm">merge</span>
          ) : null}
        </span>
      </span>
    </Link>
  );
  if (!card || answered || withdrawn || gone) return link;
  return (
    <div className="irow-wrap">
      {link}
      <span className="irow-menu">
        <CardOverflow card={card} off={standing.canAct ? null : standing.reason} />
      </span>
    </div>
  );
}

function Group({
  title,
  why,
  entries,
  empty,
  selected,
  ctx,
  perm,
}: {
  title: string;
  why: string;
  entries: Entry[];
  empty: string;
  selected: string | undefined;
  ctx: Context;
  perm?: boolean;
}) {
  return (
    <section aria-label={title}>
      <div className={`ghead${perm ? " perm" : ""}`}>
        {perm ? <LockIcon /> : <NeedsIcon />}
        {title}
        {/* Open items only: what is answered stays listed but no longer counts. */}
        <span className="c">
          {entries.filter((entry) => entry.type !== "decision" || entry.answered === null).length}
        </span>
        <span className="sp" />
        <span className="why">{why}</span>
      </div>
      {entries.length ? (
        entries.map((entry) => (
          <Row key={entry.id} entry={entry} selected={entry.id === selected} ctx={ctx} />
        ))
      ) : (
        <div className="irow empty">{empty}</div>
      )}
    </section>
  );
}

export function Inbox({
  selected,
  host,
}: {
  selected: string | undefined;
  host: string | undefined;
}) {
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  const answered = useStore(answeredStore);
  const withdrawn = useStore(withdrawnStore);

  const cards = new Map(board.rows.map((row) => [row.id, row]));
  const shown = new Set(
    scopedProjects(prefs, projects.projects, board.rows, host).map((project) =>
      ownerKey(project.id, project),
    ),
  );
  // Same scope as the sidebar badge: a card that left the board still counts.
  const inScope = (cardId: string) => {
    const row = cards.get(cardId);
    return !row || (shown.has(ownerKey(row.project_id, row)) && (!host || row.host_id === host));
  };
  const ctx: Context = { cards, projects: projects.projects };

  const openIds = new Set(attention.items.map((item) => item.id));
  const decisions: DecisionEntry[] = [
    ...attention.items.map((item) => ({
      type: "decision" as const,
      id: item.id,
      item,
      answered: answered[item.id]?.answer ?? null,
      withdrawn: withdrawn[item.id] ?? null,
    })),
    // Answered here: gone from the open queue, kept with its receipt.
    ...Object.values(answered)
      .filter((entry) => !openIds.has(entry.item.id))
      .map((entry) => ({
        type: "decision" as const,
        id: entry.item.id,
        item: entry.item,
        answered: entry.answer,
        withdrawn: null,
      })),
    // Withdrawn here: likewise kept, under No longer current.
    ...Object.values(withdrawn)
      .filter((entry) => !openIds.has(entry.item.id) && !answered[entry.item.id])
      .map((entry) => ({
        type: "decision" as const,
        id: entry.item.id,
        item: entry.item,
        answered: null,
        withdrawn: entry,
      })),
  ].filter((entry) => inScope(entry.item.card_id));
  // Oldest first: the item that has waited longest leads.
  decisions.sort((a, b) => a.item.opened_at.localeCompare(b.item.opened_at));
  const current = decisions.filter((entry) => entry.item.still_current || entry.answered !== null);
  const stale = decisions.filter((entry) => !entry.item.still_current && entry.answered === null);
  const prompts: Entry[] = attention.permission_prompts
    .filter((prompt) => inScope(prompt.card_id))
    .map((prompt) => ({ type: "permission", id: promptId(prompt), prompt }));

  const all = [...current, ...prompts, ...stale];
  const chosen = all.find((entry) => entry.id === selected) ?? (selected ? undefined : all[0]);

  return (
    <div className="inbox" data-pane={selected ? "detail" : "list"} data-testid="inbox">
      <div className="in-list">
        <RecapCard />
        <Group
          title="Decisions"
          why="your call"
          entries={current}
          empty="No decisions waiting."
          selected={chosen?.id}
          ctx={ctx}
        />
        <Group
          title="Permission prompts"
          why="a keypress in a pane"
          entries={prompts}
          empty="No prompts waiting."
          selected={chosen?.id}
          ctx={ctx}
          perm
        />
        {stale.length ? (
          <section aria-label="No longer current">
            <div className="ghead gone">
              <RetiredIcon />
              No longer current<span className="c">{stale.length}</span>
            </div>
            {stale.map((entry) => (
              <Row key={entry.id} entry={entry} selected={entry.id === chosen?.id} ctx={ctx} />
            ))}
          </section>
        ) : null}
      </div>
      <div className="in-detail">
        {chosen ? (
          <div className="inner" key={chosen.id}>
            {chosen.type === "decision" ? (
              <DecisionDetail
                item={chosen.item}
                answered={chosen.answered}
                withdrawn={chosen.withdrawn}
                ctx={ctx}
              />
            ) : (
              <PermissionDetail prompt={chosen.prompt} ctx={ctx} />
            )}
          </div>
        ) : (
          <div className="in-empty">
            <div>
              <b>{selected ? "Not in the inbox any more" : "Inbox zero"}</b>
              {selected
                ? "It was answered or withdrawn elsewhere."
                : "Nothing in the shown projects needs you."}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
