import { Link } from "@tanstack/react-router";
import type {
  AttentionItem,
  ThreadActivity,
  ThreadAsk,
  ThreadCard,
  ThreadCheckpoint,
  ThreadDecision,
  ThreadPr,
  ThreadReply,
  ThreadStep,
} from "../../lib/api-types";
import { formatLisbonDay, formatLisbonNear } from "../../lib/clock";
import { untagged } from "../../lib/orchestrator-text";
import { useStore } from "../../lib/store";
import { AnswerForm } from "../attention/answer";
import { answeredStore } from "../attention/answered";
import { useApprovalPr } from "../attention/approval-facts";
import { ApproveActions } from "../attention/approve";
import { KIND_LABELS } from "../attention/format";
import { OkIcon } from "../attention/icons";
import { Ago } from "../board/ago";
import { ChevronDownIcon, PrIcon, TerminalIcon } from "../board/icons";
import { showToast } from "../board/toast";
import { CheckIcon, InboxIcon } from "../shell/icons";
import {
  CopyIcon,
  DoneIcon,
  DotIcon,
  FailedIcon,
  ReplyToIcon,
  RestartIcon,
  StepsIcon,
} from "./icons";
import { Markdown } from "./markdown-view";
import { activityText, decisionText, stepFailed, stepTitle, undelivered } from "./model";

// "18:02" today, "Yesterday 18:02", "Tue 18:02" this week, "Thu 12 Nov, 18:00"
// before it: Lisbon time.
function stamp(iso: string | null): string | null {
  return formatLisbonNear(iso);
}

// A time as the chat shows it, with the full date when the pointer is on it.
export function Stamp({ at, className }: { at: string | null; className?: string }) {
  const shown = stamp(at);
  if (!shown || !at) return null;
  return (
    <time className={className} dateTime={at} title={formatLisbonDay(at) ?? undefined}>
      {shown}
    </time>
  );
}

// How the orchestrator tied a message to a card: it filed the card for it, or
// took it as a follow-up on one. A card the owner linked with # says nothing.
const RELATION: Record<string, string> = { created: "Filed", followup: "Follow-up on" };

function CardRef({
  card,
  owner,
}: {
  card: ThreadCard & { relation?: string | null };
  owner: string | undefined;
}) {
  const relation = card.relation ? RELATION[card.relation] : undefined;
  return (
    <Link
      to="/cards/$cardId"
      params={{ cardId: card.display_id }}
      search={{ owner }}
      className="chat-ref"
      title={card.title}
    >
      {relation ? <span className="rel">{relation}</span> : null}
      <span className="id">{card.display_id}</span>
      <span className="t">{card.title}</span>
    </Link>
  );
}

function PrRef({ pr }: { pr: ThreadPr }) {
  return (
    <a className="chat-ref" href={pr.url} target="_blank" rel="noreferrer">
      <PrIcon />
      <span className="id">#{pr.number}</span>
    </a>
  );
}

// The owner's message, on the right. When it was sent shows outside it, on
// its right, while the pointer is on it. It says more when it never reached the orchestrator.
export function AskMessage({
  ask,
  owner,
  fresh,
}: {
  ask: ThreadAsk;
  owner: string | undefined;
  // It came in while the chat was open, so it eases in.
  fresh: boolean;
}) {
  const failed = undelivered(ask);
  return (
    <div className="chat-msg user" data-testid="chat-ask" data-fresh={fresh || undefined}>
      <div className="chat-ask-row">
        <Markdown className="chat-bubble" text={ask.text} />
        <Stamp className="chat-ask-time" at={ask.at} />
      </div>
      {ask.cards.length > 0 ? (
        <div className="chat-refs">
          {ask.cards.map((card) => (
            <CardRef key={card.id} card={card} owner={owner} />
          ))}
        </div>
      ) : null}
      {failed ? (
        <div className="chat-meta failed" data-testid="chat-undelivered">
          <FailedIcon />
          {failed}
        </div>
      ) : ask.source === "prompt" ? (
        <div className="chat-meta">Typed in the terminal</div>
      ) : null}
    </div>
  );
}

// The owner's message while it is on its way: shown at once, as sent.
export function PendingAsk({ text }: { text: string }) {
  return (
    <div className="chat-msg user pending" data-testid="chat-pending">
      <Markdown className="chat-bubble" text={text} />
    </div>
  );
}

