import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";
import type { CSSProperties } from "react";

import type { Machine, Project } from "../../lib/api-types";
import { focusReason, ownerKey, ownerOf, projectKey, shownId } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { agentsQuery, boardQuery, machinesQuery, projectsQuery, runQuery } from "../../lib/queries";
import { prefsStore } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { hostAwareActivity } from "../states/degraded";
import { Ago } from "./ago";
import { agentGroups, findLine } from "./agents-model";
import type { AgentLine } from "./agents-model";
import { EmptyScope } from "./board-view";
import { formatLisbonMoment } from "./format";
import { focusCommand, focusTooltip, useHerdrFocus, useOpenKey } from "./herdr";
import { ChevronLeftIcon, PaneIcon } from "./icons";
import { AsOf, HostChip, PeerNote } from "./host-chip";
import { useAmbiguousIds, useStanding } from "./peer-state";
import { ModelChip, ownerSearch } from "./tile";
import { ToastHost } from "./toast";
import { BoardIcon, ServerIcon } from "../shell/icons";
import "./board.css";

const ROLE_COLOR: Record<string, string> = {
  orchestrator: "var(--orange)",
  scout: "var(--blue)",
  build: "var(--green)",
  ship: "var(--green)",
  review: "var(--indigo)",
  verify: "var(--teal)",
};

function RoleLabel({ role }: { role: string }) {
  return (
    <span className="lab sm">
      <span className="dot" style={{ background: ROLE_COLOR[role] ?? "var(--fg-4)" }} />
      {role}
    </span>
  );
}

