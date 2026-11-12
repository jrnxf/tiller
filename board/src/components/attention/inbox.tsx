import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useMemo } from "react";
import type { CSSProperties, ReactNode } from "react";

import type {
  AgentRow,
  AttentionItem,
  BoardRow,
  PermissionPrompt,
  Project,
  PromptDialog,
  ReplyItem,
} from "../../lib/api-types";
import { focusReason, ownerKey, ownerOf, projectIndex, shownId } from "../../lib/peers";
import type { Owned } from "../../lib/peers";
import { AWAY_BRIEF_ID, briefIsUnread, briefReadStore } from "../../lib/away";
import { prefsStore } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import {
  agentsQuery,
  attentionQuery,
  awayBriefQuery,
  boardQuery,
  projectsQuery,
} from "../../lib/queries";
import { dialogWaits, scopeTest, scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { CheckIcon } from "../shell/icons";
import { Ago } from "../board/ago";
import { AnswerStrip } from "../board/answer-strip";
import { HostChip } from "../board/host-chip";
import { ShieldIcon } from "../board/icons";
import { useAmbiguousIds, useStanding, useNamedProjects } from "../board/peer-state";
import { readPane, terminalTailPath } from "../board/terminal-stream";
import type { Tail } from "../board/terminal-stream";
import { AwayBriefDetail, AwayBriefRow } from "./away-brief";
import {
  answeredStore,
  dropStaleSent,
  markSent,
  openInbox,
  sentStore,
  withdrawnStore,
} from "./answered";
import type { Withdrawn } from "./answered";
import { AnswerForm } from "./answer";
import { ApproveBlock } from "./approve";
import { CardActions, HerdrMenu } from "./card-actions";
import { CardOverflow } from "./card-lifecycle";
import { tidy } from "./ansi";
import { RecapCard } from "./recap";
import { RepliesGroup, ReplyDetail, useReplies } from "./replies";
import {
  KIND_LABELS,
  PRIORITIES,
  formatAge,
  formatWhen,
  parseEvidence,
  recommender,
  staleText,
} from "./format";
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
import { ViewLive } from "./view-live";
import { agentName, useSavedMachine, useSessionAgent } from "./agent-name";
import { WithdrawButton } from "./withdraw";
import "./inbox.css";

// One row of the inbox: a decision (an attention item), a permission prompt,
// or an agent that waits on a dialog no hook asked for. Decisions and prompts
// are different kinds of thing and never share a list (state.md).
interface DecisionEntry {
  type: "decision";
  id: string;
  item: AttentionItem;
  answered: string | null;
  withdrawn: Withdrawn | null;
}

type Entry =
  | DecisionEntry
  | { type: "permission"; id: string; prompt: PermissionPrompt }
  | { type: "dialog"; id: string; agent: AgentRow };

// A row of the list: an entry, or a reply from the orchestrator.
type Shown =
  | { type: "entry"; id: string; entry: Entry }
  | { type: "reply"; id: string; reply: ReplyItem };

function promptId(prompt: PermissionPrompt): string {
  return `perm-${prompt.session_id}`;
}

function dialogId(agent: AgentRow): string {
  return `dialog-${agent.session_id}`;
}

// The card an entry is about; an orchestrator's dialog has none.
function cardOfEntry(entry: Entry): string | null {
  if (entry.type === "decision") return entry.item.card_id;
  return entry.type === "permission" ? entry.prompt.card_id : entry.agent.card_id;
}

// What names the wait an answer from here went to, as the answer strip names
// it (protocol.md 7.4, Prompt): a permission prompt by when it began, a dialog
// by the newest observation when the answer went, the strip's `seen`. A later
// observation while the agent still waits is the next wait.
function waitKey(entry: Entry): string | null {
  if (entry.type === "decision") return null;
  return entry.type === "permission" ? entry.prompt.since : entry.agent.activity_observed_at;
}

// When the entry began to wait on the owner.
function sinceOfEntry(entry: Entry): string {
  if (entry.type === "decision") return entry.item.opened_at;
  if (entry.type === "permission") return entry.prompt.since;
  return entry.agent.state_since ?? entry.agent.activity_observed_at;
}

interface Context {
  cards: Map<string, BoardRow>;
  projects: Project[];
}

// The row that owns an entry: its card when the board still has it, else the
// entry itself, which carries the same host fields.
function ownerOfEntry(entry: Entry, ctx: Context): Owned {
  const own =
    entry.type === "decision"
      ? entry.item
      : entry.type === "permission"
        ? entry.prompt
        : entry.agent;
  const cardId = cardOfEntry(entry);
  return (cardId === null ? undefined : ctx.cards.get(cardId)) ?? own;
}

// The id a row shows for its card: the card's own, the end of a card id the
// board does not have any more, or none for an entry with no card.
function cardLabel(
  cardId: string | null,
  ctx: Context,
  ambiguous: ReturnType<typeof useAmbiguousIds>,
): string | null {
  if (cardId === null) return null;
  const card = ctx.cards.get(cardId);
  return card ? shownId(card, ambiguous) : cardId.slice(-7);
}

function Meta({
  cardId,
  owner,
  ctx,
  chip,
  when,
}: {
  cardId: string | null;
  owner: Owned;
  ctx: Context;
  chip: ReactNode;
  when: string;
}) {
  const card = cardId === null ? undefined : ctx.cards.get(cardId);
  const ambiguous = useAmbiguousIds();
  const label = cardLabel(cardId, ctx, ambiguous);
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
      {label === null ? null : (
        <>
          <span>{label}</span>
          <span className="sep">/</span>
        </>
      )}
      <span>{when}</span>
    </div>
  );
}

