import { Link, useNavigate } from "@tanstack/react-router";
import type { CSSProperties } from "react";

import type { AwayBrief, BoardRow, Project } from "../../lib/api-types";
import { AWAY_BRIEF_ID, markBriefRead, mergeLabel, projectName, readKey } from "../../lib/away";
import type { HostAwayRead, HostContract } from "../../lib/away";
import { formatLisbonDay } from "../../lib/clock";
import { projectIndex } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { KIND_LABELS, formatWhen } from "./format";
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

// "Mon 9 Nov, 22:40 - Tue 10 Nov, 08:15 (Lisbon)": from the first start to
// the last end, across every host's contract.
function range(briefs: AwayBrief[]): string {
  const starts = briefs.map((brief) => brief.started_at);
  const ends = briefs.map((brief) => brief.ended_at ?? brief.until_at);
  const first = starts.reduce((a, b) => (Date.parse(b) < Date.parse(a) ? b : a));
  const last = ends.reduce((a, b) => (Date.parse(b) > Date.parse(a) ? b : a));
  return `${formatLisbonDay(first)} - ${formatLisbonDay(last)} (Lisbon)`;
}

export function AwayBriefRow({
  contracts,
  selected,
}: {
  contracts: HostContract[];
  selected: boolean;
}) {
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
          <span className="s">{range(contracts.map((contract) => contract.brief))}</span>
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

// One host's part of the brief: its scope, and what happened under it.
function HostBrief({
  contract,
  label,
  ctx,
}: {
  contract: HostContract;
  label: string | null;
  ctx: Context;
}) {
  const { brief } = contract;
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
  const covered = brief.projects
    .map((id) => projectName(id, contract.host, ctx.projects))
    .join(", ");

  return (
    <div
      className="ab-host"
      role={label ? "group" : undefined}
      aria-label={label ?? undefined}
      data-testid="away-brief-host"
    >
      {label ? <div className="ab-hn">{label}</div> : null}
      <div className="ab-s">
        {brief.orders ? <p className="ab-quote">&ldquo;{brief.orders}&rdquo;</p> : null}
        <dl className="props ab-props">
          <dt>Projects</dt>
          <dd>{covered}</dd>
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
            note={KIND_LABELS[item.kind] ?? "Decision"}
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
    </div>
  );
}

// One brief for every host's ended contract (UA-30). With paired hosts each
// part names its host, and a peer the board could not read is named too.
export function AwayBriefDetail({
  contracts,
  unread,
  grouped,
  local,
  ctx,
}: {
  contracts: HostContract[];
  unread: HostAwayRead[];
  grouped: boolean;
  local: string | null;
  ctx: Context;
}) {
  const navigate = useNavigate();
  const briefs = contracts.map((contract) => contract.brief);
  const waiting = briefs.some(
    (brief) => brief.waiting_decisions.length + brief.waiting_approvals.length > 0,
  );
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
        <span>{range(briefs)}</span>
      </div>
      <h2 className="d-q">While you were away</h2>
      <div className="ab-card" data-testid="away-brief">
        {contracts.map((contract) => (
          <HostBrief
            key={readKey(contract)}
            contract={contract}
            label={grouped ? (contract.host ?? local ?? "This host") : null}
            ctx={ctx}
          />
        ))}
        {unread.length > 0 ? (
          <div className="ab-s" data-testid="away-brief-unread">
            <dl className="props ab-props">
              {unread.map((entry) => (
                <UnreadLine key={entry.host} entry={entry} />
              ))}
            </dl>
          </div>
        ) : null}
        <div className="ab-f">
          <span className="sp" />
          {waiting ? (
            <Link
              to="/inbox"
              search={({ host }) => ({ host, item: firstWaiting(briefs) })}
              className="btn"
            >
              Open waiting items
            </Link>
          ) : null}
          <button
            type="button"
            className="btn primary"
            onClick={() => {
              markBriefRead(contracts.map(readKey));
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

function UnreadLine({ entry }: { entry: HostAwayRead }) {
  return (
    <>
      <dt>Not read</dt>
      <dd>{entry.message}. Its part shows here once it can be read.</dd>
    </>
  );
}

// The first waiting decision opens in the inbox; with only approvals waiting,
// the inbox's own default shows the oldest item.
function firstWaiting(briefs: AwayBrief[]): string | undefined {
  return briefs.flatMap((brief) => brief.waiting_decisions)[0]?.attention_id;
}
