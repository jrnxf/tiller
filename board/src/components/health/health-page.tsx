import { useSuspenseQuery } from "@tanstack/react-query";
import { useState } from "react";

import type { SignalsResponse } from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { prefsStore, visibleProjects } from "../../lib/prefs";
import { projectColor } from "../../lib/project-color";
import { projectsQuery, quotaQuery, signalsQuery } from "../../lib/queries";
import { useStore } from "../../lib/store";
import { ChevronDownIcon, ChevronRightIcon, WarnIcon } from "../board/icons";
import { CheckIcon } from "../shell/icons";
import { BarChart, LineChart, Spark } from "./charts";
import type { Point } from "./charts";
import { lisbonShortDay, lisbonWeekLabel } from "./lisbon";
import { accepted, projectRows, quotaBlockOf, signalsOf, verdictOf } from "./model";
import type { QuotaBlock, Signal } from "./model";
import "../board/board.css";
import "./health.css";

const STATE_LABEL = { good: "Good", watch: "Watch" } as const;

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
      <Spark label={signal.title} values={signal.values} weeks={weeks} />
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
  const used = block.percentUsed === null ? null : Math.round(block.percentUsed);
  const elapsed = block.elapsedPercent === null ? null : Math.round(block.elapsedPercent);
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
          className={`qmeter big${used === null ? " unk" : ""}`}
          role="img"
          aria-label={
            used === null
              ? "Weekly use is not reported"
              : `${used}% used${elapsed === null ? "" : `, ${elapsed}% of the week gone`}`
          }
        >
          {used === null ? null : <i style={{ width: `${block.percentUsed}%` }} />}
          {elapsed === null ? null : <u style={{ left: `${block.elapsedPercent}%` }} />}
        </div>
        <div className="q-key">
          {used === null ? null : (
            <span>
              <i className="k-fill" />
              {used}% used
            </span>
          )}
          {elapsed === null ? null : (
            <span>
              <i className="k-tick" />
              {elapsed}% of the week gone
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
                  <i style={{ width: `${100 - other.percentLeft}%` }} />
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

const ratio = (value: number) => Number(value.toFixed(2)).toString();
const whole = (value: number) => String(Math.round(value));
const tenth = (value: number) => Number(value.toFixed(1)).toString();

function Numbers({ data, shown }: { data: SignalsResponse; shown: Set<string> }) {
  const projects = useSuspenseQuery(projectsQuery()).data.projects;
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
        </div>
        <div className="cpanel">
          <div className="ch">
            <div>
              <h3>Times you were needed per accepted outcome</h3>
              <p className="cs">Times you were needed, divided by outcomes accepted that week.</p>
            </div>
          </div>
          <BarChart
            title="Times you were needed per accepted outcome"
            points={series((week) => (accepted(week) > 0 ? week.needed / accepted(week) : null))}
            format={ratio}
            unit="per outcome"
          />
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
              <th className="r">Needed per outcome</th>
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
                    </td>
                    <td className="r">{row.escaped}</td>
                    <td className="r">
                      {row.median === null ? "none" : `${tenth(row.median)} min`}
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function HealthPage() {
  const data = useSuspenseQuery(signalsQuery()).data;
  const quota = useSuspenseQuery(quotaQuery()).data;
  const projects = useSuspenseQuery(projectsQuery()).data.projects;
  const prefs = useStore(prefsStore);
  const [open, setOpen] = useState(false);
  const now = serverNow();
  const signals = signalsOf(data, now);
  const verdict = verdictOf(signals, data, now);
  const weeks = data.weeks.map((week) => ({
    label: lisbonWeekLabel(week.start),
    start: lisbonShortDay(week.start),
  }));
  const shown = new Set(visibleProjects(prefs, projects).map((project) => project.id));

  return (
    <div className="health" data-testid="health">
      <div className="h-in">
        {verdict ? (
          <section
            className={`verdict ${verdict.tone === "bad" ? "bad" : ""}`}
            aria-label="Verdict"
          >
            <span className="v-ic">{verdict.tone === "bad" ? <WarnIcon /> : <CheckIcon />}</span>
            <div>
              <h2>
                <span>{verdict.head}</span>
                {verdict.text}
              </h2>
              <p>{verdict.watch}</p>
            </div>
          </section>
        ) : (
          <section className="verdict" aria-label="Verdict">
            <span className="v-ic">
              <CheckIcon />
            </span>
            <div>
              <h2>
                <span>Nothing to judge yet.</span> No week has been recorded.
              </h2>
            </div>
          </section>
        )}
        {signals.length > 0 ? (
          <div className="sigs">
            {signals.map((signal) => (
              <SignalCard key={signal.id} signal={signal} weeks={weeks} />
            ))}
          </div>
        ) : null}
        <Quota block={quotaBlockOf(quota)} />
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