function AgentRowView({
  line,
  project,
  openCards,
  host,
  selected,
  onOpen,
}: {
  line: AgentLine;
  project: Project;
  openCards: number;
  host: Machine | undefined;
  selected: boolean;
  onOpen: () => void;
}) {
  const { agent, card } = line;
  const standing = useStanding(agent);
  const activity = hostAwareActivity(agent, host);
  const off = focusReason(ownerOf(agent));
  const ambiguous = useAmbiguousIds();
  const navigate = useNavigate();
  const select = () =>
    void navigate({
      to: "/agents",
      search: (prev) => ({ ...prev, agent: agent.session_id }),
      replace: true,
    });
  return (
    <div
      role="button"
      tabIndex={0}
      className={`ag-row${line.orchestrator ? "" : " nest"}${standing.dim ? " dim" : ""}`}
      aria-selected={selected}
      data-testid={`agent-${agent.session_id}`}
      onClick={select}
      onKeyDown={(event) => {
        if (event.target === event.currentTarget && (event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          select();
        }
      }}
    >
      <i
        className={`ld ${activity.dot}${activity.dot === "live" ? " pulse" : ""} ag-dot`}
        title={activity.label}
      />
      <div className="ag-main">
        <div className="ag-l1">
          <span className="wsl">{line.label}</span>
          <RoleLabel role={agent.role} />
          <HostChip entity={agent} />
          <ModelChip model={agent.model} />
          <span className="ag-t">
            {card ? (
              <>
                <span className="aid">{shownId(card, ambiguous)}</span> {card.title}
              </>
            ) : (
              <>
                <span className="aid">
                  {openCards} open card{openCards === 1 ? "" : "s"}
                </span>{" "}
                in {project.name}
              </>
            )}
          </span>
        </div>
        <div className="ag-act" data-s={activity.dot}>
          {standing.dim ? (
            <AsOf asOf={standing.asOf} />
          ) : (
            <>
              {activity.label},{" "}
              <Ago at={agent.activity_observed_at} prefix="observed" oldAfter={300} />
            </>
          )}
        </div>
        {agent.last_status_line ? (
          <div className="ag-last">
            <span>&rsaquo;</span>
            <code>{agent.last_status_line}</code>
          </div>
        ) : null}
      </div>
      <div className="ag-r">
        {agent.state_since ? (
          <span className="meta">
            <Ago at={agent.state_since} />
          </span>
        ) : null}
        <button
          type="button"
          className="btn sm"
          disabled={off !== null}
          title={off ?? focusTooltip(host)}
          onClick={(event) => {
            event.stopPropagation();
            onOpen();
          }}
        >
          <PaneIcon />
          Open in herdr
          {selected ? <kbd>O</kbd> : null}
        </button>
      </div>
    </div>
  );
}

function AgentDetail({
  line,
  project,
  color,
  host,
  onOpen,
}: {
  line: AgentLine;
  project: Project;
  color: string;
  host: Machine | undefined;
  onOpen: () => void;
}) {
  const { agent, card } = line;
  const standing = useStanding(agent);
  const off = focusReason(ownerOf(agent));
  const activity = hostAwareActivity(agent, host);
  // The loader reads every listed agent's run, so a selection is already cached.
  const run = useQuery({
    ...runQuery(agent.run_id ?? "", ownerOf(agent)),
    enabled: Boolean(agent.run_id),
  }).data;
  const remote = host?.kind === "remote";
  return (
    <div className="agd">
      <div className="agd-h">
        <Link
          to="/agents"
          search={(prev) => ({ ...prev, agent: undefined })}
          className="btn ghost sm in-back"
        >
          <ChevronLeftIcon />
          Agents
        </Link>
        <i className={`ld ${activity.dot}${activity.dot === "live" ? " pulse" : ""}`} />
        <span className="wsl">{line.label}</span>
      </div>
      <div className="agd-b">
        <div className="agd-chips">
          <span className="lab">
            <span className="dot" style={{ background: ROLE_COLOR[agent.role] ?? "var(--fg-4)" }} />
            {agent.role}
          </span>
          <HostChip entity={agent} />
          <ModelChip model={agent.model} />
        </div>
        <PeerNote standing={standing} />
        <h4>{card ? card.title : `${project.name} orchestrator`}</h4>
        <p className="agd-act" data-s={activity.dot}>
          {activity.label}
        </p>
        {agent.last_status_line ? (
          <div className="ag-last big">
            <span>&rsaquo;</span>
            <code>{agent.last_status_line}</code>
          </div>
        ) : null}
        <div className="acts">
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
          {card ? (
            <Link
              to="/cards/$cardId"
              params={{ cardId: card.id }}
              search={ownerSearch(card)}
              className="btn"
            >
              <BoardIcon />
              Open card {card.display_id}
            </Link>
          ) : null}
        </div>
        <p className="agd-hint">
          {remote
            ? "Opens herdr on the remote machine first, then focuses this agent's terminal."
            : "Focuses this agent's terminal in herdr."}{" "}
          Shortcut <kbd>O</kbd>.{" "}
          <code>{focusCommand(agent, project.id, card?.display_id ?? null)}</code>
        </p>
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
          <dt>State</dt>
          <dd>
            {activity.label}
            {agent.state_since ? (
              <span className="meta">
                for <Ago at={agent.state_since} />
              </span>
            ) : null}
          </dd>
        </dl>
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
      </div>
    </div>
  );
}

export function AgentsView({
  host,
  selected,
}: {
  host: string | undefined;
  selected: string | undefined;
}) {
  const { data: roster } = useSuspenseQuery(agentsQuery());
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const prefs = useStore(prefsStore);
  const focus = useHerdrFocus();
  const navigate = useNavigate();

  const shownProjects = scopedProjects(prefs, projects.projects, board.rows, host);
  const ambiguous = useAmbiguousIds();
  const groups = agentGroups(roster, shownProjects, board.rows, host, ambiguous);
  const current = findLine(groups, selected);
  // On a phone the list shows alone until a row is chosen; the O key must not
  // act on an agent that is not on screen.
  const openCurrent = current
    ? () => {
        const stacked =
          typeof window.matchMedia === "function" &&
          window.matchMedia("(max-width: 820px)").matches;
        if (stacked && !selected) return;
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

  if (shownProjects.length === 0) {
    return (
      <div className="agents-view">
        <EmptyScope host={host} what="agents" />
      </div>
    );
  }
  if (groups.length === 0 || !current) {
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

  const hostOf = (id: string) => machines.machines.find((machine) => machine.id === id);
  const colorOf = (key: string) =>
    projectColor(projects.projects.findIndex((p) => projectKey(p) === key));
  const openCards = new Map<string, number>();
  for (const row of board.rows) {
    if (row.column !== "done")
      openCards.set(
        ownerKey(row.project_id, row),
        (openCards.get(ownerKey(row.project_id, row)) ?? 0) + 1,
      );
  }
  return (
    <div className="agents-view">
      <div className="agents" data-pane={selected ? "detail" : "list"}>
        <div className="ag-list">
          {groups.map(({ project, lines }) => {
            const workers = lines.filter((line) => !line.orchestrator).length;
            const remote = hostOf(lines[0]?.agent.host_id ?? "");
            return (
              <section key={projectKey(project)} aria-label={project.name}>
                <div
                  className="ag-ph"
                  style={{ "--pc": colorOf(projectKey(project)) } as CSSProperties}
                >
                  <span className="pdot" />
                  <b>{project.name}</b>
                  {remote?.kind === "remote" ? (
                    <span className="lab sm">
                      <ServerIcon />
                      {remote.label}
                    </span>
                  ) : null}
                  <span className="sp" />
                  <span className="meta">
                    orchestrator{workers ? `, ${workers} worker${workers > 1 ? "s" : ""}` : " only"}
                  </span>
                </div>
                {lines.map((line) => (
                  <AgentRowView
                    key={line.agent.session_id}
                    line={line}
                    project={project}
                    openCards={openCards.get(projectKey(project)) ?? 0}
                    host={hostOf(line.agent.host_id)}
                    selected={line.agent.session_id === current.line.agent.session_id}
                    onOpen={() => focus(line.agent)}
                  />
                ))}
              </section>
            );
          })}
        </div>
        <aside className="ag-detail" aria-label="Agent details">
          <AgentDetail
            line={current.line}
            project={current.group.project}
            color={colorOf(projectKey(current.group.project))}
            host={hostOf(current.line.agent.host_id)}
            onOpen={() => focus(current.line.agent)}
          />
        </aside>
      </div>
      <ToastHost />
    </div>
  );
}
