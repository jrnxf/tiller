import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as KeyEvent, RefObject } from "react";

import type { AgentRow, Machine, PermissionPrompt, Project } from "../../lib/api-types";
import { usePageFullScreen } from "../../lib/fullscreen";
import { focusReason, ownerOf, projectKey, shownId } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import {
  agentsQuery,
  attentionQuery,
  boardQuery,
  machinesQuery,
  projectsQuery,
  runQuery,
} from "../../lib/queries";
import { prefsStore } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { useStore } from "../../lib/store";
import { hostAwareActivity } from "../states/degraded";
import { quietFor } from "./activity";
import { Ago, Lasting, useTick } from "./ago";
import { agentGroups, findLine, nextNeeding, stepLine } from "./agents-model";
import type { AgentGroup, AgentLine } from "./agents-model";
import { AnswerStrip } from "./answer-strip";
import { EmptyScope } from "./board-view";
import { formatAge, formatLisbonMoment } from "./format";
import { focusCommand, focusTooltip, useHerdrFocus, useOpenKey } from "./herdr";
import { ChevronLeftIcon, InfoIcon, PaneIcon } from "./icons";
import { LiveTerminal, isHolderPane, usePhone } from "./live-terminal";
import type { SwitchKey, TerminalControl } from "./live-terminal";
import { AsOf, HostChip, PeerNote } from "./host-chip";
import { useAmbiguousIds, useStanding, useNamedProjects } from "./peer-state";
import { StaleChip } from "./stale-sessions";
import { NO_RUN, NOT_WORKING, SteerBar } from "./steer-bar";
import { RestartOrchestratorDialog } from "./orchestrator-start";
import {
  PausedNote,
  ResumedNote,
  RunDialogs,
  RunMenu,
  runStateOf,
  useGoOn,
  useResumeSession,
} from "./run-menu";
import type { RunDialog, RunTarget } from "./run-menu";
import { PlayIcon, ReopenIcon } from "../attention/icons";
import { ModelChip, ownerSearch } from "./tile";
import { BoardIcon, CloseIcon, ServerIcon } from "../shell/icons";
import "./board.css";
import "./agents.css";

const ROLE_COLOR: Record<string, string> = {
  orchestrator: "var(--orange)",
  scout: "var(--blue)",
  build: "var(--green)",
  ship: "var(--green)",
  review: "var(--indigo)",
  verify: "var(--teal)",
};

// A motion curve from styles.css, for the Web Animations API.
function ease(token: "--ease-out" | "--ease-drawer"): string {
  return getComputedStyle(document.documentElement).getPropertyValue(token).trim() || "ease-out";
}

// How the selection last moved. A click glides and fades; a key moves at
// once, since keys move it many times a minute.
type Via = "pointer" | "key";

