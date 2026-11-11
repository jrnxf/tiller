import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import type { ReactNode } from "react";

import type { MutationResponse, RetroView, RetrosResponse } from "../../lib/api-types";
import { serverNow } from "../../lib/clock";
import { API_KEY_PREFIX, RETROS_PREFIX, retrosQuery } from "../../lib/queries";
import type { ActionError } from "../attention/api";
import { sendMutation } from "../attention/api";
import { ReopenIcon } from "../attention/icons";
import { useAction } from "../attention/use-action";
import { useTick } from "../board/ago";
import { day, sentence, when } from "./parts";

// The Retros list under a project's memory document (learning-loop.md section
// 4): what each full retro of the last 30 days came to, every one whose
// curator did not finish, why, with Run again. Nothing runs such a retro
// again by itself; the owner does, here or from Health.

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
  const by = retro.started_by === "daemon" ? "the daemon" : "you";
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

// What `POST /api/v1/retros/{id}/run` answers with: the retro its key claimed.
interface Claimed {
  state: "pending" | "done";
  outcome: string | null;
  finished_at: string | null;
}

// A row's state as Run again sees it: a refusal or a note it met holds only
// while this stays the same.
const rowState = (retro: RetroView) =>
  `${retro.id}:${retro.status}:${retro.finished_at ?? ""}:${retro.run_refusal ?? ""}`;

export type RunAgainState = ReturnType<typeof useRunAgain>;

// Run again for one retro. The key is minted once per run that did not
// finish (the retro and when it ended), so a retry after a lost answer
// replays the same click and starts no second curator, and a run that ended
// since is a new click. A click is recorded once: the button stays off until
// the list shows where it went.
export function useRunAgain(retro: RetroView) {
  const queryClient = useQueryClient();
  const state = rowState(retro);
  const [replayed, setReplayed] = useState<Claimed | null>(null);
  const [failed, setFailed] = useState<{ error: ActionError; state: string } | null>(null);
  const id = retro.id;
  const send = useCallback(
    (idempotency_key: string) => {
      setReplayed(null);
      // The button turns on again only once the list says what happened.
      const refresh = () =>
        queryClient.invalidateQueries({
          queryKey: API_KEY_PREFIX,
          predicate: (query) => String(query.queryKey[1] ?? "").startsWith(RETROS_PREFIX),
        });
      return sendMutation<Claimed>("POST", `/api/v1/retros/${id}/run`, {
        idempotency_key,
      }).finally(refresh);
    },
    [id, queryClient],
  );
  const onRecorded = useCallback((response: MutationResponse<Claimed>) => {
    // A replayed key answers with the retro it claimed then. One that is done
    // ended that run already: say so, since the row looks as it did.
    if (response.replayed && response.data?.state === "done") setReplayed(response.data);
  }, []);
  const action = useAction({
    binding: `${retro.id}:${retro.finished_at ?? ""}`,
    send,
    onRecorded,
  });
  // A failure belongs to the row as it was when it came back.
  if (action.error && failed?.error !== action.error) {
    setFailed({ error: action.error, state });
  }
  const error =
    action.error && failed?.error === action.error && failed.state === state
      ? sentence(action.error.message)
      : null;
  const ranAlready =
    replayed !== null &&
    retro.status === "unfinished" &&
    replayed.outcome === "unfinished" &&
    replayed.finished_at !== null &&
    Date.parse(replayed.finished_at) === Date.parse(retro.finished_at ?? "");
  return {
    trigger: action.trigger,
    off: retro.run_refusal !== null || action.pending || action.recorded,
    error,
    note: ranAlready
      ? "Your earlier Run again had already run, and it did not finish either."
      : null,
  };
}

export function RunAgain({ retro, run }: { retro: RetroView; run: RunAgainState }) {
  return (
    <button
      type="button"
      className="btn primary sm"
      disabled={run.off}
      title={retro.run_refusal ?? undefined}
      aria-label={`Run the retro of ${retro.card} again`}
      onClick={run.trigger}
    >
      <ReopenIcon />
      Run again
    </button>
  );
}

// What Run again met, under its row: a refusal, or a click that had run.
export function RunAgainNote({ run, className }: { run: RunAgainState; className: string }) {
  if (!run.error && !run.note) return null;
  return (
    <div className={className}>
      <p className="refusal inline" role={run.error ? "alert" : "status"}>
        {run.error ?? run.note}
      </p>
    </div>
  );
}

// The command that runs a waiting retro's curator now, by the retro's short id.
const runCommand = (retro: RetroView) => `tiller retro run ${retro.id.slice(-7).toLowerCase()}`;

function Unfinished({ retro }: { retro: RetroView }) {
  const run = useRunAgain(retro);
  let meta = `${why(retro.learning)}, ${when(finishedAt(retro))}. Nothing runs it again by itself.`;
  if (retro.run_refusal) meta += ` Run again is off: ${sentence(retro.run_refusal)}`;
  return (
    <li data-status={retro.status}>
      <RowTitle retro={retro} label={{ text: "Did not finish", tone: " bad" }} />
      <span className="m">{meta}</span>
      <RunAgain retro={retro} run={run} />
      <RunAgainNote run={run} className="h-refusal" />
    </li>
  );
}

function RowTitle({ retro, label }: { retro: RetroView; label: { text: string; tone: string } }) {
  return (
    <span className="t">
      <span className={`lab sm${label.tone}`}>{label.text}</span>
      <span className="rt-card">{retro.card}</span>
      {retro.title}
    </span>
  );
}

function Row({ retro, limit }: { retro: RetroView; limit: number }) {
  let label: { text: string; tone: string };
  let meta: ReactNode;
  let right;
  switch (retro.status) {
    case "unfinished":
      return <Unfinished retro={retro} />;
    case "running":
      label = { text: "Running", tone: " acc" };
      meta = startedLine(retro, limit);
      right = <Elapsed from={retro.curator_started_at} />;
      break;
    case "pending":
      // Nothing starts a waiting full retro's curator: the owner does.
      label = { text: "Waiting", tone: " mute" };
      meta = (
        <>
          Waiting since {when(retro.created_at)}. Nothing starts its curator by itself: run{" "}
          <code>{runCommand(retro)}</code> in a terminal.
        </>
      );
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
      <RowTitle retro={retro} label={label} />
      <span className="m">{meta}</span>
      {right}
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

const WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

// A row the 30 days do not hold: an unfinished retro is listed however old.
function older(retro: RetroView, now: number): boolean {
  const times = [retro.created_at, retro.finished_at, retro.curator_started_at];
  return Math.max(...times.map((at) => (at ? Date.parse(at) : 0))) < now - WINDOW_MS;
}

export function Retros({ project, doc }: { project: string; doc: string }) {
  const read = useQuery(retrosQueryWhileRunning(project));
  if (!read.data) return null;
  const { retros, curator_timeout_minutes: limit } = read.data;
  const now = serverNow();
  const span = retros.some((retro) => older(retro, now))
    ? "last 30 days and every unfinished one"
    : "last 30 days";
  return (
    <section className="h-box rt-box" aria-labelledby="retros-title">
      <div className="h-head">
        <h2 id="retros-title">Retros</h2>
        <span className="c">{`${doc} · ${span}`}</span>
      </div>
      {retros.length === 0 ? (
        <p className="h-none">No retros in the last 30 days.</p>
      ) : (
        <ul className="h-list">
          {retros.map((retro) => (
            <Row key={retro.id} retro={retro} limit={limit} />
          ))}
        </ul>
      )}
    </section>
  );
}
