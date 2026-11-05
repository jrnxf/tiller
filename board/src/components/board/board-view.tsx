import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, RefObject } from "react";

import type { BoardFilter } from "../../lib/api-types";
import { ownerKey, projectKey, standingOf } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { agentsQuery, boardQuery, machinesQuery, projectsQuery } from "../../lib/queries";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { boardLookStore, toggleDoneFolded } from "./board-prefs";
import { ChevronLeftIcon, Status } from "./icons";
import { laneHealth } from "../states/degraded";
import { SECTION_RETRY_MS, SectionNote, failureReason } from "../states/section";
import { Lane } from "./lane";
import { COLUMNS, onHost, passesFilter, tilesOf } from "./model";
import type { ColumnId, Tile } from "./model";
import { LayersIcon, CloseIcon } from "../shell/icons";
import "./board.css";

const FILTER_NAME: Record<BoardFilter, string | null> = {
  all: null,
  active: "Active",
  needs: "Needs you",
};

const COLUMN_MARK = {
  asked: "asked",
  queued: "queued",
  working: "working",
  needs_you: "needs_you",
  landing: "landing",
  done: "done",
} as const;

// Width of a column that has no full cards: the same 52 px the mocks use.
const MINI_WIDTH = 52;
const FULL_WIDTH = 176;
const GAP = 8;

// How much of each card the board shows. 0 is the full card; each step drops
// detail until the lanes fit the screen: 1 moves the freshness up beside the id,
// 2 clips titles to one line, 3 keeps only the title and the badge.
const MAX_FIT = 3;

interface FitState {
  key: string;
  level: number;
}

// Raises the level, one step at a time, while the board overflows its frame, and
// starts again from the full card whenever what is shown or its room changes.
// The steps run in a layout effect, so the screen never paints an overflowing
// intermediate.
function useFitLevel(ref: RefObject<HTMLDivElement | null>, content: string): number {
  const [size, setSize] = useState("");
  const key = `${content}|${size}`;
  const [state, setState] = useState<FitState>({ key, level: 0 });

  let level = state.level;
  if (state.key !== key) {
    setState({ key, level: 0 });
    level = 0;
  }

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measured = `${element.clientWidth}x${element.clientHeight}`;
    if (size === "") {
      setSize(measured);
      return;
    }
    // Phones stack the columns and scroll; only the grid has a frame to fit.
    const stacked =
      typeof window.matchMedia === "function" && window.matchMedia("(max-width: 820px)").matches;
    if (
      !stacked &&
      element.scrollHeight > element.clientHeight + 1 &&
      state.key === key &&
      level < MAX_FIT
    ) {
      setState({ key, level: level + 1 });
    }
  }, [key, level, ref, size, state.key]);

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() =>
      setSize(`${element.clientWidth}x${element.clientHeight}`),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);

  return level;
}

export function EmptyScope({ host, what }: { host: string | undefined; what: "cards" | "agents" }) {
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const label = machines.machines.find((machine) => machine.id === host)?.label;
  return (
    <div className="empty" data-testid="board-empty">
      <div>
        <b>{host ? `Nothing on ${label ?? "that machine"} in this view` : "No projects shown"}</b>
        {host
          ? `No ${what} on ${label ?? "that machine"} match the projects you are showing.`
          : `Every project is hidden from ${what === "cards" ? "the board" : "this screen"}.`}
        <br />
        {host ? (
          <Link to="." search={(prev) => ({ ...prev, host: undefined })} className="btn">
            <CloseIcon />
            Clear the machine filter
          </Link>
        ) : (
          <button
            type="button"
            className="btn"
            onClick={() => updatePrefs({ view: "all", hiddenProjects: [] })}
          >
            <LayersIcon />
            Show all projects
          </button>
        )}
      </div>
    </div>
  );
}

