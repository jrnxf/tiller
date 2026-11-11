import { formatLisbonDate } from "../../lib/clock";
import type { ActionError } from "../attention/api";

// What the daemon sends in `error.current` when the Stop hook's switching rule
// refuses block-once (`protocol.md` 2.5): the same numbers
// `tiller intake stats` prints, and the window they cover.
export interface SwitchingRuleNumbers {
  days: number;
  stops: number;
  would_block: number;
  false_blocks: number;
  false_block_rate: number | null;
  min_stops: number;
  max_false_block_rate: number;
  since: string;
  until: string;
}

const number = (value: unknown): value is number => typeof value === "number";

// The numbers behind a refusal, or null for any other refusal.
export function switchingRuleOf(error: ActionError): SwitchingRuleNumbers | null {
  const current = error.current;
  if (error.code !== "refused" || !current) return null;
  const { days, stops, would_block, false_blocks, min_stops, max_false_block_rate } = current;
  if (
    !number(days) ||
    !number(stops) ||
    !number(would_block) ||
    !number(false_blocks) ||
    !number(min_stops) ||
    !number(max_false_block_rate) ||
    typeof current.since !== "string" ||
    typeof current.until !== "string"
  ) {
    return null;
  }
  return {
    days,
    stops,
    would_block,
    false_blocks,
    false_block_rate: number(current.false_block_rate) ? current.false_block_rate : null,
    min_stops,
    max_false_block_rate,
    since: current.since,
    until: current.until,
  };
}

const percent = (rate: number) => `${(rate * 100).toFixed(1).replace(/\.0$/, "")}%`;

// Why block once is refused now, in the rule's own words and with this
// week's numbers beside what the rule needs.
export function SwitchingRule({ rule }: { rule: SwitchingRuleNumbers }) {
  const short = rule.stops < rule.min_stops;
  const over = rule.false_block_rate !== null && rule.false_block_rate >= rule.max_false_block_rate;
  const from = formatLisbonDate(rule.since);
  const to = formatLisbonDate(rule.until);
  return (
    <>
      <p>
        Not saved: block once needs {rule.min_stops} orchestrator stops in the last {rule.days} days
        with a false-block rate under {percent(rule.max_false_block_rate)}.
      </p>
      <dl className="set-facts" data-testid="switching-rule">
        <dt>Stops</dt>
        <dd className={short ? "bad" : undefined}>
          {rule.stops}
          {short ? <span className="meta">need {rule.min_stops}</span> : null}
        </dd>
        <dt>Would block</dt>
        <dd>{rule.would_block}</dd>
        <dt>False blocks</dt>
        <dd className={over ? "bad" : undefined}>
          {rule.false_blocks}
          {rule.false_block_rate === null ? (
            <span className="meta">no stop would have blocked yet</span>
          ) : (
            <>
              <span className="meta">{percent(rule.false_block_rate)} of those</span>
              {over ? (
                <span className="meta">need under {percent(rule.max_false_block_rate)}</span>
              ) : null}
            </>
          )}
        </dd>
        <dt>Window</dt>
        <dd>
          {from} to {to}
        </dd>
      </dl>
    </>
  );
}
