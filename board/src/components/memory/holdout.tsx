import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import type { MemoryEvalJob, MemoryEvalPlan } from "../../lib/api-types";
import { evalPlanQuery } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ulid } from "../attention/ulid";
import { showToast } from "../board/toast";
import { lisbonDay } from "../health/lisbon";
import { Modal } from "../ui/modal";
import { PlayIcon } from "./icons";
import { Inline, plural, sentence, useMemoryRefresh, when } from "./parts";

// The holdout eval the board starts (learning-loop.md section 6): the dialog
// that says what it costs before it starts, and the bar a running one shows.

// "4%", "0.5%": a whole number when it is one.
const percent = (value: number) => `${Number(value.toFixed(1))}%`;

// At least a minute: an estimate under one still takes a while.
const minutes = (value: number) => Math.max(1, Math.ceil(value));

// The plan is under the floor: the week's reset says when it can run.
function underFloor(plan: MemoryEvalPlan): boolean {
  const { left_after: after, floor } = plan.quota;
  return after !== null && floor !== null && after <= floor;
}

function Facts({ plan }: { plan: MemoryEvalPlan }) {
  const { quota } = plan;
  return (
    <dl className="ev-facts">
      <dt>Cases</dt>
      <dd>{plan.cases}</dd>
      <dt>Model runs</dt>
      <dd>
        {plan.model_runs}
        <span className="meta">
          {plural(plan.case_runs, "case run")}, {plural(plan.judge_runs, "judge run")}
        </span>
      </dd>
      <dt>Quota</dt>
      <dd>about {percent(plan.percent)} of the week</dd>
      <dt>Left after</dt>
      {quota.left_after === null ? (
        <dd>
          unknown
          <span className="meta">no current quota reading</span>
        </dd>
      ) : (
        <dd data-bad={underFloor(plan) || undefined}>
          about {percent(quota.left_after)}
          {quota.floor === null ? null : <span className="meta">floor {percent(quota.floor)}</span>}
        </dd>
      )}
      <dt>Time</dt>
      <dd>about {minutes(plan.minutes)} min</dd>
    </dl>
  );
}

// Starts the holdout eval of a proposal's change set once the owner has read
// its cost. It refuses under the quota floor, and says when the week resets.
export function EvalDialog({ proposal, onClose }: { proposal: string; onClose: () => void }) {
  const plan = useQuery(evalPlanQuery(proposal));
  const refresh = useMemoryRefresh();
  // One key for this dialog, so a second click is the same start.
  const [key] = useState(ulid);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<ActionError | null>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const data = plan.data;
  const refusal = data?.refusal ?? null;

  const start = async () => {
    setSending(true);
    setError(null);
    try {
      await sendMutation("POST", `/api/v1/memory/proposals/${proposal}/eval`, {
        idempotency_key: key,
      });
    } catch (caught) {
      setSending(false);
      setError(
        caught instanceof ActionError
          ? caught
          : new ActionError(0, "internal", "Something went wrong, so the eval did not start."),
      );
      void plan.refetch();
      return;
    }
    await refresh();
    showToast("ok", "Holdout eval started", "Keep turns on when it passes.");
    onClose();
  };

  return (
    <Modal
      className="dialog ev-dialog"
      labelledBy="eval-dialog-title"
      describedBy="eval-dialog-body"
      dismissable={!sending}
      onClose={onClose}
      initialFocus={cancel}
    >
      <h2 id="eval-dialog-title">
        {data ? `Run the holdout eval for ${data.change_set}?` : "Run the holdout eval?"}
      </h2>
      <p id="eval-dialog-body">
        It runs every holdout case against the document with this change, then a judge scores each
        one. The cost is an estimate.
      </p>
      {data ? <Facts plan={data} /> : null}
      {plan.isPending ? (
        <p className="ev-wait" role="status">
          Reading the holdout cases and the quota...
        </p>
      ) : null}
      {plan.isError ? (
        <p className="refusal" role="alert">
          Could not read what the eval costs: {plan.error.message}
        </p>
      ) : null}
      {refusal && data ? (
        <p className="refusal" role="alert">
          {sentence(`Not started: ${refusal}`)}
          {underFloor(data) && data.quota.resets_at
            ? ` The week resets ${when(data.quota.resets_at)}.`
            : null}
        </p>
      ) : null}
      {error && !refusal ? <Inline error={error} /> : null}
      <div className="dialog-actions">
        <button ref={cancel} type="button" className="btn" disabled={sending} onClick={onClose}>
          {refusal ? "Close" : "Cancel"}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={!data || refusal !== null || sending}
          onClick={() => void start()}
        >
          <PlayIcon />
          {sending ? "Starting..." : "Run eval"}
        </button>
      </div>
    </Modal>
  );
}

// The clock a running eval's bar moves on.
function useNow(every: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), every);
    return () => clearInterval(timer);
  }, [every]);
  return now;
}

// A running eval on its suggestion's card. The runner reports nothing until it
// ends, so the bar follows the time estimate, never a case count.
export function EvalRun({ job }: { job: MemoryEvalJob }) {
  const now = useNow(5000);
  const total = Math.max(1, job.minutes) * 60_000;
  const elapsed = Math.max(0, now - Date.parse(job.started_at));
  const share = Math.min(0.95, Math.max(0.03, elapsed / total));
  const left = Math.ceil((total - elapsed) / 60_000);
  return (
    <div className="ev-run" role="status">
      <span>
        Holdout eval: {plural(job.cases, "case")}, started {lisbonDay(job.started_at).time}
      </span>
      <span
        className="bar"
        role="progressbar"
        aria-label="Estimated progress"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(share * 100)}
      >
        <i style={{ width: `${share * 100}%` }} />
      </span>
      <span>{left > 0 ? `about ${left} min left` : "longer than its estimate"}</span>
    </div>
  );
}
