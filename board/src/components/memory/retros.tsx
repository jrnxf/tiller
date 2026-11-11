import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import type { RetroView, RetrosResponse } from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { API_KEY_PREFIX, RETROS_PREFIX, retrosQuery } from "../../lib/queries";
import { ActionError, sendMutation } from "../attention/api";
import { ReopenIcon } from "../attention/icons";
import { ulid } from "../attention/ulid";
import { useTick } from "../board/ago";
import { Inline, day, sentence, when } from "./parts";

// The Retros list under a project's memory document (learning-loop.md section
// 4): what each full retro of the last 30 days came to, and for one whose
// curator did not finish, why, with Run again. The sweep never runs such a
// retro again; the owner does, here or from Health.

// A running retro is read again on this beat, besides its change events, so
// the row ends even when an event is missed.
const RUNNING_POLL_MS = 5000;

export const retrosQueryWhileRunning = (project?: string) => ({
  ...retrosQuery(project),
  refetchInterval: (read: { state: { data?: RetrosResponse } }) =>
    read.state.data?.retros.some((retro) => retro.status === "running") ? RUNNING_POLL_MS : false,
});

// "Curator was cut at the 20 min limit", from the note the run ended with.
function why(learning: string | null): string {
  const cut = /^curator cut after (\d+) minutes/.exec(learning ?? "");
  if (cut) return `Curator was cut at the ${cut[1]} min limit`;
  return sentence(learning ?? "the curator did not finish").replace(/\.$/, "");
}

// "cut at the 20 min limit", for a line that names the card first.
export function whyShort(learning: string | null): string {
  const text = why(learning);
  return text.startsWith("Curator was ")
    ? text.slice("Curator was ".length)
    : `${text.charAt(0).toLowerCase()}${text.slice(1)}`;
}

const finishedAt = (retro: RetroView) => retro.finished_at ?? retro.created_at;

// "Started by you, Wed 11 Nov, 15:41 · limit 20 min".
export function startedLine(retro: RetroView, limit: number): string {
  const by = retro.started_by === "daemon" ? "the sweep" : "you";
  const started = retro.curator_started_at ? `, ${when(retro.curator_started_at)}` : "";
  return `Started by ${by}${started} · limit ${limit} min`;
}

