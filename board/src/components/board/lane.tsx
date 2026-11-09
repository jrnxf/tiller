import { useCallback, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, MouseEvent } from "react";

import type { AgentRow, Machine, Project } from "../../lib/api-types";
import { agentActivity } from "./activity";
import { Ago } from "./ago";
import { toggleLane } from "./board-prefs";
import { isUntriaged } from "./bugs";
import { ChevronDownIcon, PaneIcon, RouteIcon, Status, WarnIcon } from "./icons";
import type { StatusMark } from "./icons";
import { COLUMNS, openCount } from "./model";
import type { ColumnId, Tile } from "./model";
import { PeerNote } from "./host-chip";
import { MiniTile, TileView } from "./tile";
import { HOST_PROJECT, focusReason, hostField, ownerOf, projectKey } from "../../lib/peers";
import type { Standing } from "../../lib/peers";
import { useHerdrFocus } from "./herdr";
import { NewCardDialog } from "./new-card";
import { MoveBugsDialog } from "./triage";
import { StartOrchestratorButton } from "./orchestrator-start";
import { PlusIcon, ServerIcon } from "../shell/icons";
import { StaleChip } from "./stale-sessions";
import type { LaneHealth } from "../states/degraded";
import { LaneNote } from "../states/lane-note";

const COLUMN_MARK: Record<ColumnId, StatusMark> = {
  asked: "asked",
  queued: "queued",
  working: "working",
  landing: "landing",
  done: "done",
};

function OrchestratorMini({
  orchestrator,
  health,
}: {
  orchestrator: AgentRow;
  health: LaneHealth;
}) {
  if (health.kind === "disconnected") {
    return (
      <span className="orch">
        <i className="ld off" />
        <span className="tx">
          orchestrator unreachable{health.since ? `, last seen ${health.since}` : ""}
        </span>
      </span>
    );
  }
  if (health.kind === "stale") {
    return (
      <span className="orch">
        <i className="ld stale" />
        <span className="tx">orchestrator silent for {health.ago}</span>
      </span>
    );
  }
  const view = agentActivity(orchestrator);
  return (
    <span className="orch">
      <i className={`ld ${view.dot}${view.dot === "live" ? " pulse" : ""}`} />
      <span className="tx">
        {orchestrator.model ? `orchestrator ${orchestrator.model}` : "orchestrator"},{" "}
        {view.label.toLowerCase()}, <Ago at={orchestrator.activity_observed_at} prefix="observed" />
      </span>
      <StaleChip agent={orchestrator} />
    </span>
  );
}

interface LaneProps {
  project: Project;
  color: string;
  tiles: Tile[];
  open: boolean;
  focusMode: boolean;
  orchestrator: AgentRow | undefined;
  // The agents read succeeded, so no orchestrator row means none is running.
  orchestratorsKnown: boolean;
  remoteHost: Machine | undefined;
  health: LaneHealth;
  standing: Standing;
  hostLabel: string;
  // The host that owns the lane's project, and the project that takes its
  // Tiller bugs (state.md, hosts.bug_project_id).
  owner: Machine | undefined;
  bugProject: Project | undefined;
  doneFolded: boolean;
  filterName: string | null;
}

// Where `tiller bug file` sends a host's Tiller bugs, on that host's `fleet`
// lane: the inline form for the header, the line form below it on a phone.
function BugRoute({ bugProject, line }: { bugProject: Project | undefined; line?: boolean }) {
  return (
    <span className={`bug-route${line ? " line" : ""}`} data-none={bugProject ? undefined : "true"}>
      {bugProject ? <RouteIcon /> : <WarnIcon />}
      {bugProject ? `Tiller bugs go to ${bugProject.name}` : "No project takes Tiller bugs here"}
    </span>
  );
}

// The Tiller bugs that waited in `fleet` before the host took them, with the
// action that moves them all to its bug project.
function WaitingBugs({
  tiles,
  owner,
  bugProject,
  standing,
}: {
  tiles: Tile[];
  owner: Machine;
  bugProject: Project;
  standing: Standing;
}) {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const rows = tiles.flatMap((tile) => (tile.row && isUntriaged(tile.row) ? [tile.row] : []));
  if (rows.length === 0) return null;
  const target = {
    machine: owner,
    project: bugProject,
    label: `${owner.label} · ${bugProject.name}`,
  };
  const n = rows.length === 1 ? "1 Tiller bug waits" : `${rows.length} Tiller bugs wait`;
  return (
    <div className="lane-bugs" data-testid="waiting-bugs">
      <WarnIcon />
      <span className="tx">
        {n} here from before {bugProject.name} took them.
      </span>
      <button
        ref={button}
        type="button"
        className="btn sm"
        disabled={!standing.canAct}
        title={standing.reason ?? `Move them to ${target.label}`}
        onClick={() => setOpen(true)}
      >
        <RouteIcon />
        Move {rows.length} to {bugProject.name}
      </button>
      {open ? (
        <MoveBugsDialog
          rows={rows}
          target={target}
          hostLabel={owner.label}
          onClose={() => {
            setOpen(false);
            button.current?.focus();
          }}
        />
      ) : null}
    </div>
  );
}

