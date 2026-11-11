import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useState } from "react";

import type { InterventionKind, SignalsIntervention, SignalsResponse } from "../../lib/api-types";
import { formatLisbon, serverNow } from "../../lib/clock";
import { prefsStore, visibleProjects } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import {
  projectsQuery,
  queryKeyFor,
  quotaQuery,
  signalsPath,
  signalsQuery,
} from "../../lib/queries";
import { useStore } from "../../lib/store";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { ChevronDownIcon, ChevronRightIcon } from "../board/icons";
import { Select } from "../ui/select";
import { BarChart, LineChart, Spark, StackedBarChart } from "./charts";
import type { Point } from "./charts";
import { Learning } from "./learning";
import { MemorySkips } from "./memory-skips";
import { lisbonShortDay, lisbonWeekLabel } from "./lisbon";
import { accepted, projectRows, quotaBlockOf, sampleLines, signalsOf } from "./model";
import type { QuotaBlock, Signal } from "./model";
import "../board/board.css";
import "./health.css";
import { useNamedProjects } from "../board/peer-state";

const STATE_LABEL = { good: "Good", watch: "Watch", none: "No data yet" } as const;

function SignalCard({
  signal,
  weeks,
}: {
  signal: Signal;
  weeks: { label: string; start: string }[];
}) {
  return (
    <article
      className="sig"
      data-state={signal.state}
      data-signal={signal.id}
      aria-label={`${signal.title}: ${STATE_LABEL[signal.state]}`}
    >
      <header>
        <h3>{signal.title}</h3>
        <span className={`state ${signal.state}`}>
          <i />
          {STATE_LABEL[signal.state]}
        </span>
      </header>
      <div className="sig-big">
        <b>{signal.big}</b>
        <span>{signal.unit}</span>
      </div>
      <p className="sig-read">{signal.read}</p>
      <div className="sig-facts">
        {signal.facts.map((fact) => (
          <span key={fact} className="lab sm">
            {fact}
          </span>
        ))}
      </div>
      <Spark
        label={signal.title}
        values={signal.values}
        weeks={weeks}
        muted={signal.state === "none"}
      />
      <footer>
        <span>Last 8 weeks</span>
        <b>{signal.last}</b>
      </footer>
    </article>
  );
}

function Quota({ block }: { block: QuotaBlock | null }) {
  if (!block) {
    return (
      <section className="quota quota-empty" aria-label="Claude quota">
        <div className="q-main">
          <div className="q-head">
            <span className="q-title">Claude quota</span>
          </div>
          <p className="q-line">No quota data yet.</p>
        </div>
      </section>
    );
  }
  const left = block.percentLeft;
  const pace = block.paceMarker === null ? null : Math.round(block.paceMarker);
  return (
    <section className="quota" aria-label="Claude quota" data-tone={block.tone}>
      <div className="q-main">
        <div className="q-head">
          <span className="q-title">Claude quota</span>
          <span className={`q-state ${block.tone === "ok" ? "" : block.tone}`}>
            <i />
            {block.word}
          </span>
        </div>
        <p className="q-line">
          {block.percentLeft === null ? (
            "Weekly use is not reported."
          ) : (
            <>
              <b>{block.percentLeft}%</b> of this week left.
            </>
          )}
          {block.resetsShort ? ` Resets ${block.resetsShort}.` : ""} {block.verdict}
        </p>
        <div
          className={`qmeter big${left === null ? " unk" : ""}`}
          role="img"
          aria-label={
            left === null
              ? "Weekly use is not reported"
              : `${left}% left${pace === null ? "" : `, an even spend would have ${pace}% left by now`}`
          }
        >
          {left === null ? null : <i style={{ width: `${left}%` }} />}
          {block.paceMarker === null ? null : <u style={{ left: `${block.paceMarker}%` }} />}
        </div>
        <div className="q-key">
          {left === null ? null : (
            <span>
              <i className="k-fill" />
              {left}% left
            </span>
          )}
          {pace === null ? null : (
            <span>
              <i className="k-tick" />
              even pace would have {pace}% left
            </span>
          )}
          {block.resetsFull ? <span className="muted">Resets {block.resetsFull}</span> : null}
        </div>
      </div>
      <div className="q-side">
        <div className="q-sh">Other windows</div>
        {block.others.length === 0 ? <div className="qo-v muted">none reported</div> : null}
        {block.others.map((other) => (
          <div key={other.label} className="qo">
            <span className="qo-l">{other.label}</span>
            {other.percentLeft === null ? (
              <>
                <span className="qmeter sm unk" aria-hidden="true" />
                <span className="qo-v muted">not reported</span>
              </>
            ) : (
              <>
                <span className="qmeter sm" aria-hidden="true">
                  <i style={{ width: `${other.percentLeft}%` }} />
                </span>
                <span className="qo-v">{other.percentLeft}% left</span>
              </>
            )}
          </div>
        ))}
        <div className="q-facts">
          {block.reserve ? <span className="lab sm">{block.reserve}</span> : null}
          {block.burn ? <span className="lab sm">{block.burn}</span> : null}
        </div>
      </div>
    </section>
  );
}

