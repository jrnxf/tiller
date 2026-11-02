import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { CSSProperties, ReactNode } from "react";

import type { AttentionItem, BoardRow, PermissionPrompt, Project } from "../../lib/api-types";
import { prefsStore } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import { attentionQuery, boardQuery, projectsQuery } from "../../lib/queries";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { answeredStore } from "./answered";
import { AnswerForm } from "./answer";
import { ApproveBlock } from "./approve";
import { CardActions, OpenInHerdr } from "./card-actions";
import { KIND_LABELS, evidenceLines, formatAge, formatWhen } from "./format";
import { BackIcon, FileIcon, InfoIcon, LockIcon, NeedsIcon, OkIcon, RetiredIcon } from "./icons";
import "./inbox.css";

// One row of the inbox: a decision (an attention item) or a permission prompt.
// They are different kinds of thing and never share a list (state.md).
interface DecisionEntry {
  type: "decision";
  id: string;
  item: AttentionItem;
  answered: string | null;
}

type Entry = DecisionEntry | { type: "permission"; id: string; prompt: PermissionPrompt };

function promptId(prompt: PermissionPrompt): string {
  return `perm-${prompt.session_id}`;
}

interface Context {
  cards: Map<string, BoardRow>;
  projects: Project[];
}

function Meta({
  cardId,
  ctx,
  chip,
  when,
}: {
  cardId: string;
  ctx: Context;
  chip: ReactNode;
  when: string;
}) {
  const card = ctx.cards.get(cardId);
  const index = ctx.projects.findIndex((project) => project.id === card?.project_id);
  const project = ctx.projects[index];
  return (
    <div className="d-meta">
      {chip}
      {project ? (
        <>
          <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
          <span>{project.name}</span>
          <span className="sep">/</span>
        </>
      ) : null}
      <span>{card?.display_id ?? cardId.slice(-7)}</span>
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

function Evidence({ evidence }: { evidence: string | null }) {
  const lines = evidenceLines(evidence);
  if (lines.length === 0) return null;
  return (
    <div className="blk">
      <div className="blk-h">
        Evidence<span className="c">{lines.length}</span>
      </div>
      <div className="evi">
        {lines.map((line, index) => (
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
    </div>
  );
}

function Current({ item, answered }: { item: AttentionItem; answered: string | null }) {
  if (answered !== null) {
    return (
      <div className="cur">
        <CheckIcon />
        <span>
          <b>Answered.</b> You answered from this page; the receipt below follows it.
        </span>
      </div>
    );
  }
  if (!item.still_current) {
    return (
      <div className="cur old" data-testid="not-current">
        <InfoIcon />
        <span>
          <b>No longer current.</b> The run that asked this has ended or the card moved on. An
          answer is still recorded, but it may change nothing.
        </span>
      </div>
    );
  }
  return (
    <div className="cur" data-testid="current">
      <CheckIcon />
      <span>
        <b>Current.</b> Still open, and the run that asked is still waiting.
      </span>
    </div>
  );
}

function DecisionDetail({
  item,
  answered,
  ctx,
}: {
  item: AttentionItem;
  answered: string | null;
  ctx: Context;
}) {
  const chip = (
    <span className="lab sm">
      <NeedsIcon />
      {KIND_LABELS[item.kind] ?? item.kind}
    </span>
  );
  return (
    <>
      <BackLink />
      <Meta
        cardId={item.card_id}
        ctx={ctx}
        chip={chip}
        when={`asked ${formatWhen(item.opened_at)}, ${formatAge(item.opened_at)} ago`}
      />
      <h2 className="d-q">{item.question}</h2>
      {item.consequence ? <p className="d-ctx">{item.consequence}</p> : null}
      <Current item={item} answered={answered} />
      <Evidence evidence={item.evidence} />
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
      {item.approval ? <ApproveBlock item={item} /> : <AnswerForm item={item} />}
      <CardActions
        card={ctx.cards.get(item.card_id) ?? null}
        sessionId={item.opened_by_session_id}
      />
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
        <OpenInHerdr sessionId={prompt.session_id} label="Open the pane in herdr" />
      </div>
      <CardActions card={ctx.cards.get(prompt.card_id) ?? null} sessionId={null} />
    </>
  );
}

function Row({ entry, selected, ctx }: { entry: Entry; selected: boolean; ctx: Context }) {
  const cardId = entry.type === "decision" ? entry.item.card_id : entry.prompt.card_id;
  const card = ctx.cards.get(cardId);
  const index = ctx.projects.findIndex((project) => project.id === card?.project_id);
  const project = ctx.projects[index];
  const since = entry.type === "decision" ? entry.item.opened_at : entry.prompt.since;
  const answered = entry.type === "decision" && entry.answered !== null;
  const gone = entry.type === "decision" && !entry.item.still_current && !answered;
  const title =
    entry.type === "decision"
      ? entry.item.question
      : `${entry.prompt.tool}: ${entry.prompt.detail ?? "waiting for a keypress"}`;
  return (
    <Link
      to="/inbox"
      search={({ host }) => ({ host, item: entry.id })}
      className={`irow${gone ? " gone" : ""}${answered ? " answered" : ""}`}
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
          {project ? (
            <>
              <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
              {project.name}
              <span className="sl">/</span>
            </>
          ) : null}
          {card?.display_id ?? cardId.slice(-7)}
          <span className="sl">/</span>
          {formatAge(since)}
          {answered ? <span className="lab sm ok">answered</span> : null}
          {entry.type === "decision" && entry.item.kind === "merge" && !answered ? (
            <span className="lab sm">merge</span>
          ) : null}
        </span>
      </span>
    </Link>
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
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const prefs = useStore(prefsStore);
  const answered = useStore(answeredStore);

  const cards = new Map(board.rows.map((row) => [row.id, row]));
  const shown = new Set(
    scopedProjects(prefs, projects.projects, board.rows, host).map((project) => project.id),
  );
  // Same scope as the sidebar badge: a card that left the board still counts.
  const inScope = (cardId: string) => {
    const row = cards.get(cardId);
    return !row || (shown.has(row.project_id) && (!host || row.host_id === host));
  };
  const ctx: Context = { cards, projects: projects.projects };

  const openIds = new Set(attention.items.map((item) => item.id));
  const decisions: DecisionEntry[] = [
    ...attention.items.map((item) => ({
      type: "decision" as const,
      id: item.id,
      item,
      answered: answered[item.id]?.answer ?? null,
    })),
    // Answered here: gone from the open queue, kept with its receipt.
    ...Object.values(answered)
      .filter((entry) => !openIds.has(entry.item.id))
      .map((entry) => ({
        type: "decision" as const,
        id: entry.item.id,
        item: entry.item,
        answered: entry.answer,
      })),
  ].filter((entry) => inScope(entry.item.card_id));
  // Oldest first, as the Needs you column sorts (state.md, section 6).
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
              <DecisionDetail item={chosen.item} answered={chosen.answered} ctx={ctx} />
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
