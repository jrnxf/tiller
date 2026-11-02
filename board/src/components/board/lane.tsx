import type { CSSProperties } from "react";

import type { AgentRow, Machine, Project } from "../../lib/api-types";
import { agentActivity } from "./activity";
import { Ago } from "./ago";
import { toggleLane } from "./board-prefs";
import { ChevronDownIcon, PaneIcon, Status } from "./icons";
import type { StatusMark } from "./icons";
import { COLUMNS, openCount } from "./model";
import type { ColumnId, Tile } from "./model";
import { MiniTile, TileView } from "./tile";
import { useHerdrFocus } from "./herdr";
import { ServerIcon } from "../shell/icons";
import type { LaneHealth } from "../states/degraded";
import { LaneNote } from "../states/lane-note";

const COLUMN_MARK: Record<ColumnId, StatusMark> = {
  asked: "asked",
  queued: "queued",
  working: "working",
  needs_you: "needs_you",
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
        orchestrator {orchestrator.model}, {view.label.toLowerCase()},{" "}
        <Ago at={orchestrator.activity_observed_at} prefix="observed" />
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
  remoteHost: Machine | undefined;
  health: LaneHealth;
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
  remoteHost,
  health,
  hostLabel,
  doneFolded,
  filterName,
}: LaneProps) {
  const focus = useHerdrFocus();
  const counts = COLUMNS.map((column) => ({
    column,
    count: tiles.filter((tile) => tile.column === column.id).length,
  })).filter(({ count }) => count > 0);

  let body;
  if (tiles.length === 0) {
    body = (
      <div className="lane-cells">
        <div className="lane-empty">
          <div className="tx">
            <b>{filterName ? "Nothing in this tab" : "No cards yet"}</b>
            {filterName
              ? `No ${project.name} cards match "${filterName}".`
              : `${project.name} has no cards. Ask its orchestrator for a first card, or open its pane.`}
          </div>
          {filterName ? null : (
            <>
              {orchestrator ? (
                <button
                  type="button"
                  className="btn ghost"
                  disabled={health.kind === "disconnected"}
                  title="Focus this agent's terminal in herdr"
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
              className={`cell${items.length === 0 || folded ? " mini" : ""}`}
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
      <section className="lane" data-open="true" data-testid={`lane-${project.id}`}>
        {body}
      </section>
    );
  }

  return (
    <section
      className="lane"
      data-open={open}
      data-state={health.kind === "ok" ? undefined : health.kind}
      data-testid={`lane-${project.id}`}
    >
      <div
        className="lane-h"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        style={{ "--pc": color } as CSSProperties}
        onClick={() => toggleLane(project.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            toggleLane(project.id);
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
            <span
              key={column.id}
              className={`cc${column.id === "needs_you" ? " attn" : ""}`}
              title={column.name}
            >
              <Status mark={COLUMN_MARK[column.id]} />
              {count}
            </span>
          ))}
        </span>
        <span className="sp" />
        {orchestrator ? <OrchestratorMini orchestrator={orchestrator} health={health} /> : null}
      </div>
      {health.kind === "ok" ? null : (
        <LaneNote health={health} projectName={project.name} hostLabel={hostLabel} />
      )}
      {body}
    </section>
  );
}