function reducedMotion(): boolean {
  return (
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

// Runs an animation where the browser has the Web Animations API.
function play(
  element: Element | null | undefined,
  keyframes: Keyframe[],
  options: KeyframeAnimationOptions,
): Animation | null {
  if (!element || typeof element.animate !== "function") return null;
  return element.animate(keyframes, options);
}

// The rail row of agent `id`.
function rowOf(list: HTMLElement | null, id: string | undefined): HTMLElement | undefined {
  return [...(list?.querySelectorAll<HTMLElement>("[data-session]") ?? [])].find(
    (row) => row.dataset.session === id,
  );
}

// Plays a one-off CSS animation by class, again from the start if it runs.
function flash(element: Element | null, name: string) {
  if (!(element instanceof HTMLElement)) return;
  element.classList.remove(name);
  void element.offsetWidth;
  element.classList.add(name);
  element.addEventListener("animationend", () => element.classList.remove(name), { once: true });
}

function RoleLabel({ role }: { role: string }) {
  return (
    <span className="lab sm">
      <span className="dot" style={{ background: ROLE_COLOR[role] ?? "var(--fg-4)" }} />
      {role}
    </span>
  );
}

// A count that rolls to its new value: up when it grows, down when it shrinks.
function Roll({ value }: { value: number }) {
  const box = useRef<HTMLSpanElement>(null);
  const last = useRef(value);
  useLayoutEffect(() => {
    const was = last.current;
    last.current = value;
    const element = box.current;
    if (!element || was === value || reducedMotion() || typeof element.animate !== "function") {
      return;
    }
    const old = document.createElement("span");
    old.textContent = String(was);
    old.setAttribute("aria-hidden", "true");
    element.appendChild(old);
    const up = value > was;
    const options = { duration: 200, easing: ease("--ease-out"), fill: "forwards" as const };
    const out = old.animate(
      [
        { transform: "none", opacity: 1 },
        { transform: `translateY(${up ? -100 : 100}%)`, opacity: 0 },
      ],
      options,
    );
    out.onfinish = () => old.remove();
    element.firstElementChild?.animate(
      [
        { transform: `translateY(${up ? 100 : -100}%)`, opacity: 0 },
        { transform: "none", opacity: 1 },
      ],
      options,
    );
  }, [value]);
  return (
    <span className="av-cnt" ref={box}>
      <span>{value}</span>
    </span>
  );
}

function promptText(prompt: PermissionPrompt | undefined): string | null {
  if (!prompt) return null;
  return prompt.detail ? `${prompt.tool}: ${prompt.detail}` : `${prompt.tool} asks for permission`;
}

// How long the row has been in its state, or, for a working agent that has
// gone quiet, how long since its last output.
function RowAge({ agent, live }: { agent: AgentRow; live: boolean }) {
  useTick();
  const quiet = live ? quietFor(agent) : null;
  if (quiet !== null) {
    return (
      <span className="qt" title={`Working, no output for ${formatAge(quiet)}`}>
        quiet {formatAge(quiet)}
      </span>
    );
  }
  return agent.state_since ? <Ago at={agent.state_since} /> : null;
}

// The agent's last output, amber once a working agent has gone quiet.
function LastOutput({ agent, live }: { agent: AgentRow; live: boolean }) {
  useTick();
  const quiet = live && quietFor(agent) !== null;
  return <Ago at={agent.last_output_at ?? null} prefix="last" oldAfter={quiet ? 0 : undefined} />;
}

function RailRow({
  line,
  last,
  host,
  prompt,
  selected,
  button,
  onSelect,
}: {
  line: AgentLine;
  // The last worker's elbow ends the tree's stem.
  last: boolean;
  host: Machine | undefined;
  prompt: PermissionPrompt | undefined;
  selected: boolean;
  // The phone's list holds no selection, so each row is a button to open.
  button: boolean;
  onSelect: () => void;
}) {
  const { agent } = line;
  const remote = host?.kind === "remote";
  const standing = useStanding(agent);
  const activity = hostAwareActivity(agent, host);
  const needs = activity.dot === "waiting";
  const status = standing.dim ? (
    <AsOf asOf={standing.asOf} />
  ) : needs ? (
    (promptText(prompt) ?? activity.label)
  ) : activity.dot === "live" ? (
    (agent.last_status_line ?? activity.label)
  ) : (
    activity.label
  );
  return (
    <div
      {...(button
        ? {
            role: "button",
            tabIndex: 0,
            onKeyDown: (event: KeyEvent<HTMLDivElement>) => {
              if (event.key !== "Enter" && event.key !== " ") return;
              event.preventDefault();
              onSelect();
            },
          }
        : { role: "option", "aria-selected": selected })}
      id={`av-row-${agent.session_id}`}
      data-testid={`agent-${agent.session_id}`}
      data-session={agent.session_id}
      data-s={needs ? "needs" : activity.dot}
      className={`av-row ${line.orchestrator ? "orch" : "wk"}${last ? " last" : ""}${standing.dim ? " dim" : ""}`}
      onClick={onSelect}
    >
      <i className={`ld ${activity.dot}`} title={activity.label} />
      <span className="nm">
        <span className="l">{line.name}</span>
        <span className="r">{agent.role}</span>
        {remote ? (
          <span className="av-host" title={`Runs on ${host.label}`}>
            <ServerIcon />
            <span className="vh">Runs on {host.label}</span>
          </span>
        ) : null}
      </span>
      <span className="tm">
        {needs ? (
          <span className="nt">Needs you</span>
        ) : (
          <RowAge agent={agent} live={activity.dot === "live"} />
        )}
      </span>
      <span className="st">
        {needs || activity.dot !== "live" ? null : <span className="vh">{activity.label}: </span>}
        {status}
      </span>
    </div>
  );
}

function Rail({
  groups,
  current,
  hostOf,
  colorOf,
  prompts,
  needing,
  listRef,
  hlRef,
  list,
  onSelect,
  onNext,
}: {
  groups: AgentGroup[];
  current: string;
  hostOf: (id: string) => Machine | undefined;
  colorOf: (key: string) => string;
  prompts: Map<string, PermissionPrompt>;
  needing: number;
  listRef: RefObject<HTMLDivElement | null>;
  hlRef: RefObject<HTMLDivElement | null>;
  // A phone's list alone: rows open an agent and nothing is selected.
  list: boolean;
  onSelect: (id: string) => void;
  onNext: () => void;
}) {
  const count = groups.reduce((sum, group) => sum + group.lines.length, 0);
  return (
    <aside className="av-rail">
      <div className="av-rh">
        <span className="meta">
          {count} agent{count === 1 ? "" : "s"}
        </span>
        <span className="sp" />
        {needing > 0 ? (
          <button
            type="button"
            className="av-needs"
            title="Go to the next agent that needs you (N)"
            onClick={onNext}
          >
            <i className="ld waiting" />
            Needs you
            <Roll value={needing} />
            <kbd>N</kbd>
          </button>
        ) : null}
      </div>
      <div
        className="av-list"
        ref={listRef}
        aria-label="Agents"
        {...(list
          ? { role: "list" }
          : { role: "listbox", tabIndex: 0, "aria-activedescendant": `av-row-${current}` })}
      >
        <div className="av-hl" ref={hlRef} aria-hidden="true" />
        {groups.map(({ project, lines }) => {
          const workers = lines.filter((line) => !line.orchestrator);
          const last = workers.at(-1)?.agent.session_id;
          return (
            <div
              key={projectKey(project)}
              role="group"
              aria-label={project.name}
              className="av-g"
              style={{ "--pc": colorOf(projectKey(project)) } as CSSProperties}
            >
              <div className="av-gh" aria-hidden="true">
                <span className="pdot" />
                <b>{project.name}</b>
                <HostChip entity={project} />
                <span className="sp" />
                <span>
                  {workers.length} worker{workers.length === 1 ? "" : "s"}
                </span>
              </div>
              {lines.map((line) => (
                <RailRow
                  key={line.agent.session_id}
                  line={line}
                  last={line.agent.session_id === last}
                  host={hostOf(line.agent.host_id)}
                  prompt={prompts.get(line.agent.session_id)}
                  selected={line.agent.session_id === current}
                  button={list}
                  onSelect={() => onSelect(line.agent.session_id)}
                />
              ))}
            </div>
          );
        })}
      </div>
      <div className="av-rf" aria-hidden="true">
        <span>
          <kbd>J</kbd>
          <kbd>K</kbd> move
        </span>
        <span>
          <kbd>N</kbd> needs you
        </span>
        <span>
          <kbd>↵</kbd> type
        </span>
      </div>
    </aside>
  );
}

function Details({
  line,
  project,
  color,
  host,
  open,
  onClose,
}: {
  line: AgentLine;
  project: Project;
  color: string;
  host: Machine | undefined;
  open: boolean;
  onClose: () => void;
}) {
  const { agent, card } = line;
  const activity = hostAwareActivity(agent, host);
  // The loader reads every listed agent's run, so a selection is already cached.
  const run = useQuery({
    ...runQuery(agent.run_id ?? "", ownerOf(agent)),
    enabled: Boolean(agent.run_id),
  }).data;
  const remote = host?.kind === "remote";
  return (
    <aside className="av-det" aria-label="Agent details" data-open={open} inert={!open}>
      <div className="av-det-h">
        <b>Details</b>
        <span className="sp" />
        <button
          type="button"
          className="ib sm"
          title="Close (I)"
          aria-label="Close the details"
          onClick={onClose}
        >
          <CloseIcon />
        </button>
      </div>
      <dl className="props">
        <dt>Project</dt>
        <dd>
          <span className="pdot" style={{ "--pc": color } as CSSProperties} />
          {project.name}
          {host ? <span className="meta">{host.label}</span> : null}
        </dd>
        <dt>Card</dt>
        <dd>
          {card ? (
            <span className="mono">{card.display_id}</span>
          ) : (
            <span className="muted">none, watches the whole project</span>
          )}
        </dd>
        <dt>Branch</dt>
        <dd>
          {run?.branch ? (
            <span className="mono">{run.branch}</span>
          ) : (
            <span className="muted">none</span>
          )}
        </dd>
        <dt>Worktree</dt>
        <dd>
          {run?.worktree ? (
            <span className="mono">{run.worktree}</span>
          ) : (
            <span className="muted">none</span>
          )}
        </dd>
        <dt>Workspace</dt>
        <dd>
          <span className="mono">{agent.workspace_label ?? line.label}</span>
          <span className="meta">herdr label</span>
        </dd>
        <dt>Freshness</dt>
        <dd>
          <i className={`ld ${activity.dot}`} />
          <Ago at={agent.activity_observed_at} prefix="observed" oldAfter={300} />
        </dd>
        {agent.last_output_at ? (
          <>
            <dt>Output</dt>
            <dd>
              <LastOutput agent={agent} live={activity.dot === "live"} />
            </dd>
          </>
        ) : null}
        <dt>State</dt>
        <dd>
          {activity.label}
          {agent.state_since ? (
            <span className="meta">
              <Lasting since={agent.state_since} />
            </span>
          ) : null}
        </dd>
      </dl>
      <p className="av-hint">
        {remote
          ? "Open in herdr opens herdr on the remote machine first, then focuses this agent's terminal."
          : "Open in herdr focuses this agent's terminal in herdr."}{" "}
        <code>{focusCommand(agent, project.id, card?.display_id ?? null)}</code>
      </p>
      <div className="sect">
        <div className="sect-h">
          Recent events<span className="c">newest first</span>
        </div>
        {run && run.events.length > 0 ? (
          <div className="evlog">
            {run.events.map((event) => (
              <div
                key={`${event.at}-${event.text}`}
                className={event.tone === "bad" ? "bad" : event.tone === "warn" ? "warnl" : ""}
              >
                <span className="tm">{formatLisbonMoment(event.at)}</span>
                <span className="src">{event.source}</span>
                <span className="tx">{event.text}</span>
              </div>
            ))}
          </div>
        ) : (
          <p className="muted">No events recorded for this agent yet.</p>
        )}
      </div>
    </aside>
  );
}

function Stage({
  line,
  project,
  color,
  host,
  prompt,
  details,
  carry,
  phone,
  bodyRef,
  control,
  onDetails,
  onOpen,
  onBack,
  onTyping,
  onSwitch,
}: {
  line: AgentLine;
  project: Project;
  color: string;
  host: Machine | undefined;
  prompt: PermissionPrompt | undefined;
  details: boolean;
  carry: boolean;
  phone: boolean;
  bodyRef: RefObject<HTMLDivElement | null>;
  control: RefObject<TerminalControl | null>;
  onDetails: (open: boolean) => void;
  onOpen: () => void;
  onBack: () => void;
  onTyping: (typing: boolean) => void;
  onSwitch: (key: SwitchKey) => void;
}) {
  const { agent, card } = line;
  // The page in full screen pins this card over it, a switch to another
  // agent included; leaving the Agents view leaves full screen.
  const full = usePageFullScreen() && !phone;
  useEffect(
    () => () => {
      if (document.fullscreenElement === document.documentElement) {
        void document.exitFullscreen().catch(() => {});
      }
    },
    [],
  );
  const standing = useStanding(agent);
  const ambiguous = useAmbiguousIds();
  const off = focusReason(ownerOf(agent));
  const peerHost = ownerOf(agent);
  const activity = hostAwareActivity(agent, host);
  const needs = activity.dot === "waiting";
  const remote = host?.kind === "remote";
  // The run's controls act on a pane on this machine only, as steer does.
  const local = !remote && !peerHost;
  const state = runStateOf(agent, activity);
  const orchestrator = agent.role === "orchestrator";
  // The loader reads every listed agent's run, so this is already cached.
  const run = useQuery({
    ...runQuery(agent.run_id ?? "", ownerOf(agent)),
    enabled: Boolean(agent.run_id),
  }).data;
  const target: RunTarget = {
    sessionId: agent.session_id,
    name: line.name,
    orchestrator,
    state,
    runOpen: run?.ended_at === null,
  };
  // The Stage stays mounted across agents, so an open dialog is kept with the
  // session it was opened for and never shows over another agent.
  const [asked, setAsked] = useState<{ session: string; dialog: RunDialog | "restart" } | null>(
    null,
  );
  const asking = asked?.session === agent.session_id ? asked.dialog : null;
  const ask = (next: RunDialog | "restart") =>
    setAsked({ session: agent.session_id, dialog: next });
  const closeAsk = () => setAsked(null);
  const resumeSession = useResumeSession(target);
  const goOn = useGoOn(agent.session_id, agent.pause);
  const paused = state === "paused" && agent.pause ? agent.pause : null;
  // The primary follows the run: Resume a pause, bring back a dead pane,
  // otherwise open the pane. Open in herdr moves into the Run menu then.
  const primary = !local
    ? "open"
    : paused
      ? "resume"
      : state === "dead"
        ? orchestrator
          ? "restart"
          : "relaunch"
        : "open";
  const detail = needs
    ? promptText(prompt)
    : paused
      ? `until ${formatLisbonMoment(paused.until) ?? paused.until}`
      : activity.dot === "live"
        ? agent.last_status_line
        : null;
  const heading = (
    <span className="mir-title">
      {line.label}
      {host ? <span className="meta">{host.label}</span> : null}
    </span>
  );
  const body = (
    <LiveTerminal
      key={agent.session_id}
      variant="stage"
      sessionId={agent.session_id}
      label={line.label}
      heading={heading}
      control={control}
      autoType={carry}
      onTyping={onTyping}
      onSwitch={onSwitch}
      // A paired peer is a remote host too, but its pane streams through the
      // peer link; only a saved machine's is read as a tail.
      machine={remote && !peerHost ? host.label : undefined}
      peer={peerHost}
      holder={!remote && !peerHost && isHolderPane(agent.pane_id)}
      strip={
        remote || peerHost ? null : (
          <AnswerStrip
            key={agent.session_id}
            sessionId={agent.session_id}
            waiting={agent.activity === "waiting_permission" || agent.activity === "waiting_input"}
            observedAt={agent.activity_observed_at}
            agent={{
              name: line.name,
              card: card ? shownId(card, ambiguous) : null,
              runId: agent.run_id ?? null,
            }}
          />
        )
      }
      actions={
        <button type="button" className="btn sm" title={focusTooltip(host)} onClick={onOpen}>
          <PaneIcon />
          Open in herdr
        </button>
      }
    />
  );
  return (
    <div className={full ? "av-card full" : "av-card"}>
      <div className="av-card-h">
        {phone ? (
          <button type="button" className="ib av-back" aria-label="Back to agents" onClick={onBack}>
            <ChevronLeftIcon />
          </button>
        ) : null}
        <div className="av-who">
          <div className="av-who-1">
            <i className={`ld ${activity.dot}`} title={activity.label} />
            <span className="nm">{line.name}</span>
            <RoleLabel role={agent.role} />
            <ModelChip model={agent.model} />
            <StaleChip agent={agent} />
            <HostChip entity={agent} />
            {card ? (
              <span className="ttl">
                <span className="id">{shownId(card, ambiguous)}</span>
                {card.title}
              </span>
            ) : null}
          </div>
          <div className="av-who-2" data-s={needs ? "needs" : activity.dot}>
            <span className="lb">{activity.label}</span>
            {detail ? ` · ${detail}` : null}
            {" · "}
            {standing.dim ? (
              <AsOf asOf={standing.asOf} />
            ) : (
              <Ago at={agent.activity_observed_at} prefix="observed" oldAfter={300} />
            )}
          </div>
        </div>
        <div className="av-acts">
          {local ? (
            <RunMenu
              target={target}
              onDialog={ask}
              onResumeSession={resumeSession}
              onRestart={orchestrator && primary !== "restart" ? () => ask("restart") : undefined}
              onOpen={primary === "open" ? undefined : onOpen}
            />
          ) : null}
          <button
            type="button"
            className="btn"
            aria-pressed={details}
            title="Show the agent's details (I)"
            onClick={() => onDetails(!details)}
          >
            <InfoIcon />
            Details
            <kbd>I</kbd>
          </button>
          {card ? (
            <Link
              to="/cards/$cardId"
              params={{ cardId: card.id }}
              search={ownerSearch(card)}
              className="btn"
              title={`Open card ${card.display_id}`}
            >
              <BoardIcon />
              Open card
            </Link>
          ) : null}
          {primary === "resume" ? (
            <button
              type="button"
              className="btn primary"
              disabled={goOn.pending || goOn.sent !== null}
              title={`Send ${line.name} a "go on" line`}
              onClick={goOn.trigger}
            >
              <PlayIcon />
              {goOn.pending ? "Resuming..." : "Resume"}
            </button>
          ) : primary === "relaunch" || primary === "restart" ? (
            <button
              type="button"
              className="btn primary"
              title={
                primary === "restart"
                  ? `Stop the ${project.name} orchestrator and start a new one`
                  : `Start a new pane for ${line.name} on the same branch`
              }
              onClick={() => ask(primary)}
            >
              <ReopenIcon />
              {primary === "restart" ? "Restart..." : "Relaunch..."}
            </button>
          ) : (
            <button
              type="button"
              className="btn primary"
              disabled={off !== null}
              title={off ?? focusTooltip(host)}
              onClick={onOpen}
            >
              <PaneIcon />
              Open in herdr
              <kbd>O</kbd>
            </button>
          )}
        </div>
      </div>
      <PeerNote standing={standing} />
      {paused ? (
        <PausedNote pause={paused} receiptKey={goOn.sent} />
      ) : goOn.sent ? (
        <ResumedNote receiptKey={goOn.sent} />
      ) : null}
      {local ? (
        <RunDialogs
          target={target}
          dialog={asking === "restart" ? null : asking}
          onClose={closeAsk}
          onOpen={onOpen}
        />
      ) : null}
      {local && orchestrator && asking === "restart" ? (
        <RestartOrchestratorDialog
          projectId={project.id}
          projectName={project.name}
          silent={activity.dot === "stale"}
          onClose={closeAsk}
        />
      ) : null}
      <div className="av-card-b" ref={bodyRef}>
        {body}
        {
          // Steer and interrupt reach a pane on this machine only: a peer's
          // pane is watch only, and a saved machine's pane is read as a tail.
          remote || peerHost ? null : (
            <SteerBar
              key={`${agent.session_id}:steer`}
              sessionId={agent.session_id}
              name={line.name}
              off={!agent.run_id ? NO_RUN : activity.dot === "live" ? null : NOT_WORKING}
            />
          )
        }
      </div>
      <Details
        line={line}
        project={project}
        color={color}
        host={host}
        open={details}
        onClose={() => onDetails(false)}
      />
    </div>
  );
}

const NO_PROMPTS: PermissionPrompt[] = [];

export function AgentsView({
  host,
  selected,
}: {
  host: string | undefined;
  selected: string | undefined;
}) {
  const { data: roster } = useSuspenseQuery(agentsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
  const { data: machines } = useSuspenseQuery(machinesQuery());
  // What a waiting agent asks, when the attention read has it; the rail does
  // not wait for it.
  const permissionPrompts = useQuery(attentionQuery()).data?.permission_prompts ?? NO_PROMPTS;
  const prefs = useStore(prefsStore);
  const focus = useHerdrFocus();
  const navigate = useNavigate();
  const phone = usePhone();
  const [details, setDetails] = useState(false);
  const [typing, setTyping] = useState(false);
  // Type to the next agent's pane as soon as it is live: the owner switched
  // with Option-J or Option-K while typing.
  const [carry, setCarry] = useState(false);
  const via = useRef<Via | null>(null);
  const control = useRef<TerminalControl>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const hlRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const leaving = useRef<Animation | null>(null);

  const shownProjects = scopedProjects(prefs, projects.projects, board.rows, host);
  const ambiguous = useAmbiguousIds();
  const groups = agentGroups(roster, shownProjects, board.rows, host, ambiguous);
  const current = findLine(groups, selected);
  const currentId = current?.line.agent.session_id;
  const hostOf = (id: string) => machines.machines.find((machine) => machine.id === id);
  const colorOf = (key: string) =>
    projectColor(projects.projects.findIndex((p) => projectKey(p) === key));
  const needs = (agent: AgentRow) =>
    hostAwareActivity(agent, hostOf(agent.host_id)).dot === "waiting";
  const lines = groups.flatMap((group) => group.lines);
  const needing = lines.filter((line) => needs(line.agent)).length;
  const prompts = new Map(permissionPrompts.map((prompt) => [prompt.session_id, prompt]));
  // On a phone the list shows alone until a row is chosen.
  const pane = phone ? (selected ? "stage" : "list") : "both";

  const select = (id: string, how: Via, type = false) => {
    via.current = how;
    setCarry(type);
    void navigate({
      to: "/agents",
      search: (prev) => ({ ...prev, agent: id }),
      replace: true,
    });
  };
  const step = (by: 1 | -1, type = false) => {
    const next = stepLine(groups, currentId, by);
    if (next && next.line.agent.session_id !== currentId) {
      select(next.line.agent.session_id, "key", type);
    }
  };
  const goNext = (how: Via, type = false) => {
    const next = nextNeeding(groups, currentId, needs);
    if (next) select(next.line.agent.session_id, how, type);
  };

  // The O key must not act on an agent that is not on screen.
  const openCurrent = current
    ? () => {
        if (pane === "list") return;
        if (focusReason(ownerOf(current.line.agent))) return;
        focus(current.line.agent);
      }
    : null;
  useOpenKey(openCurrent);

  // A selection that no longer exists falls back to the first agent, and the
  // URL follows, so a link never points at nothing.
  useEffect(() => {
    if (selected && current && current.line.agent.session_id !== selected) {
      void navigate({
        to: "/agents",
        search: (prev) => ({ ...prev, agent: undefined }),
        replace: true,
      });
    }
  }, [selected, current, navigate]);

  // The view's keys. The latest handler is kept in a ref, so the listener is
  // added once and still sees this render's groups.
  const onKey = useRef<(event: KeyboardEvent) => void>(() => {});
  useEffect(() => {
    onKey.current = (event) => {
      if (!current || event.defaultPrevented) return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (shortcutsBlocked(event)) return;
      if (pane === "list") return;
      const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
      const onList = event.target === listRef.current;
      if (key === "j" || (onList && key === "ArrowDown")) step(1);
      else if (key === "k" || (onList && key === "ArrowUp")) step(-1);
      else if (key === "n") goNext("key");
      else if (key === "i") setDetails((open) => !open);
      else if (key === "Escape" && details) setDetails(false);
      else if (/^[1-9]$/.test(key)) {
        // A digit is the answer button that shows it, as on the prompt itself.
        const answer = document.querySelector<HTMLButtonElement>(`.av-ans[data-n="${key}"]`);
        if (!answer || answer.disabled) return;
        answer.click();
      } else if (key === "Enter") {
        // Enter on a button or a link is that control's own.
        if (event.target instanceof Element && event.target.closest("button, a")) return;
        control.current?.type();
      } else return;
      event.preventDefault();
    };
  });
  useEffect(() => {
    const listener = (event: KeyboardEvent) => onKey.current(event);
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  }, [onKey]);

  const onSwitch = (key: SwitchKey) => {
    if (key === "needs") goNext("key", true);
    else step(key === "next" ? 1 : -1, true);
  };

  // The highlight sits under the selected row: it glides there after a click
  // and jumps there after a key. It follows the row through any render and
  // any change to the rail's size.
  const placeHighlight = useEffectEvent((glide: boolean) => {
    const hl = hlRef.current;
    const row = rowOf(listRef.current, currentId);
    if (!hl || !row) return;
    hl.style.transition = glide ? `transform 180ms ${ease("--ease-out")}` : "none";
    hl.style.width = `${row.offsetWidth}px`;
    hl.style.height = `${row.offsetHeight}px`;
    hl.style.transform = `translate(${row.offsetLeft}px, ${row.offsetTop}px)`;
    hl.dataset.shown = "true";
  });
  useLayoutEffect(() => {
    placeHighlight(via.current === "pointer" && !reducedMotion());
  });
  const rowCount = lines.length;
  useEffect(() => {
    const list = listRef.current;
    if (!list || rowCount === 0 || typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(() => placeHighlight(false));
    observer.observe(list);
    for (const row of list.querySelectorAll("[data-session]")) observer.observe(row);
    return () => observer.disconnect();
  }, [rowCount]);
  // A new selection that a key or a link made scrolls into view; a refetch
  // leaves the owner's scroll alone.
  useLayoutEffect(() => {
    if (via.current !== "pointer")
      rowOf(listRef.current, currentId)?.scrollIntoView?.({ block: "nearest" });
  }, [currentId, via, listRef]);

  // A row that joins the rail eases in and glows once; a row that starts
  // needing the owner pings, and its dot ripples.
  const seen = useRef<Map<string, boolean> | null>(null);
  const railState = lines.map((line) => `${line.agent.session_id}:${needs(line.agent)}`).join(" ");
  useLayoutEffect(() => {
    const list = listRef.current;
    const now = new Map(
      railState.split(" ").map((entry) => {
        const at = entry.lastIndexOf(":");
        return [entry.slice(0, at), entry.slice(at + 1) === "true"] as const;
      }),
    );
    const before = seen.current;
    seen.current = now;
    if (!list || !before) return;
    const calm = reducedMotion();
    for (const row of list.querySelectorAll<HTMLElement>("[data-session]")) {
      const id = row.dataset.session ?? "";
      if (!before.has(id)) {
        play(
          row,
          calm
            ? [{ opacity: 0 }, { opacity: 1 }]
            : [
                { opacity: 0, transform: "translateY(-6px) scale(0.98)" },
                { opacity: 1, transform: "none" },
              ],
          { duration: calm ? 200 : 240, easing: calm ? "ease" : ease("--ease-out") },
        );
        flash(row, "fresh");
      } else if (now.get(id) && !before.get(id)) {
        flash(row, "ping");
        flash(row.querySelector(".ld"), "ripple");
      }
    }
  }, [railState]);

  // A clicked switch fades the new screen in through a slight blur, which
  // hides the swap; a key switch is instant.
  const onStage = useRef(currentId);
  useLayoutEffect(() => {
    if (onStage.current === currentId) return;
    onStage.current = currentId;
    if (via.current !== "pointer" || phone) return;
    play(
      bodyRef.current,
      reducedMotion()
        ? [{ opacity: 0.4 }, { opacity: 1 }]
        : [
            { opacity: 0.25, filter: "blur(2px)" },
            { opacity: 1, filter: "blur(0)" },
          ],
      { duration: 140, easing: ease("--ease-out") },
    );
  }, [currentId, phone, onStage, via, bodyRef]);

  // On a phone the stage pushes in over the list, and Back slides it away.
  const shownPane = useRef(pane);
  useLayoutEffect(() => {
    const was = shownPane.current;
    shownPane.current = pane;
    leaving.current?.cancel();
    leaving.current = null;
    if (was !== "list" || pane !== "stage" || via.current !== "pointer") return;
    const calm = reducedMotion();
    play(
      stageRef.current,
      calm
        ? [{ opacity: 0 }, { opacity: 1 }]
        : [{ transform: "translateX(100%)" }, { transform: "none" }],
      { duration: calm ? 200 : 320, easing: ease("--ease-drawer") },
    );
  }, [pane, shownPane, leaving, via, stageRef]);
  const toList = () =>
    void navigate({
      to: "/agents",
      search: (prev) => ({ ...prev, agent: undefined }),
      replace: true,
    });
  const back = () => {
    const calm = reducedMotion();
    const out = play(
      stageRef.current,
      calm
        ? [{ opacity: 1 }, { opacity: 0 }]
        : [{ transform: "none" }, { transform: "translateX(100%)" }],
      { duration: calm ? 160 : 220, easing: ease("--ease-drawer"), fill: "forwards" },
    );
    if (!out) return toList();
    leaving.current = out;
    out.onfinish = toList;
  };

  if (shownProjects.length === 0) {
    return (
      <div className="agents-view">
        <EmptyScope host={host} what="agents" />
      </div>
    );
  }
  if (groups.length === 0 || !current || !currentId) {
    const label = machines.machines.find((machine) => machine.id === host)?.label;
    return (
      <div className="agents-view">
        <div className="empty" data-testid="agents-empty">
          <div>
            <b>{host ? `No agents on ${label ?? "that machine"}` : "No agents running"}</b>
            Agents show here once a card is dispatched to one.
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="agents-view av-view" data-pane={pane} data-typing={typing}>
      <Rail
        groups={groups}
        current={currentId}
        hostOf={hostOf}
        colorOf={colorOf}
        prompts={prompts}
        needing={needing}
        listRef={listRef}
        hlRef={hlRef}
        list={pane === "list"}
        onSelect={(id) => select(id, "pointer")}
        onNext={() => goNext("pointer")}
      />
      {/* A phone's list shows alone, and no agent's stream runs behind it. */}
      {pane === "list" ? null : (
        <div className="av-stage" ref={stageRef}>
          <Stage
            line={current.line}
            project={current.group.project}
            color={colorOf(projectKey(current.group.project))}
            host={hostOf(current.line.agent.host_id)}
            prompt={prompts.get(currentId)}
            details={details}
            carry={carry && selected === currentId}
            phone={phone}
            bodyRef={bodyRef}
            control={control}
            onDetails={setDetails}
            onOpen={() => focus(current.line.agent)}
            onBack={back}
            onTyping={setTyping}
            onSwitch={onSwitch}
          />
        </div>
      )}
    </div>
  );
}
