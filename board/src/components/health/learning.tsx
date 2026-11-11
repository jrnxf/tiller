import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import type { RetroView } from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { WarnIcon } from "../board/icons";
import { plural, sentence, when } from "../memory/parts";
import {
  Elapsed,
  RunAgain,
  RunAgainNote,
  retrosQueryWhileRunning,
  startedLine,
  useRunAgain,
  whyShort,
} from "../memory/retros";
import { ClockIcon } from "../memory/icons";
import { CheckIcon } from "../shell/icons";
import { lisbonWeekLabel } from "./lisbon";

const WEEK_MS = 7 * 24 * 60 * 60 * 1000;

// Finished in this Lisbon week (Monday to Sunday).
function thisWeek(iso: string | null, now: number): boolean {
  if (iso === null) return false;
  const at = Date.parse(iso);
  return (
    now - at < WEEK_MS && lisbonWeekLabel(iso) === lisbonWeekLabel(new Date(now).toISOString())
  );
}

const KEPT = new Set(["accepted", "applied"]);

// The newest retro that did not finish, with Open in Memory and Run again.
function Unfinished({ newest, count }: { newest: RetroView; count: number }) {
  const run = useRunAgain(newest);
  return (
    <li data-kind="unfinished">
      <span className="si bad">
        <WarnIcon />
      </span>
      <span className="t">{plural(count, "retro")} did not finish</span>
      <span className="m">
        {newest.card} · {whyShort(newest.learning)}, {when(newest.finished_at ?? newest.created_at)}
        {count > 1 ? ` · and ${count - 1} more in Memory` : ""}
        {newest.run_refusal ? `. Run again is off: ${sentence(newest.run_refusal)}` : ""}
      </span>
      <span className="acts">
        <Link className="btn" to="/memory" search={{ doc: newest.project_id }}>
          Open in Memory
        </Link>
        <RunAgain retro={newest} run={run} />
      </span>
      <RunAgainNote run={run} className="ln-refusal" />
    </li>
  );
}

// The Learning panel (learning-loop.md section 4): a retro whose curator did
// not finish, with Open in Memory and Run again, what runs now, and what the
// retros of this week came to.
export function Learning() {
  const read = useQuery(retrosQueryWhileRunning());
  if (!read.data) return null;
  const { retros, curator_timeout_minutes: limit } = read.data;
  const now = serverNow();
  const unfinished = retros.filter((retro) => retro.status === "unfinished");
  const running = retros.filter((retro) => retro.status === "running");
  const done = retros.filter(
    (retro) => retro.status === "done" && thisWeek(retro.finished_at, now),
  );
  const suggested = done.filter((retro) => retro.proposal !== null);
  const kept = suggested.filter((retro) => KEPT.has(retro.proposal?.state ?? ""));
  const newest: RetroView | undefined = unfinished[0];

  return (
    <section className="cpanel learning" aria-labelledby="learning-title">
      <div className="ch">
        <div>
          <h3 id="learning-title">Learning</h3>
          <p className="cs">Retros and the curator, this week.</p>
        </div>
      </div>
      <ul className="ln-list">
        {newest ? <Unfinished newest={newest} count={unfinished.length} /> : null}
        {running.map((retro) => (
          <li key={retro.id} data-kind="running">
            <span className="si acc">
              <ClockIcon />
            </span>
            <span className="t">The retro of {retro.card} runs</span>
            <span className="m">{startedLine(retro, limit)}</span>
            <span className="acts">
              <Elapsed from={retro.curator_started_at} />
            </span>
          </li>
        ))}
        <li data-kind="done">
          <span className="si ok">
            <CheckIcon />
          </span>
          <span className="t">
            {done.length === 0 ? "No retros done" : `${plural(done.length, "retro")} done`}
          </span>
          <span className="m">
            {suggested.length === 0
              ? "No suggestions"
              : `${plural(suggested.length, "suggestion")}, ${kept.length} kept`}
          </span>
        </li>
      </ul>
    </section>
  );
}