export function Lane({
  project,
  color,
  tiles,
  open,
  focusMode,
  orchestrator,
  orchestratorsKnown,
  remoteHost,
  health,
  standing,
  hostLabel,
  owner,
  bugProject,
  doneFolded,
  filterName,
}: LaneProps) {
  const focus = useHerdrFocus();
  const [filing, setFiling] = useState(false);
  const newCard = useRef<HTMLButtonElement>(null);
  const closeFiling = useCallback(() => {
    setFiling(false);
    newCard.current?.focus();
  }, []);
  const key = projectKey(project);
  const off = focusReason(ownerOf(project));
  // A peer's project starts on its host, through the forwarded start.
  const startable = !orchestrator && orchestratorsKnown;
  const counts = COLUMNS.map((column) => ({
    column,
    count: tiles.filter((tile) => tile.column === column.id).length,
  })).filter(({ count }) => count > 0);
  const waiting = tiles.filter((tile) => tile.row?.needs_you).length;
  const hostLane = project.id === HOST_PROJECT;
  const takesBugs = bugProject !== undefined && projectKey(bugProject) === key;

  let body;
  if (tiles.length === 0) {
    body = (
      <div className="lane-cells">
        <div className="lane-empty">
          <div className="tx">
            <b>{filterName ? "Nothing in this tab" : "No cards yet"}</b>
            {filterName
              ? `No ${project.name} cards match "${filterName}".`
              : startable
                ? `${project.name} has no cards and no orchestrator. Start its orchestrator, then ask it for a first card.`
                : `${project.name} has no cards. Ask its orchestrator for a first card, or open its pane.`}
          </div>
          {filterName ? null : (
            <>
              {orchestrator ? (
                <button
                  type="button"
                  className="btn ghost"
                  disabled={health.kind === "disconnected" || off !== null}
                  title={off ?? "Focus this agent's terminal in herdr"}
                  onClick={() => focus(orchestrator)}
                >
                  <PaneIcon />
                  Open in herdr
                </button>
              ) : null}
            </>
          )}
        </div>
      </div>
    );
  } else {
    body = (
      <div className="lane-cells">
        {COLUMNS.map((column) => {
          const items = tiles.filter((tile) => tile.column === column.id);
          const folded = column.id === "done" && doneFolded;
          return (
            <div
              key={column.id}
              className={`cell${folded && items.length > 0 ? " mini" : ""}`}
              data-n={items.length}
              data-column={column.id}
            >
              <div className="cell-label">
                <Status mark={COLUMN_MARK[column.id]} />
                <b>{column.name}</b>
                <span>{items.length}</span>
              </div>
              {folded
                ? items.map((tile) =>
                    tile.row ? <MiniTile key={tile.key} row={tile.row} /> : null,
                  )
                : items.map((tile) => <TileView key={tile.key} tile={tile} lane={health.kind} />)}
            </div>
          );
        })}
      </div>
    );
  }

  if (focusMode) {
    return (
      <section className="lane" data-open="true" data-testid={`lane-${key}`}>
        {body}
      </section>
    );
  }

  return (
    <section
      className="lane"
      data-open={open}
      data-state={health.kind === "ok" ? undefined : health.kind}
      data-testid={`lane-${key}`}
    >
      <div
        className="lane-h"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        style={{ "--pc": color } as CSSProperties}
        onClick={() => toggleLane(key)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            toggleLane(key);
          }
        }}
      >
        <ChevronDownIcon />
        <span className="pdot" />
        <span className="nm">{project.name}</span>
        <span className="c">{openCount(tiles)} open</span>
        {takesBugs ? <span className="lab sm acc takes-bugs">Takes Tiller bugs</span> : null}
        {remoteHost ? (
          <span className="host-t">
            <ServerIcon />
            {remoteHost.label}
          </span>
        ) : null}
        <span className="ccs">
          {counts.map(({ column, count }) => (
            <span key={column.id} className="cc" title={column.name}>
              <Status mark={COLUMN_MARK[column.id]} />
              {count}
            </span>
          ))}
          {waiting > 0 ? (
            <span className="cc attn" title="Needs you">
              <Status mark="needs_you" />
              {waiting}
            </span>
          ) : null}
        </span>
        <span className="sp" />
        {hostLane ? <BugRoute bugProject={bugProject} /> : null}
        <button
          ref={newCard}
          type="button"
          className="btn sm new-card"
          aria-label="New card"
          disabled={!standing.canAct}
          title={standing.reason ?? `File a card in ${project.name}`}
          onClick={(event: MouseEvent) => {
            event.stopPropagation();
            setFiling(true);
          }}
          onKeyDown={(event: KeyboardEvent) => event.stopPropagation()}
        >
          <PlusIcon />
          <span>New card</span>
        </button>
        {orchestrator ? (
          <OrchestratorMini orchestrator={orchestrator} health={health} />
        ) : startable ? (
          <StartOrchestratorButton
            projectId={project.id}
            projectName={project.name}
            {...hostField(project)}
            off={standing.canAct ? null : standing.reason}
          />
        ) : null}
      </div>
      {filing ? <NewCardDialog project={project} onClose={closeFiling} /> : null}
      {health.kind === "ok" ? null : (
        <LaneNote health={health} projectName={project.name} hostLabel={hostLabel} />
      )}
      {hostLane ? <BugRoute bugProject={bugProject} line /> : null}
      {hostLane && owner && bugProject ? (
        <WaitingBugs tiles={tiles} owner={owner} bugProject={bugProject} standing={standing} />
      ) : null}
      <PeerNote standing={standing} />
      {body}
    </section>
  );
}
