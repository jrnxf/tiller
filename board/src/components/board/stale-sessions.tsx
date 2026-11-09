import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

import type { AgentRow, Machine, RestartStaleResponse } from "../../lib/api-types";
import { VIEW_ONLY_REASON, isPeerRow, peerMachines, standingOf } from "../../lib/peers";
import { agentsQuery } from "../../lib/queries";
import { sendMutation } from "../attention/api";
import { useAction } from "../attention/use-action";
import { ProjectDialog } from "../shell/project-dialog";
import { WarnIcon } from "../shell/icons";
import { useMachineRows } from "./peer-state";

// A session started on an older tiller keeps that release's role prompt and
// hooks until it restarts. A peer's rows carry that peer's own staleness,
// judged against the peer's tiller, so they count here too. The restart runs
// on the peer when the pairing allows it, and otherwise has to be done there.
interface StaleSession {
  agent: AgentRow;
  name: string;
  idle: boolean;
  // The peer's label; null for this host's own session.
  host: string | null;
  // Whether this board can restart it: this host's own, or a live `act` peer.
  canAct: boolean;
  // Why it cannot, as the banner says it.
  reason: string | null;
}

function sessionName(agent: AgentRow, project: string | undefined): string {
  const host = isPeerRow(agent) && agent.host ? ` on ${agent.host}` : "";
  if (agent.role === "orchestrator") return `${project ?? "project"} orchestrator${host}`;
  return `${agent.card_id ?? agent.session_id} ${agent.role}${host}`;
}

function staleSessions(orchestrators: AgentRow[], machines: Machine[]): StaleSession[] {
  const out: StaleSession[] = [];
  for (const orchestrator of orchestrators) {
    for (const agent of [orchestrator, ...orchestrator.workers]) {
      if (!agent.stale) continue;
      const standing = standingOf(agent, machines);
      out.push({
        agent,
        name: sessionName(agent, orchestrator.project_id),
        idle: agent.activity === "idle",
        host: standing.peer ? standing.host : null,
        canAct: standing.canAct,
        reason: standing.reason,
      });
    }
  }
  return out;
}

const versionOf = (machines: Machine[], host: string) =>
  peerMachines(machines).find((machine) => machine.label === host)?.version;

// "0.1.0-pre.20" reads "pre.20" in a chip.
function shortVersion(version: string): string {
  const at = version.indexOf("-");
  return at >= 0 ? version.slice(at + 1) : version;
}

export function StaleChip({ agent }: { agent: AgentRow }) {
  const machines = useMachineRows();
  const local = useQuery(agentsQuery()).data?.tiller_version;
  const stale = agent.stale;
  if (!stale) return null;
  // A peer's session is judged against that peer's tiller.
  const current = isPeerRow(agent) && agent.host ? versionOf(machines, agent.host) : local;
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
    async (idempotency_key: string) => {
      // One restart per host: this host's own, then each peer's, relayed to it.
      const hosts = [...new Set(sessions.map(({ host }) => host))];
      const answers = [];
      for (const host of hosts) {
        answers.push(
          await sendMutation<RestartStaleResponse>("POST", "/api/v1/agents/restart-stale", {
            sessions: sessions
              .filter((entry) => entry.host === host)
              .map(({ agent }) => agent.session_id),
            idempotency_key: host ? `${idempotency_key}:${host}` : idempotency_key,
            ...(host ? { host } : {}),
          }),
        );
      }
      const [first] = answers;
      if (!first) throw new Error("nothing to restart");
      return { ...first, data: { sessions: answers.flatMap((answer) => answer.data.sessions) } };
    },
    // The sessions at the click are what is asked: the list below may move after.
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

// "1 on host-a, 2 on host-b": where the peers' older sessions are.
function onHosts(sessions: StaleSession[]): string {
  const counts = new Map<string, number>();
  for (const { host } of sessions) if (host) counts.set(host, (counts.get(host) ?? 0) + 1);
  return [...counts].map(([host, count]) => `${count} on ${host}`).join(", ");
}

// What a session this board cannot restart says: the restart has to happen on
// its own host.
function elsewhere(sessions: StaleSession[]): string[] {
  const hosts = new Map<string, string | null>();
  for (const { host, reason } of sessions) if (host) hosts.set(host, reason);
  return [...hosts].map(([host, reason]) =>
    reason === VIEW_ONLY_REASON
      ? `${host} is paired view only, so restart its sessions on ${host}.`
      : `${reason ?? `${host} is not reachable`}, so its sessions wait there.`,
  );
}

// Above the board: how many sessions run an older tiller, and the review that
// restarts them. Once every one is restarted or queued it says what waits.
export function StaleBanner() {
  const roster = useQuery(agentsQuery()).data;
  const machines = useMachineRows();
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const all = staleSessions(roster?.orchestrators ?? [], machines);
  const unqueued = all.filter(({ agent }) => !agent.stale?.restart_queued);
  const queued = all.filter(({ agent }) => agent.stale?.restart_queued);
  const actionable = unqueued.filter(({ canAct }) => canAct);
  const stuck = unqueued.filter(({ canAct }) => !canAct);
  const close = useCallback(() => {
    setOpen(false);
    opener.current?.focus();
  }, []);
  if (all.length === 0) return null;
  const peers = onHosts(unqueued);
  const local = unqueued.some(({ host }) => !host);
  const peerVersions = [...new Set(unqueued.flatMap(({ host }) => (host ? [host] : [])))].flatMap(
    (host) => {
      const version = versionOf(machines, host);
      return version ? [`${host} runs ${version}.`] : [];
    },
  );
  return (
    <div className="banner warn" role="status" aria-live="polite" data-testid="stale-banner">
      <WarnIcon />
      <div className="tx">
        {unqueued.length > 0 ? (
          <>
            <b>
              {unqueued.length === 1
                ? "1 session runs an older tiller"
                : `${unqueued.length} sessions run an older tiller`}
              {peers ? ` (${peers})` : ""}.
            </b>{" "}
            {local ? `Tiller is now ${roster?.tiller_version ?? "newer"}. ` : ""}
            {peerVersions.join(" ")}
            {peerVersions.length > 0 ? " " : ""}
            {actionable.length > 0
              ? "Restarting them picks up the new hooks and role prompt."
              : "Restarting them picks up the new hooks and role prompt, on their own host."}
            {stuck.length > 0 ? ` ${elsewhere(stuck).join(" ")}` : ""}
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
      {actionable.length > 0 ? (
        <button ref={opener} type="button" className="btn" onClick={() => setOpen(true)}>
          Review and restart
        </button>
      ) : null}
      {open ? <RestartDialog sessions={actionable} onClose={close} /> : null}
    </div>
  );
}
