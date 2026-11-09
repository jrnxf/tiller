import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

import type { AgentRow, RestartStaleResponse } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { ProjectDialog } from "../shell/project-dialog";
import { WarnIcon } from "../shell/icons";

// A session started on an older tiller keeps that release's role prompt and
// hooks until it restarts. Only this host's sessions count: a peer's rows
// carry that peer's own staleness and restart there.
interface StaleSession {
  agent: AgentRow;
  name: string;
  idle: boolean;
}

function sessionName(agent: AgentRow, project: string | undefined): string {
  if (agent.role === "orchestrator") return `${project ?? "project"} orchestrator`;
  return `${agent.card_id ?? agent.session_id} ${agent.role}`;
}

function staleSessions(orchestrators: AgentRow[]): StaleSession[] {
  const out: StaleSession[] = [];
  for (const orchestrator of orchestrators) {
    for (const agent of [orchestrator, ...orchestrator.workers]) {
      if (!agent.stale || agent.link) continue;
      out.push({
        agent,
        name: sessionName(agent, orchestrator.project_id),
        idle: agent.activity === "idle",
      });
    }
  }
  return out;
}

// "0.1.0-pre.20" reads "pre.20" in a chip.
function shortVersion(version: string): string {
  const at = version.indexOf("-");
  return at >= 0 ? version.slice(at + 1) : version;
}

export function StaleChip({ agent }: { agent: AgentRow }) {
  const current = useQuery(agentsQuery()).data?.tiller_version;
  const stale = agent.stale;
  if (!stale || agent.link) return null;
  const started = `Started on ${stale.since === "unknown" ? "an unknown version" : stale.since}.`;
  const tip = stale.restart_queued
    ? `${started} Restarts when it next goes idle.`
    : `${started}${current ? ` Current is ${current}.` : ""}${stale.prompt_changed ? " Role prompt changed." : ""}`;
  return (
    <span className="stale-chip" title={tip}>
      older tiller · {shortVersion(stale.since)}
    </span>
  );
}

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? "" : "s"}`;

function RestartDialog({ sessions, onClose }: { sessions: StaleSession[]; onClose: () => void }) {
  const queryClient = useQueryClient();
  const [result, setResult] = useState<RestartStaleResponse | null>(null);
  const ids = sessions.map(({ agent }) => agent.session_id);
  const send = useCallback(
    (idempotency_key: string) =>
      sendMutation<RestartStaleResponse>("POST", "/api/v1/agents/restart-stale", {
        sessions: ids,
        idempotency_key,
      }),
    // The ids at the click are what is asked: the list below may move after.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [ids.join(",")],
  );
  const restart = useAction({
    binding: "stale-restart",
    send,
    onRecorded: (response) => {
      setResult(response.data);
      void queryClient.invalidateQueries({ queryKey: agentsQuery().queryKey });
    },
  });
  const cancel = useRef<HTMLButtonElement>(null);
  const outcomes = new Map(result?.sessions.map((entry) => [entry.session_id, entry]));
  const done = result !== null;
  // Rows keep to the sessions asked, so a restarted one stays on screen.
  const [asked] = useState(sessions);
  const rows = done ? asked : sessions;

  return (
    <ProjectDialog
      name="stale"
      pending={restart.pending}
      onClose={onClose}
      initial={cancel}
      className="stale-dialog"
    >
      <h2 id="stale-dialog-title">Restart {plural(rows.length, "older session")}</h2>
      <p id="stale-dialog-body">
        Each one re-reads its handoff on start, so it loses only its chat context. A session that is
        working restarts when it next goes idle.
      </p>
      <div className="pdialog-card">
        <ul className="stale-rows" aria-label="Restart older sessions">
          {rows.map(({ agent, name, idle }) => {
            const outcome = outcomes.get(agent.session_id);
            return (
              <li key={agent.session_id}>
                <i className={`ld ${idle ? "idle" : "live"}`} />
                <span className="nm">{name}</span>
                <small>
                  {outcome
                    ? outcome.outcome === "restarted"
                      ? "restarted ✓"
                      : outcome.outcome === "queued"
                        ? "queued · restarts when idle"
                        : (outcome.error ?? "could not restart")
                    : idle
                      ? "idle · restarts now"
                      : "working · restarts when idle"}
                </small>
              </li>
            );
          })}
        </ul>
      </div>
      {restart.error ? (
        <p className="refusal" role="alert">
          {restart.error.message}
        </p>
      ) : null}
      <div className="dialog-actions">
        {done ? (
          <button ref={cancel} type="button" className="btn primary" onClick={onClose}>
            Done
          </button>
        ) : (
          <>
            <button
              ref={cancel}
              type="button"
              className="btn"
              disabled={restart.pending}
              onClick={onClose}
            >
              Cancel
            </button>
            <button
              type="button"
              className="btn primary"
              disabled={restart.pending}
              onClick={restart.trigger}
            >
              {restart.pending ? "Restarting..." : "Restart"}
            </button>
          </>
        )}
      </div>
    </ProjectDialog>
  );
}

// Above the board: how many sessions run an older tiller, and the review that
// restarts them. Once every one is restarted or queued it says what waits.
export function StaleBanner() {
  const roster = useQuery(agentsQuery()).data;
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const all = staleSessions(roster?.orchestrators ?? []);
  const unqueued = all.filter(({ agent }) => !agent.stale?.restart_queued);
  const queued = all.filter(({ agent }) => agent.stale?.restart_queued);
  const close = useCallback(() => {
    setOpen(false);
    opener.current?.focus();
  }, []);
  if (all.length === 0) return null;
  return (
    <div className="banner warn" role="status" aria-live="polite" data-testid="stale-banner">
      <WarnIcon />
      <div className="tx">
        {unqueued.length > 0 ? (
          <>
            <b>
              {unqueued.length === 1
                ? "1 session runs an older tiller."
                : `${unqueued.length} sessions run an older tiller.`}
            </b>{" "}
            Tiller is now {roster?.tiller_version ?? "newer"}. Restarting them picks up the new
            hooks and role prompt.
          </>
        ) : (
          <>
            <b>
              {queued.length === 1
                ? "1 session will restart when it goes idle."
                : `${queued.length} sessions will restart when they go idle.`}
            </b>{" "}
            {queued.map(({ name }) => name).join(", ")} {queued.length === 1 ? "is" : "are"} still
            working.
          </>
        )}
      </div>
      {unqueued.length > 0 ? (
        <button ref={opener} type="button" className="btn" onClick={() => setOpen(true)}>
          Review and restart
        </button>
      ) : null}
      {open ? <RestartDialog sessions={unqueued} onClose={close} /> : null}
    </div>
  );
}
