import { Link, useNavigate } from "@tanstack/react-router";
import type { CSSProperties } from "react";

import type { AwayBrief, BoardRow, Project } from "../../lib/api-types";
import { AWAY_BRIEF_ID, markBriefRead, mergeLabel } from "../../lib/away";
import { formatLisbonDay } from "../../lib/clock";
import { projectIndex } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { formatWhen } from "./format";
import { BackIcon, NeedsIcon } from "./icons";
import "./away-brief.css";

const MoonIcon = () => (
  <svg className="icon" width="13" height="13" viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M12.8 10.2A5.5 5.5 0 0 1 5.8 3.2a5.5 5.5 0 1 0 7 7z"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinejoin="round"
    />
  </svg>
);

// "Mon 9 Nov, 22:40 - Tue 10 Nov, 08:15 (Lisbon)".
function range(brief: AwayBrief): string {
  const from = formatLisbonDay(brief.started_at);
  const to = formatLisbonDay(brief.ended_at ?? brief.until_at);
  return `${from} - ${to} (Lisbon)`;
}

export function AwayBriefRow({ brief, selected }: { brief: AwayBrief; selected: boolean }) {
  return (
    <section aria-label="Away brief">
      <Link
        to="/inbox"
        search={({ host }) => ({ host, item: AWAY_BRIEF_ID })}
        className="irow"
        data-selected={selected}
        data-testid="inbox-row-away-brief"
      >
        <span className="ii away-ii">
          <MoonIcon />
        </span>
        <span className="bd">
          <span className="t">While you were away</span>
          <span className="s">{range(brief)}</span>
        </span>
      </Link>
    </section>
  );
}

interface Context {
  cards: Map<string, BoardRow>;
  projects: Project[];
}

// One line of a section: the project dot, the card's id and title, a note on
// the right. A card the board still has links to its page.
function Line({
  card,
  title,
  note,
  ctx,
}: {
  card: string | null;
  title: string;
  note: string;
  ctx: Context;
}) {
  const row = card ? ctx.cards.get(card) : undefined;
  const index = row ? projectIndex(ctx.projects, row) : -1;
  const body = (
    <>
      {index >= 0 ? (
        <span className="pdot" style={{ "--pc": projectColor(index) } as CSSProperties} />
      ) : null}
      {card ? <b>{card}</b> : null}
      <span className="ab-t">{title}</span>
      <span className="sp" />
      <span className="m">{note}</span>
    </>
  );
  return row ? (
    <Link to="/cards/$cardId" params={{ cardId: row.id }} className="ab-r">
      {body}
    </Link>
  ) : (
    <div className="ab-r">{body}</div>
  );
}

function Section({
  title,
  count,
  tone,
  children,
}: {
  title: string;
  count: number;
  tone?: "warn";
  children: React.ReactNode;
}) {
  if (count === 0) return null;
  return (
    <div className="ab-s" role="group" aria-label={title}>
      <div className={`ab-h${tone ? ` ${tone}` : ""}`}>
        {tone ? <NeedsIcon /> : null}
        {title}
        <span className="n">{count}</span>
      </div>
      {children}
    </div>
  );
}

function plural(count: number, one: string): string {
  return `${count} ${one}${count === 1 ? "" : "s"}`;
}

export function AwayBriefDetail({ brief, ctx }: { brief: AwayBrief; ctx: Context }) {
  const navigate = useNavigate();
  const { spend } = brief;
  const byCard = new Map<string, { title: string; runs: number; failed: boolean }>();
  for (const run of brief.runs) {
    const seen = byCard.get(run.card) ?? { title: run.title, runs: 0, failed: false };
    byCard.set(run.card, {
      title: seen.title,
      runs: seen.runs + 1,
      failed: seen.failed || run.outcome === "failed",
    });
  }
  const grantOf = (card: string) =>
    brief.merged_by_grant.find((line) => line.card === card && line.grant_id);
  const waiting = brief.waiting_decisions.length + brief.waiting_approvals.length;
  const quota =
    spend.quota_floor_percent === null
      ? "no quota floor"
      : `quota floor ${spend.quota_floor_percent} %`;
  const merges = brief.merged_by_grant.length;

  return (
    <>
      <Link
        to="/inbox"
        search={({ host }) => ({ host })}
        className="btn sm in-back"
        aria-label="Back to the inbox"
      >
        <BackIcon />
        Inbox
      </Link>
      <div className="d-meta">
        <span className="lab sm acc">
          <MoonIcon />
          Away brief
        </span>
        <span>{range(brief)}</span>
      </div>
      <h2 className="d-q">While you were away</h2>
      <div className="ab-card" data-testid="away-brief">
        <div className="ab-s">
          {brief.orders ? <p className="ab-quote">&ldquo;{brief.orders}&rdquo;</p> : null}
          <dl className="props ab-props">
            <dt>Spend</dt>
            <dd>
              {spend.max_new_runs === null
                ? `${plural(spend.new_runs, "new run")}`
                : `${spend.new_runs} of ${plural(spend.max_new_runs, "new run")}`}
              {` · ${quota}`}
            </dd>
            <dt>Merges</dt>
            <dd>
              {mergeLabel(brief.merge)}
              {` · ${plural(merges, "merge")}`}
            </dd>
          </dl>
        </div>
        <Section title="Landed" count={brief.landed.length}>
          {brief.landed.map((card) => (
            <Line
              key={card.card}
              card={card.card}
              title={card.title}
              note={
                grantOf(card.card) ? `grant ${grantOf(card.card)?.grant_id?.slice(-4)}` : "landed"
              }
              ctx={ctx}
            />
          ))}
        </Section>
        <Section title="Runs started" count={brief.runs.length}>
          {[...byCard].map(([card, seen]) => (
            <Line
              key={card}
              card={card}
              title={seen.title}
              note={`${plural(seen.runs, "run")}${seen.failed ? ", failed" : ""}`}
              ctx={ctx}
            />
          ))}
        </Section>
        <Section title="Still waiting for you" count={waiting} tone="warn">
          {brief.waiting_decisions.map((item) => (
            <Line
              key={item.attention_id}
              card={item.card}
              title={item.question}
              note="Decision"
              ctx={ctx}
            />
          ))}
          {brief.waiting_approvals.map((item) => (
            <Line
              key={item.approval_id}
              card={item.card}
              title={`Approve ${item.action}`}
              note={formatWhen(item.requested_at)}
              ctx={ctx}
            />
          ))}
        </Section>
        <div className="ab-f">
          <span className="sp" />
          {waiting > 0 ? (
            <Link
              to="/inbox"
              search={({ host }) => ({ host, item: firstWaiting(brief) })}
              className="btn"
            >
              Open waiting items
            </Link>
          ) : null}
          <button
            type="button"
            className="btn primary"
            onClick={() => {
              markBriefRead(brief.approval_id);
              void navigate({ to: "/inbox", search: ({ host }) => ({ host }) });
            }}
          >
            Mark read
          </button>
        </div>
      </div>
    </>
  );
}

// The first waiting decision opens in the inbox; with only approvals waiting,
// the inbox's own default shows the oldest item.
function firstWaiting(brief: AwayBrief): string | undefined {
  return brief.waiting_decisions[0]?.attention_id;
}