export function BoardView({ host, filter }: { host: string | undefined; filter: BoardFilter }) {
  const frame = useRef<HTMLDivElement>(null);
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: projects } = useSuspenseQuery(projectsQuery());
  // Agents only add each lane's orchestrator and its health: when they cannot
  // be read the cards still show, with a note, and the read retries.
  const agentsRead = useQuery({
    ...agentsQuery(),
    refetchInterval: (query) => (query.state.status === "error" ? SECTION_RETRY_MS : false),
  });
  const orchestrators = agentsRead.data?.orchestrators ?? [];
  const { data: machines } = useSuspenseQuery(machinesQuery());
  const prefs = useStore(prefsStore);
  const look = useStore(boardLookStore);

  const lanes = scopedProjects(prefs, projects.projects, board.rows, host);
  const focusMode = prefs.view === "focus";
  const tiles = tilesOf(board).filter((tile) => passesFilter(tile, filter) && onHost(tile, host));
  const fit = useFitLevel(
    frame,
    [
      prefs.density,
      prefs.view,
      filter,
      host,
      look.collapsed.join(","),
      look.doneFolded,
      lanes.map((project) => projectKey(project)).join(","),
      tiles
        .map((tile) => `${tile.key}:${tile.column}:${tile.row?.badge}:${tile.row?.title}`)
        .join("|"),
    ].join("#"),
  );

  if (lanes.length === 0) {
    return (
      <div className="board-view" ref={frame}>
        <EmptyScope host={host} what="cards" />
      </div>
    );
  }

  const byProject = new Map<string, Tile[]>();
  for (const tile of tiles) {
    byProject.set(tile.projectId, [...(byProject.get(tile.projectId) ?? []), tile]);
  }
  const shown = lanes.flatMap((project) => byProject.get(projectKey(project)) ?? []);

  const inColumn = (id: ColumnId) => shown.filter((tile) => tile.column === id).length;
  const counts = COLUMNS.map((column) => inColumn(column.id));
  const folded = (index: number) =>
    counts[index] === 0 || (COLUMNS[index]?.id === "done" && look.doneFolded);
  const widths = shown.length
    ? COLUMNS.map((_, index) => (folded(index) ? `${MINI_WIDTH}px` : "minmax(0,1fr)")).join(" ")
    : "repeat(6,minmax(0,1fr))";
  const full = shown.length ? COLUMNS.filter((_, index) => !folded(index)).length : 6;
  const minWidth =
    32 + (COLUMNS.length - 1) * GAP + full * FULL_WIDTH + (COLUMNS.length - full) * MINI_WIDTH;

  return (
    <div className="board-view" ref={frame}>
      {agentsRead.isError && !agentsRead.data ? (
        <SectionNote
          label="Agents"
          reason={`${failureReason(agentsRead.error)} Lanes show no orchestrator until it answers.`}
          retry={() => void agentsRead.refetch()}
        />
      ) : null}
      <div
        className="board"
        style={{ "--cols": widths, "--minw": `${minWidth}px` } as CSSProperties}
        data-fit={fit}
        data-testid="board"
      >
        <div className="bhead">
          {COLUMNS.map((column, index) => {
            const isDone = column.id === "done";
            if (isDone && folded(index) && counts[index]) {
              return (
                <button
                  key={column.id}
                  type="button"
                  className="colh mini"
                  title="Expand Done"
                  onClick={toggleDoneFolded}
                >
                  <Status mark={COLUMN_MARK[column.id]} />
                  <span className="n">{column.name}</span>
                  <span className="c">{counts[index]}</span>
                </button>
              );
            }
            return (
              <div
                key={column.id}
                className={`colh${folded(index) ? " mini" : ""}`}
                title={column.name}
              >
                <Status mark={COLUMN_MARK[column.id]} />
                <span className="n">{column.name}</span>
                <span className="c">{counts[index]}</span>
                {isDone ? (
                  <button
                    type="button"
                    className="ib sm hide-sm"
                    title="Collapse Done"
                    aria-label="Collapse Done"
                    onClick={toggleDoneFolded}
                  >
                    <ChevronLeftIcon />
                  </button>
                ) : null}
              </div>
            );
          })}
        </div>
        {lanes.map((project) => {
          const key = projectKey(project);
          const orchestrator = orchestrators.find(
            (agent) => agent.project_id === project.id && ownerKey(agent.project_id, agent) === key,
          );
          const hostOfOrchestrator = machines.machines.find(
            (machine) => machine.id === orchestrator?.host_id,
          );
          return (
            <Lane
              key={key}
              project={project}
              color={projectColor(projects.projects.findIndex((p) => projectKey(p) === key))}
              tiles={byProject.get(key) ?? []}
              open={!look.collapsed.includes(key)}
              focusMode={focusMode}
              orchestrator={orchestrator}
              remoteHost={hostOfOrchestrator?.kind === "remote" ? hostOfOrchestrator : undefined}
              health={
                standingOf(project, machines.machines).peer
                  ? { kind: "ok" }
                  : laneHealth(orchestrator, hostOfOrchestrator)
              }
              standing={standingOf(project, machines.machines)}
              hostLabel={hostOfOrchestrator?.label ?? "unknown"}
              doneFolded={look.doneFolded}
              filterName={FILTER_NAME[filter]}
            />
          );
        })}
      </div>
    </div>
  );
}
