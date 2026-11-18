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
import { formatLisbon, formatLisbonDay, minutesSince } from "../../lib/clock";
import { untagged } from "../../lib/orchestrator-text";
import { useStore } from "../../lib/store";
import { AnswerForm } from "../attention/answer";
import { answeredStore } from "../attention/answered";
import { ApprovalFacts } from "../attention/approval-facts";
import { ApproveActions } from "../attention/approve";
import { KIND_LABELS } from "../attention/format";
import { OkIcon } from "../attention/icons";
import { Ago } from "../board/ago";
import { ChevronDownIcon, PrIcon, TerminalIcon } from "../board/icons";
import { showToast } from "../board/toast";
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

// "Tue 18:02" this week, "Thu 12 Nov, 18:00" before it: Lisbon time, with the day.
export function stamp(iso: string | null): string | null {
  const minutes = minutesSince(iso);
  return minutes !== null && minutes > 6 * 24 * 60 ? formatLisbonDay(iso) : formatLisbon(iso);
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

// The owner's message, on the right, with when it was sent. It says more only
// when it never reached the orchestrator.
export function AskMessage({ ask, owner }: { ask: ThreadAsk; owner: string | undefined }) {
  const failed = undelivered(ask);
  const at = stamp(ask.at);
  return (
    <div className="chat-msg user" data-testid="chat-ask">
      <Markdown className="chat-bubble" text={ask.text} />
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
      ) : (
        <div className="chat-meta">
          {ask.source === "prompt" ? `Typed in the terminal ${at ?? ""}` : at}
        </div>
      )}
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
  follow,
  onReply,
  onSuggest,
}: {
  item: ThreadReply;
  owner: string | undefined;
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
    <div className="chat-msg assistant" data-testid="chat-reply">
      <Markdown className="chat-prose" text={body} />
      {reply.card ? (
        <div className="chat-refs">
          <CardRef card={reply.card} owner={owner} />
        </div>
      ) : null}
      <div className="chat-acts">
        <span className="stamp">{stamp(reply.replied_at)}</span>
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
          <span className="when">{formatLisbon(step.at)}</span>
        </div>
        {step.system ? <div className="desc">Recorded by Tiller</div> : null}
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
}: {
  activity: ThreadActivity;
  owner: string | undefined;
}) {
  const count = activity.steps.length;
  return (
    <details className="chat-cot" data-testid="chat-activity">
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
        {checkpoint.restarted ? "Orchestrator restarted" : "Orchestrator started"}{" "}
        {stamp(checkpoint.at)}
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
  return (
    <div className="cq chat-confirm" data-testid="chat-confirm">
      <div className={sent ? "cq-item done" : "cq-item"}>
        <div className="cq-meta">
          {sent ? <OkIcon /> : <span className="cq-dot" />}
          <b>{sent ? "Answered" : "Needs you"}</b>
          <span className="lab sm">{kindLabel(decision)}</span>
          <Ago at={item?.opened_at ?? decision.at} prefix="asked" />
          <Link to="/inbox" search={({ host }) => ({ host, item: inbox })} className="cq-open">
            Open in the Inbox
          </Link>
        </div>
        <h5 className="cq-q">{question}</h5>
        {decision.card ? (
          <div className="chat-refs">
            <CardRef card={decision.card} owner={owner} />
            {decision.pr ? <PrRef pr={decision.pr} /> : null}
          </div>
        ) : null}
        {item?.consequence ? <p className="cq-ctx">{item.consequence}</p> : null}
        <div className="cq-answer">
          {item === null ? (
            <p className="cq-ctx">The Inbox has this decision. Open it there to answer.</p>
          ) : item.approval ? (
            <>
              <ApprovalFacts item={item} />
              <ApproveActions item={item} />
            </>
          ) : (
            <AnswerForm item={item} keys={false} />
          )}
        </div>
      </div>
    </div>
  );
}
