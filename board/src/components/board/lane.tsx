import type { CSSProperties } from "react";

import type { AgentRow, Machine, Project } from "../../lib/api-types";
import { agentActivity } from "./activity";
import { Ago } from "./ago";
import { toggleLane } from "./board-prefs";
import { ChevronDownIcon, PaneIcon, Status } from "./icons";
import type { StatusMark } from "./icons";
import { COLUMNS, openCount } from "./model";
import type { ColumnId, Tile } from "./model";
import { PeerNote } from "./host-chip";
import { MiniTile, TileView } from "./tile";
import { focusReason, isPeerRow, ownerOf, projectKey } from "../../lib/peers";
import type { Standing } from "../../lib/peers";
import { useHerdrFocus } from "./herdr";
import { StartOrchestratorButton } from "./orchestrator-start";
import { ServerIcon } from "../shell/icons";
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
  doneFolded: boolean;
  filterName: string | null;
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
  doneFolded,
  filterName,
}: LaneProps) {
  const focus = useHerdrFocus();
  const key = projectKey(project);
  const off = focusReason(ownerOf(project));
  // Only this host's own projects start here: a peer's start is not forwarded.
  const startable = !orchestrator && orchestratorsKnown && !isPeerRow(project);
  const counts = COLUMNS.map((column) => ({
    column,
    count: tiles.filter((tile) => tile.column === column.id).length,
  })).filter(({ count }) => count > 0);
  const waiting = tiles.filter((tile) => tile.row?.needs_you).length;

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
        {orchestrator ? (
          <OrchestratorMini orchestrator={orchestrator} health={health} />
        ) : startable ? (
          <StartOrchestratorButton projectId={project.id} projectName={project.name} />
        ) : null}
      </div>
      {health.kind === "ok" ? null : (
        <LaneNote health={health} projectName={project.name} hostLabel={hostLabel} />
      )}
      <PeerNote standing={standing} />
      {body}
    </section>
  );
}