// "0:52" since the run started.
function elapsed(from: string | null, now: number): string {
  const seconds = from ? Math.max(0, Math.floor((now - Date.parse(from)) / 1000)) : 0;
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

export function Elapsed({ from }: { from: string | null }) {
  useTick();
  return <span className="r">{elapsed(from, serverNow())}</span>;
}

const PROPOSAL: Record<string, string> = {
  accepted: "kept",
  applied: "kept",
  discarded: "discarded",
  stale: "no longer applies",
  failed: "could not be applied",
};

// What a done retro came to, without its date.
function result(retro: RetroView): string {
  if (retro.proposal) {
    const where = retro.proposal.section
      ? `waiting in ${retro.proposal.section}`
      : "waiting for you";
    return `1 suggestion, ${PROPOSAL[retro.proposal.state] ?? where}`;
  }
  if (retro.outcome === "evidence") return "1 learning noted, no suggestion";
  return "No suggestions";
}

// Run again for one retro, and the refusal it met, by retro id.
export function useRunAgain() {
  const queryClient = useQueryClient();
  const [sending, setSending] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<{ id: string; error: ActionError } | null>(null);
  const run = (retro: RetroView) => {
    setSending(retro.id);
    setRefusal(null);
    sendMutation("POST", `/api/v1/retros/${retro.id}/run`, { idempotency_key: ulid() })
      .catch((caught: unknown) => {
        setRefusal({
          id: retro.id,
          error:
            caught instanceof ActionError
              ? caught
              : new ActionError(0, "unknown", "Run again failed. Try again."),
        });
      })
      .finally(() => {
        setSending(null);
        void queryClient.invalidateQueries({
          queryKey: API_KEY_PREFIX,
          predicate: (query) => String(query.queryKey[1] ?? "").startsWith(RETROS_PREFIX),
        });
      });
  };
  return {
    run,
    sending: (retro: RetroView) => sending === retro.id,
    refusalFor: (retro: RetroView) => (refusal?.id === retro.id ? refusal.error : null),
  };
}

export function RunAgain({
  retro,
  ctl,
}: {
  retro: RetroView;
  ctl: ReturnType<typeof useRunAgain>;
}) {
  const off = retro.run_refusal !== null || ctl.sending(retro);
  return (
    <button
      type="button"
      className="btn primary sm"
      disabled={off}
      title={retro.run_refusal ?? undefined}
      aria-label={`Run the retro of ${retro.card} again`}
      onClick={() => ctl.run(retro)}
    >
      <ReopenIcon />
      Run again
    </button>
  );
}

function Row({
  retro,
  limit,
  ctl,
}: {
  retro: RetroView;
  limit: number;
  ctl: ReturnType<typeof useRunAgain>;
}) {
  const refusal = ctl.refusalFor(retro);
  let label: { text: string; tone: string };
  let meta: string;
  let right;
  switch (retro.status) {
    case "unfinished":
      label = { text: "Did not finish", tone: " bad" };
      meta = `${why(retro.learning)}, ${when(finishedAt(retro))}. The sweep does not run it again.`;
      if (retro.run_refusal) meta += ` Run again is off: ${sentence(retro.run_refusal)}`;
      right = <RunAgain retro={retro} ctl={ctl} />;
      break;
    case "running":
      label = { text: "Running", tone: " acc" };
      meta = startedLine(retro, limit);
      right = <Elapsed from={retro.curator_started_at} />;
      break;
    case "pending":
      label = { text: "Waiting", tone: " mute" };
      meta = `Scheduled ${when(retro.created_at)}. The sweep runs it.`;
      right = <span className="r">{day(retro.created_at)}</span>;
      break;
    case "done":
      label = { text: "Done", tone: " ok" };
      meta = `${result(retro)} · ${when(finishedAt(retro))}`;
      right =
        retro.proposal?.state === "proposed" ? (
          <Review proposal={retro.proposal.id} card={retro.card} />
        ) : (
          <span className="r">{day(finishedAt(retro))}</span>
        );
      break;
  }
  return (
    <li data-status={retro.status}>
      <span className="t">
        <span className={`lab sm${label.tone}`}>{label.text}</span>
        <span className="rt-card">{retro.card}</span>
        {retro.title}
      </span>
      <span className="m">{meta}</span>
      {right}
      {refusal ? (
        <div className="h-refusal">
          <Inline error={refusal} />
        </div>
      ) : null}
    </li>
  );
}

// The suggestion a done retro filed sits in the document above: Review goes
// to its card there.
function Review({ proposal, card }: { proposal: string; card: string }) {
  const toCard = () => {
    const target = document.getElementById(`sug-${proposal}`);
    target?.scrollIntoView?.({ behavior: "smooth", block: "center" });
    target?.focus({ preventScroll: true });
  };
  return (
    <button
      type="button"
      className="btn sm"
      aria-label={`Review the suggestion from ${card}`}
      onClick={toCard}
    >
      Review
    </button>
  );
}

export function Retros({ project, doc }: { project: string; doc: string }) {
  const read = useQuery(retrosQueryWhileRunning(project));
  const ctl = useRunAgain();
  if (!read.data) return null;
  const { retros, curator_timeout_minutes: limit } = read.data;
  return (
    <section className="h-box rt-box" aria-labelledby="retros-title">
      <div className="h-head">
        <h2 id="retros-title">Retros</h2>
        <span className="c">{doc} · last 30 days</span>
      </div>
      {retros.length === 0 ? (
        <p className="h-none">No retros in the last 30 days.</p>
      ) : (
        <ul className="h-list">
          {retros.map((retro) => (
            <Row key={retro.id} retro={retro} limit={limit} ctl={ctl} />
          ))}
        </ul>
      )}
    </section>
  );
}