function BackLink() {
  return (
    <Link
      to="/inbox"
      search={({ host }) => ({ host })}
      className="btn sm in-back"
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
  // The pane is only there to open while the session is live. A peer's
  // pane opens from its own host, so it has no live view here.
  const off = focusReason(ownerOf(card ?? item));
  const herdr =
    sessionId && item.still_current ? (
      <span className="ev-acts">
        <HerdrMenu sessionId={sessionId} off={off} />
        {off === null ? <ViewLive sessionId={sessionId} /> : null}
      </span>
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
      <div className="not-current" data-testid="not-current">
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
  const agents = useQuery(agentsQuery()).data?.orchestrators;
  const by = recommender(item.opened_by_session_id, agents ?? []);
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
            <span className="av lg">{by.initial}</span>
            <div className="tx">
              <span className="who">{by.text}</span>
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

// What the detail says for an agent on a saved machine: the board cannot
// type into its pane, and its live view is a read-only tail.
function onSaved(name: string, what: string, machine: string): string {
  return `${name} is stopped on ${what}. It runs on the saved machine ${machine}. Answer it in herdr on ${machine}.`;
}

function PermissionDetail({ prompt, ctx }: { prompt: PermissionPrompt; ctx: Context }) {
  const { agent, saved, known } = useSessionAgent(prompt.session_id);
  const owner = ctx.cards.get(prompt.card_id) ?? prompt;
  const peer = ownerOf(owner);
  const name = agentName(agent);
  const chip = (
    <span className="lab sm warn">
      <LockIcon />
      Permission prompt
    </span>
  );
  // The hook's record of the dialog: what shows when the screen gives no
  // buttons to press (a peer's pane, a saved machine's, or no dialog read).
  const asking = (
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
  );
  return (
    <>
      <BackLink />
      <div className="d-top">
        <Meta
          cardId={prompt.card_id}
          owner={owner}
          ctx={ctx}
          chip={chip}
          when={`waiting since ${formatWhen(prompt.since)}, ${formatAge(prompt.since)}`}
        />
        <HerdrMenu sessionId={prompt.session_id} off={focusReason(peer)} />
      </div>
      <h2 className="d-q">Allow {prompt.tool}?</h2>
      {peer ? (
        <>
          <p className="d-ctx">
            {name} is stopped on a permission dialog. {peer} owns its pane, so answer it from that
            host.
          </p>
          {asking}
        </>
      ) : saved ? (
        <>
          <p className="d-ctx">{onSaved(name, "a permission dialog", saved)}</p>
          {asking}
        </>
      ) : (
        <>
          <p className="d-ctx">
            {name} is stopped on a permission dialog. Press an answer, and the board types it in the
            pane.
          </p>
          {/* Until the agents list says which machine runs it, a read could go
              to a saved machine's pane, which the daemon refuses. */}
          {known ? (
            <AnswerStrip
              key={prompt.session_id}
              sessionId={prompt.session_id}
              waiting
              observedAt={prompt.since}
              fallback={asking}
              onSent={(receipt) => markSent(promptId(prompt), prompt.since, receipt)}
            />
          ) : (
            asking
          )}
        </>
      )}
      <CardActions card={ctx.cards.get(prompt.card_id) ?? null} sessionId={null} />
    </>
  );
}

// The pane's visible rows, read once, for a dialog the board cannot read.
function PaneShows({ sessionId, herdr }: { sessionId: string; herdr: ReactNode }) {
  const tail = useQuery({
    queryKey: ["dialog-pane", sessionId],
    queryFn: async ({ signal }) => {
      const result = await readPane<Tail>(terminalTailPath(sessionId), signal);
      if (!result.ok) throw new Error(result.detail);
      return { text: result.body.text, at: new Date().toISOString() };
    },
    staleTime: Infinity,
    gcTime: 0,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  return (
    <div className="blk">
      <div className="blk-h">
        What the pane shows
        {tail.data ? (
          <span className="hint">
            <Ago at={tail.data.at} prefix="read" />
          </span>
        ) : null}
      </div>
      {tail.data && tail.data.text.trim() !== "" ? (
        <Terminal tail={tail.data.text} sessionId={sessionId} herdr={herdr} />
      ) : null}
      {/* With nothing to show, the way to the live terminal stays. */}
      {tail.isError || tail.data?.text.trim() === "" ? (
        <div className="pane-box">
          <div className="pb-h">
            <span>Pane tail</span>
            {herdr}
          </div>
          <pre>
            {tail.isError
              ? `The pane could not be read: ${tail.error.message}`
              : "The pane shows nothing yet."}
          </pre>
        </div>
      ) : null}
    </div>
  );
}

// An agent that waits on a dialog no hook asked for (protocol.md 7.4,
// Prompt). A known layout gets its answer buttons; any other gets none, as
// the board never guesses, and shows what the pane shows instead.
function DialogDetail({ agent, ctx }: { agent: AgentRow; ctx: Context }) {
  const card = agent.card_id === null ? undefined : ctx.cards.get(agent.card_id);
  const owner = card ?? agent;
  const peer = ownerOf(owner);
  const off = focusReason(peer);
  const ambiguous = useAmbiguousIds();
  const saved = useSavedMachine(agent.host_id);
  const name = agentName(agent);
  const since = agent.state_since ?? agent.activity_observed_at;
  const chip = (
    <span className="lab sm warn">
      <ShieldIcon />
      Dialog
    </span>
  );
  const top = (menu: boolean) => (
    <div className="d-top">
      <Meta
        cardId={agent.card_id}
        owner={owner}
        ctx={ctx}
        chip={chip}
        when={`waiting since ${formatWhen(since)}, ${formatAge(since)}`}
      />
      {menu ? <HerdrMenu sessionId={agent.session_id} off={off} /> : null}
    </div>
  );
  const waits = <h2 className="d-q">{name} waits for an answer</h2>;
  const unknown = (
    <>
      {top(false)}
      {waits}
      {peer ? (
        <p className="d-ctx">
          {name} is stopped on a dialog. {peer} owns its pane, so answer it from that host.
        </p>
      ) : (
        <>
          <p className="d-ctx">
            {saved
              ? onSaved(name, "a dialog", saved)
              : "The board cannot read this dialog, so it shows no buttons. Look at the live terminal and answer there."}
          </p>
          <PaneShows
            sessionId={agent.session_id}
            herdr={
              <span className="ev-acts">
                <HerdrMenu sessionId={agent.session_id} off={off} />
                <ViewLive sessionId={agent.session_id} />
              </span>
            }
          />
        </>
      )}
    </>
  );
  const lead = (dialog: PromptDialog) => (
    <>
      {top(true)}
      {dialog.layout === "trust" ? (
        <>
          <h2 className="d-q">Trust this folder?</h2>
          <p className="d-ctx">
            Claude Code asks before it works in a folder it has not seen. Tiller records trust
            before each launch, so this means that step did not take.
          </p>
        </>
      ) : (
        waits
      )}
    </>
  );
  return (
    <>
      <BackLink />
      {peer || saved ? (
        unknown
      ) : (
        <AnswerStrip
          key={agent.session_id}
          sessionId={agent.session_id}
          waiting
          observedAt={agent.activity_observed_at}
          agent={{
            name,
            card: card ? shownId(card, ambiguous) : null,
            runId: agent.run_id ?? null,
          }}
          lead={lead}
          onSent={(receipt) => {
            const entry: Entry = { type: "dialog", id: dialogId(agent), agent };
            markSent(entry.id, waitKey(entry) ?? "", receipt);
          }}
          reading={
            <>
              {top(false)}
              {waits}
            </>
          }
          fallback={unknown}
        />
      )}
      <CardActions card={card ?? null} sessionId={null} />
    </>
  );
}

function Row({ entry, selected, ctx }: { entry: Entry; selected: boolean; ctx: Context }) {
  const cardId = cardOfEntry(entry);
  const card = cardId === null ? undefined : ctx.cards.get(cardId);
  const owner = ownerOfEntry(entry, ctx);
  const standing = useStanding(owner);
  const ambiguous = useAmbiguousIds();
  const label = cardLabel(cardId, ctx, ambiguous);
  const index = card ? projectIndex(ctx.projects, card) : -1;
  const project = ctx.projects[index];
  const since = sinceOfEntry(entry);
  const answered = entry.type === "decision" && entry.answered !== null;
  const withdrawn = entry.type === "decision" && entry.withdrawn !== null;
  const gone = entry.type === "decision" && !entry.item.still_current && !answered;
  const title =
    entry.type === "decision"
      ? entry.item.question
      : entry.type === "permission"
        ? `${entry.prompt.tool}: ${entry.prompt.detail ?? "waiting for a keypress"}`
        : `${agentName(entry.agent)} waits on a dialog`;
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
      ) : entry.type === "dialog" ? (
        <span className="ii permic">
          <ShieldIcon />
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
          {label === null ? null : (
            <>
              {label}
              <span className="sl">/</span>
            </>
          )}
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
  // A read of its own: a failed one leaves the rest working.
  const agents = useQuery(agentsQuery()).data;
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const prefs = useStore(prefsStore);
  const answered = useStore(answeredStore);
  const withdrawn = useStore(withdrawnStore);
  // The away brief is a read of its own: a failed one leaves the rest working.
  const awayBrief = useQuery(awayBriefQuery()).data?.brief;
  const briefRead = useStore(briefReadStore);
  const brief = briefIsUnread(awayBrief, briefRead) ? awayBrief : null;

  const cards = new Map(board.rows.map((row) => [row.id, row]));
  const scoped = scopedProjects(prefs, projects.projects, board.rows, host);
  const shown = new Set(scoped.map((project) => ownerKey(project.id, project)));
  const { replies, loading: repliesLoading } = useReplies(scoped);
  // Same scope as the sidebar badge: a card that left the board still counts.
  const scope = scopeTest(board.rows, shown, host);
  const inScope = scope.card;
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
  // The prompts group: permission prompts, then agents that wait on a dialog.
  const waiting = agents ? dialogWaits(agents) : [];
  const prompts: Entry[] = [
    ...attention.permission_prompts
      .filter((prompt) => inScope(prompt.card_id))
      .map((prompt) => ({ type: "permission" as const, id: promptId(prompt), prompt })),
    ...waiting
      .filter(scope.agent)
      .map((agent) => ({ type: "dialog" as const, id: dialogId(agent), agent })),
  ];

  // Every row in list order, so nothing chosen shows the first.
  const all: Shown[] = [
    ...[...current, ...prompts].map((entry) => ({ type: "entry" as const, id: entry.id, entry })),
    ...replies.map((reply) => ({ type: "reply" as const, id: reply.id, reply })),
    ...stale.map((entry) => ({ type: "entry" as const, id: entry.id, entry })),
  ];
  // The brief leads the list, so it is what shows when nothing is chosen.
  const sent = useStore(sentStore);
  const sentHere = selected ? sent[selected]?.receipt : undefined;
  // A prompt or dialog back for a later wait, in scope or not, drops the mark
  // of the one answered from here, so a later "Answered" never shows its
  // receipt. It runs after each render and changes the store only when a mark
  // is stale.
  const waits = [
    ...attention.permission_prompts.map((prompt): Entry => ({
      type: "permission",
      id: promptId(prompt),
      prompt,
    })),
    ...waiting.map((agent): Entry => ({ type: "dialog", id: dialogId(agent), agent })),
  ].map((entry) => ({ id: entry.id, since: waitKey(entry) ?? "" }));
  useEffect(() => dropStaleSent(waits));
  useEffect(openInbox, []);
  const briefChosen = brief !== null && (selected === AWAY_BRIEF_ID || !selected);
  const picked = briefChosen
    ? undefined
    : (all.find((row) => row.id === selected) ?? (selected ? undefined : all[0]));
  const chosen = picked?.type === "entry" ? picked.entry : undefined;
  const chosenReply = picked?.type === "reply" ? picked.reply : undefined;

  return (
    <div className="inbox" data-pane={selected ? "detail" : "list"} data-testid="inbox">
      <div className="in-list">
        {brief ? <AwayBriefRow brief={brief} selected={briefChosen} /> : null}
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
        <RepliesGroup replies={replies} selected={picked?.id} projects={ctx.projects} />
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
        {brief && briefChosen ? (
          <div className="inner" key={AWAY_BRIEF_ID}>
            <AwayBriefDetail brief={brief} ctx={ctx} />
          </div>
        ) : chosenReply ? (
          <div className="inner" key={chosenReply.id}>
            <ReplyDetail reply={chosenReply} projects={ctx.projects} back={<BackLink />} />
          </div>
        ) : chosen ? (
          <div className="inner" key={chosen.id}>
            {chosen.type === "decision" ? (
              <DecisionDetail
                item={chosen.item}
                answered={chosen.answered}
                withdrawn={chosen.withdrawn}
                ctx={ctx}
              />
            ) : chosen.type === "permission" ? (
              <PermissionDetail prompt={chosen.prompt} ctx={ctx} />
            ) : (
              <DialogDetail agent={chosen.agent} ctx={ctx} />
            )}
          </div>
        ) : selected && repliesLoading ? null : (
          <div className="in-empty">
            <div>
              <b>
                {selected ? (sentHere ? "Answered" : "Not in the inbox any more") : "Inbox zero"}
              </b>
              {selected
                ? sentHere
                  ? `Sent ${sentHere}. The agent went on, so it left the inbox.`
                  : "It was answered or withdrawn."
                : "Nothing in the shown projects needs you."}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