const AVOIDABLE_PARTS = [
  { className: "b-cor", label: "Corrections" },
  { className: "b-esc", label: "Unplanned escalations" },
  { className: "b-rew", label: "Rework" },
];

const KINDS: { kind: InterventionKind; label: string }[] = [
  { kind: "correction", label: "Correction" },
  { kind: "unplanned_escalation", label: "Unplanned escalation" },
  { kind: "rework", label: "Rework" },
  { kind: "planned", label: "Planned" },
];
const kindLabel = (kind: InterventionKind) =>
  KINDS.find((candidate) => candidate.kind === kind)?.label ?? kind;

// One time you were needed. The rules label it when it is read; the select
// lets you say what it really was, and Reset hands it back to the rules.
function InterventionRow({ item }: { item: SignalsIntervention }) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);

  const relabel = (label: InterventionKind | null) => {
    setPending(true);
    setError(null);
    sendMutation("POST", `/api/v1/interventions/${item.id}/label`, {
      label,
      idempotency_key: ulid(),
    })
      .then(() => queryClient.invalidateQueries({ queryKey: queryKeyFor(signalsPath) }))
      .catch((caught: unknown) => {
        setError(caught instanceof ActionError ? caught : null);
      })
      .finally(() => setPending(false));
  };

  return (
    <li className="iv" data-kind={item.kind}>
      <div className="iv-main">
        <span className="iv-sum">{item.summary}</span>
        <span className="iv-meta">
          {item.display_id} &middot; {item.source === "note" ? "your note" : "asked you"} &middot;{" "}
          {formatLisbon(item.at) ?? item.at}
          {item.relabeled ? ` · the rules said ${kindLabel(item.classified).toLowerCase()}` : ""}
        </span>
        {error ? (
          <span className="refusal" role="alert">
            {error.message}
          </span>
        ) : null}
      </div>
      <Select
        className="iv-sel"
        label={`Why you were needed: ${item.summary}`}
        value={item.kind}
        disabled={pending}
        onValueChange={(next) => relabel(next as InterventionKind)}
        options={KINDS.map((candidate) => ({ value: candidate.kind, label: candidate.label }))}
      />
      {item.relabeled ? (
        <button type="button" className="btn sm" disabled={pending} onClick={() => relabel(null)}>
          Reset
        </button>
      ) : null}
    </li>
  );
}

function Interventions({ items }: { items: SignalsIntervention[] }) {
  const counted = items.filter((item) => item.kind !== "planned").length;
  return (
    <section className="cpanel ivs" aria-label="Times you were needed">
      <div className="ch">
        <div>
          <h3>Times you were needed, and why</h3>
          <p className="cs">
            Each is labeled by rule: a correction, an unplanned escalation, rework, or planned
            attention. Change a label if it is wrong; the charts follow.
          </p>
        </div>
      </div>
      {items.length === 0 ? (
        <p className="n">You have not been needed in these weeks.</p>
      ) : (
        <>
          <ul className="iv-list">
            {items.map((item) => (
              <InterventionRow key={item.id} item={item} />
            ))}
          </ul>
          <p className="n">{`The latest ${items.length}, ${counted} of them avoidable.`}</p>
        </>
      )}
    </section>
  );
}

const ratio = (value: number) => Number(value.toFixed(2)).toString();
const whole = (value: number) => String(Math.round(value));
const tenth = (value: number) => Number(value.toFixed(1)).toString();