// The orchestrator's answer: flat, with Copy and Reply when the pointer is on it.
export function ReplyMessage({
  item,
  owner,
  fresh,
  follow,
  onReply,
  onSuggest,
}: {
  item: ThreadReply;
  owner: string | undefined;
  fresh: boolean;
  // The suggested follow-ups, under the latest answer only.
  follow: string[];
  onReply: (body: string) => void;
  onSuggest: (text: string) => void;
}) {
  const { reply } = item;
  const body = untagged(reply.body);
  const copy = () => {
    void navigator.clipboard?.writeText(body).then(
      () => showToast("ok", "Copied the reply"),
      () => showToast("bad", "Could not copy", "The browser refused the clipboard."),
    );
  };
  return (
    <div className="chat-msg assistant" data-testid="chat-reply" data-fresh={fresh || undefined}>
      <Markdown className="chat-prose" text={body} />
      {reply.card ? (
        <div className="chat-refs">
          <CardRef card={reply.card} owner={owner} />
        </div>
      ) : null}
      <div className="chat-acts">
        <Stamp className="stamp" at={reply.replied_at} />
        <button type="button" className="chat-act" aria-label="Copy" title="Copy" onClick={copy}>
          <CopyIcon />
        </button>
        <button
          type="button"
          className="chat-act"
          aria-label="Reply to this"
          title="Reply to this"
          onClick={() => onReply(body)}
        >
          <ReplyToIcon />
        </button>
      </div>
      {follow.length > 0 ? (
        <div className="chat-follow" data-testid="chat-follow">
          {follow.map((text) => (
            <button
              type="button"
              key={text}
              className="chat-suggest"
              onClick={() => onSuggest(text)}
            >
              {text}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

// One daemon change the orchestrator made: the event, whether it held, and
// what it recorded.
function ToolBlock({ step }: { step: ThreadStep }) {
  const failed = stepFailed(step);
  const payload =
    Object.keys(step.payload).length > 0 ? JSON.stringify(step.payload, null, 2) : null;
  return (
    <details className="chat-tool" data-testid="chat-tool">
      <summary>
        <TerminalIcon />
        <span className="n">{step.kind}</span>
        <span className={`st ${failed ? "bad" : "ok"}`}>
          {failed ? <FailedIcon /> : <DoneIcon />}
          {failed ? "Failed" : "Completed"}
        </span>
        <ChevronDownIcon />
      </summary>
      <div className="tb">
        <div>
          <h4>Change</h4>
          <pre>{`${stepTitle(step.label)}${step.card ? `\n${step.card.display_id} ${step.card.title}` : ""}${step.pr ? `\nPR #${step.pr.number}` : ""}`}</pre>
        </div>
        {payload ? (
          <div>
            <h4>Result</h4>
            <pre>{payload}</pre>
          </div>
        ) : null}
      </div>
    </details>
  );
}

function Step({ step, owner }: { step: ThreadStep; owner: string | undefined }) {
  const failed = stepFailed(step);
  return (
    <li className={`chat-step${failed ? " bad" : ""}${step.system ? " system" : ""}`}>
      <span className="rail">{failed ? <FailedIcon /> : <DotIcon />}</span>
      <div className="txt">
        <div className="lbl">
          {stepTitle(step.label)}
          <Stamp className="when" at={step.at} />
        </div>
        {step.system ? <div className="desc">Recorded by tiller</div> : null}
        {step.card || step.pr ? (
          <div className="chat-refs">
            {step.card ? <CardRef card={step.card} owner={owner} /> : null}
            {step.pr ? <PrRef pr={step.pr} /> : null}
          </div>
        ) : null}
        {step.system ? null : <ToolBlock step={step} />}
      </div>
    </li>
  );
}

// What the orchestrator did between a message and its answer, folded.
export function ActivityBlock({
  activity,
  owner,
  fresh,
}: {
  activity: ThreadActivity;
  owner: string | undefined;
  fresh: boolean;
}) {
  const count = activity.steps.length;
  return (
    <details className="chat-cot" data-testid="chat-activity" data-fresh={fresh || undefined}>
      <summary>
        <StepsIcon />
        <span className="t">{activityText(activity.duration_seconds, count)}</span>
        <ChevronDownIcon />
      </summary>
      <ol className="chat-steps">
        {activity.steps.map((step) => (
          <Step key={step.seq} step={step} owner={owner} />
        ))}
      </ol>
    </details>
  );
}

export function CheckpointLine({ checkpoint }: { checkpoint: ThreadCheckpoint }) {
  return (
    <div className="chat-checkpoint" data-testid="chat-checkpoint">
      <span>
        <RestartIcon />
        Orchestrator restarted <Stamp at={checkpoint.at} />
      </span>
    </div>
  );
}

// The attention item a decision is answered with: the board's own read when
// it has it, which carries the newest rev, else the copy in the thread. A
// merge approval is answered through its attention item.
function itemFor(decision: ThreadDecision, attention: AttentionItem[]): AttentionItem | null {
  if (decision.source === "approval") {
    return attention.find((item) => item.approval?.id === decision.id) ?? null;
  }
  return attention.find((item) => item.id === decision.id) ?? decision.attention ?? null;
}

function kindLabel(decision: ThreadDecision): string {
  if (decision.source === "approval") {
    return decision.kind === "merge" ? "Merge approval" : "Land approval";
  }
  return KIND_LABELS[decision.kind] ?? "Decision";
}

// A decision the orchestrator needs: open, it is answered here with the
// Inbox's own controls; closed, it says what was chosen and when.
export function DecisionBlock({
  decision,
  attention,
  owner,
}: {
  decision: ThreadDecision;
  attention: AttentionItem[];
  owner: string | undefined;
}) {
  const answeredHere = useStore(answeredStore);
  const item = itemFor(decision, attention);
  const sent = item ? answeredHere[item.id] : undefined;
  const question = decision.question ?? item?.question ?? "The orchestrator needs a decision";
  if (!decision.open && !sent) {
    const text = decisionText(decision, stamp);
    return (
      <div
        className={`chat-decided${decision.state === "withdrawn" ? " mute" : ""}`}
        data-testid="chat-decided"
      >
        {decision.state === "withdrawn" ? <DotIcon /> : <DoneIcon />}
        <span className="q">{question}</span>
        <span className="res">{text}</span>
      </div>
    );
  }
  const inbox = item ? item.id : undefined;
  const approval = item?.approval ? item : null;
  // An approval is about its card, so the card names it. A question names
  // itself, and its card goes under it.
  const titleCard = approval && decision.card ? decision.card : null;
  return (
    <div className="cq chat-confirm" data-testid="chat-confirm">
      <div className="cq-item">
        <div className="chat-dk">
          {sent ? <OkIcon /> : <span className="cq-dot" />}
          <span className="kind">{kindLabel(decision)}</span>
          {sent ? (
            <span>Answered</span>
          ) : (
            <Ago at={item?.opened_at ?? decision.at} prefix="asked" />
          )}
          <Link
            to="/inbox"
            search={({ host }) => ({ host, item: inbox })}
            className="chat-dk-open"
            aria-label="Open in the Inbox"
            title="Open in the Inbox"
          >
            <InboxIcon />
          </Link>
        </div>
        {titleCard ? (
          <h5 className="chat-dq">
            <Link
              to="/cards/$cardId"
              params={{ cardId: titleCard.display_id }}
              search={{ owner }}
              className="chat-dq-card"
            >
              <span className="id">{titleCard.display_id}</span>
              {titleCard.title}
            </Link>
          </h5>
        ) : (
          <h5 className="chat-dq">{question}</h5>
        )}
        {approval ? (
          <ChatApprovalFacts item={approval} pr={decision.pr ?? null} />
        ) : decision.card ? (
          <div className="chat-dfacts">
            <Link
              to="/cards/$cardId"
              params={{ cardId: decision.card.display_id }}
              search={{ owner }}
              className="chat-dq-card"
            >
              <span className="id">{decision.card.display_id}</span>
              {decision.card.title}
            </Link>
          </div>
        ) : null}
        {item?.consequence ? <p className="cq-ctx">{item.consequence}</p> : null}
        <div className="cq-answer">
          {item === null ? (
            <p className="cq-ctx">The Inbox has this decision. Open it there to answer.</p>
          ) : item.approval ? (
            <ApproveActions item={item} />
          ) : (
            <AnswerForm item={item} keys={false} />
          )}
        </div>
      </div>
    </div>
  );
}

// What an approval lands, on one line: the pull request, its checks and the
// branch it merges into. The card's read has them; until it arrives, the
// thread's own link to the pull request stands in.
function ChatApprovalFacts({ item, pr: threadPr }: { item: AttentionItem; pr: ThreadPr | null }) {
  const { pr } = useApprovalPr(item);
  if (!pr) {
    return threadPr ? (
      <div className="chat-dfacts" data-testid="approval-facts">
        <a href={threadPr.url} target="_blank" rel="noreferrer noopener">
          <PrIcon />#{threadPr.number}
        </a>
      </div>
    ) : null;
  }
  const checks =
    pr.checks === "passing"
      ? { tone: "ok", text: "Checks passed" }
      : pr.checks === "failing"
        ? { tone: "bad", text: "Checks failed" }
        : { tone: "yel", text: "Checks running" };
  return (
    <div className="chat-dfacts" data-testid="approval-facts">
      <a href={pr.url} target="_blank" rel="noreferrer noopener" title={pr.title}>
        <PrIcon />
        {pr.repo}#{pr.number}
      </a>
      <span className={`ck ${checks.tone}`} title={pr.checks_summary ?? undefined}>
        {pr.checks === "passing" ? <CheckIcon /> : null}
        {checks.text}
      </span>
      <span>
        into <span className="mono">{pr.base}</span>
      </span>
    </div>
  );
}