function Numbers({ data, shown }: { data: SignalsResponse; shown: Set<string> }) {
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data).projects;
  const weeks = data.weeks.map((week) => ({
    label: lisbonWeekLabel(week.start),
    start: lisbonShortDay(week.start),
  }));
  const series = (pick: (week: SignalsResponse["weeks"][number]) => number | null): Point[] =>
    data.weeks.map((week, index) => ({
      label: weeks[index]?.label ?? "",
      start: weeks[index]?.start ?? "",
      value: pick(week),
    }));
  const latest = data.weeks.at(-1);
  const rows = projectRows(data, shown);
  const samples = sampleLines(data);
  return (
    <div className="num-body">
      <div className="charts">
        <div className="cpanel">
          <div className="ch">
            <div>
              <h3>Accepted outcomes per week</h3>
              <p className="cs">Cards you accepted: landed cards plus accepted scout reports.</p>
            </div>
          </div>
          <BarChart
            title="Accepted outcomes per week"
            points={series(accepted)}
            format={whole}
            unit="accepted"
          />
          {samples ? <p className="n">{samples.accepted}</p> : null}
        </div>
        <div className="cpanel">
          <div className="ch">
            <div>
              <h3>Avoidable interventions per accepted outcome</h3>
              <p className="cs">
                Times you had to step in, divided by outcomes accepted that week.
              </p>
            </div>
          </div>
          <StackedBarChart
            title="Avoidable interventions per accepted outcome"
            parts={AVOIDABLE_PARTS}
            points={data.weeks.map((week, index) => ({
              label: weeks[index]?.label ?? "",
              start: weeks[index]?.start ?? "",
              values:
                accepted(week) > 0
                  ? [
                      week.interventions.correction / accepted(week),
                      week.interventions.unplanned_escalation / accepted(week),
                      week.interventions.rework / accepted(week),
                    ]
                  : null,
            }))}
            format={ratio}
            unit="per outcome"
          />
          {samples ? <p className="n">{samples.avoidable}</p> : null}
        </div>
        <div className="cpanel">
          <div className="ch">
            <div>
              <h3>Escaped defects</h3>
              <p className="cs">
                Bugs found after landing, traced back to a card. Lower is better.
              </p>
            </div>
          </div>
          <BarChart
            title="Escaped defects"
            points={series((week) => week.escaped)}
            format={whole}
            unit="escaped"
          />
          {samples ? <p className="n">{samples.escaped}</p> : null}
        </div>
        <div className="cpanel">
          <div className="ch">
            <div>
              <h3>Median attention time per item</h3>
              <p className="cs">Minutes from an item reaching your inbox to your answer.</p>
            </div>
          </div>
          <LineChart
            title="Median attention time per item"
            points={series((week) => week.median_answer_minutes)}
            format={tenth}
            unit="min"
          />
          {samples ? <p className="n">{samples.attention}</p> : null}
        </div>
      </div>
      <div className="tbl-wrap">
        <table>
          <thead>
            <tr>
              <th>
                {latest
                  ? `${lisbonWeekLabel(latest.start).replace("W", "Week ")} by project`
                  : "By project"}
              </th>
              <th className="r">Accepted</th>
              <th className="r">Avoidable per outcome</th>
              <th className="r">Escaped defects</th>
              <th className="r">Median attention</th>
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 ? (
              <tr>
                <td colSpan={5} className="none">
                  No projects shown.
                </td>
              </tr>
            ) : (
              rows.map((row) => {
                const project = projects.find((candidate) => candidate.id === row.project_id);
                const index = projects.findIndex((candidate) => candidate.id === row.project_id);
                return (
                  <tr key={row.project_id}>
                    <td>
                      <span
                        className="pdot"
                        style={{ "--pc": projectColor(Math.max(0, index)) } as React.CSSProperties}
                      />
                      {project?.name ?? row.project_id}
                    </td>
                    <td className="r">{row.accepted}</td>
                    <td className="r">
                      {row.perOutcome === null ? "none" : row.perOutcome.toFixed(2)}
                      <small>{`${row.avoidable} of ${row.accepted} outcomes`}</small>
                    </td>
                    <td className="r">
                      {row.escaped}
                      <small>{`of ${row.accepted} outcomes`}</small>
                    </td>
                    <td className="r">
                      {row.median === null ? "none" : `${tenth(row.median)} min`}
                      <small>{`${row.answered} answered`}</small>
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
      <Interventions items={data.interventions} />
    </div>
  );
}

export function HealthPage() {
  const data = useSuspenseQuery(signalsQuery()).data;
  const quota = useSuspenseQuery(quotaQuery()).data;
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data).projects;
  const prefs = useStore(prefsStore);
  const [open, setOpen] = useState(false);
  const now = serverNow();
  const signals = signalsOf(data, now);
  const weeks = data.weeks.map((week) => ({
    label: lisbonWeekLabel(week.start),
    start: lisbonShortDay(week.start),
  }));
  const shown = new Set(visibleProjects(prefs, projects).map((project) => project.id));

  return (
    <div className="health" data-testid="health">
      <div className="h-in">
        {signals.length > 0 ? (
          <div className="sigs">
            {signals.map((signal) => (
              <SignalCard key={signal.id} signal={signal} weeks={weeks} />
            ))}
          </div>
        ) : null}
        <Quota block={quotaBlockOf(quota)} />
        <MemorySkips />
        <Learning />
        <section className="numbers">
          <button
            className="disc"
            type="button"
            aria-expanded={open}
            aria-controls="health-numbers"
            onClick={() => setOpen(!open)}
          >
            {open ? <ChevronDownIcon className="icon" /> : <ChevronRightIcon />}
            Show the numbers
            <span className="meta">weekly charts and the per-project breakdown</span>
          </button>
          <div id="health-numbers">{open ? <Numbers data={data} shown={shown} /> : null}</div>
        </section>
      </div>
    </div>
  );
}
