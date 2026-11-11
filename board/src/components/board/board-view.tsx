import { Tooltip } from "@base-ui/react/tooltip";
import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate, useRouter } from "@tanstack/react-router";
import { Component, Suspense, useEffect } from "react";
import type { ReactNode } from "react";
import type { CSSProperties } from "react";

import type { BoardFilter } from "../../lib/api-types";
import { ownerKey, projectKey, standingOf } from "../../lib/peers";
import { projectColor } from "../../lib/project-color";
import { agentsQuery, boardQuery, machinesQuery, projectsQuery } from "../../lib/queries";
import { prefsStore, updatePrefs } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { useStore } from "../../lib/store";
import { boardLookStore, toggleDoneFolded } from "./board-prefs";
import { ChevronLeftIcon, Status } from "./icons";
import { laneHealth } from "../states/degraded";
import { SECTION_RETRY_MS, SectionNote, failureReason } from "../states/section";
import { useMovingBoard } from "./glide";
import { bugProjectOf, machineOf } from "./bugs";
import { Lane } from "./lane";
import { COLUMNS, onHost, passesFilter, tilesOf } from "./model";
import type { ColumnId, Tile } from "./model";
import { LayersIcon, CloseIcon } from "../shell/icons";
import { CardPage } from "./card-page";
import "./board.css";
import { useNamedProjects } from "./peer-state";

const FILTER_NAME: Record<BoardFilter, string | null> = {
  all: null,
  active: "Active",
  needs: "Needs you",
};

const COLUMN_MARK = {
  asked: "asked",
  queued: "queued",
  working: "working",
  landing: "landing",
  done: "done",
} as const;

// Width of a folded column, an empty one or Done folded by hand: the same
// 52 px the mocks use.
const MINI_WIDTH = 52;
// Every column with cards shares the board's width, down to a readable width:
// below it the board scrolls sideways instead of squeezing the cards.
const FULL_WIDTH = 168;
const GAP = 8;

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

// A card that cannot be read must not take the board down with it.
class DrawerBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="empty">
        <div>
          <b>This card could not be loaded</b>
          It may have been removed, or the daemon is not answering.
          <br />
          <Link
            to="/"
            search={(prev) => ({ ...prev, card: undefined, owner: undefined })}
            className="btn"
          >
            Close
          </Link>
        </div>
      </div>
    );
  }
}

// The card a click opened, docked beside the board: the lanes shrink to make
// room and stay in view. Expanding it opens the full card page.
function CardDrawer({ cardId, owner }: { cardId: string; owner: string | undefined }) {
  const navigate = useNavigate();
  const router = useRouter();
  // "Open the full page" is a link to the card route. Fetch that route's code
  // once now, so the click does not wait for it on a slow network. Only the
  // code: the drawer reads the card itself, so the route's loader would read
  // it (and, for a card that cannot be read, fail) a second time.
  useEffect(() => {
    // Only a head start: a chunk that fails here loads (or says why it
    // cannot) when the full page opens, and the stale board banner hears it.
    // It returns nothing when the code is in already.
    void Promise.resolve(router.loadRouteChunk(router.routesById["/cards/$cardId"])).catch(
      () => {},
    );
  }, [router]);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || shortcutsBlocked(event)) return;
      // A popover or a menu takes its own Escape.
      const target = event.target;
      if (target instanceof Element && target.closest("[role=dialog], [role=menu]")) return;
      void navigate({
        to: "/",
        search: (prev) => ({ ...prev, card: undefined, owner: undefined }),
      });
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [navigate]);
  return (
    <aside className="drawer" aria-label="Card details" data-testid="card-drawer">
      <DrawerBoundary key={`${owner ?? ""}/${cardId}`}>
        <Suspense fallback={null}>
          <CardPage cardId={cardId} owner={owner} variant="drawer" />
        </Suspense>
      </DrawerBoundary>
    </aside>
  );
}

export function BoardView({
  host,
  filter,
  card,
  owner,
}: {
  host: string | undefined;
  filter: BoardFilter;
  card?: string;
  owner?: string;
}) {
  return (
    <div className="board-split" data-docked={card ? "true" : "false"}>
      <Board host={host} filter={filter} />
      {card ? <CardDrawer cardId={card} owner={owner} /> : null}
    </div>
  );
}

function Board({ host, filter }: { host: string | undefined; filter: BoardFilter }) {
  const { data: live } = useSuspenseQuery(boardQuery());
  const board = useMovingBoard(live);
  const projects = useNamedProjects(useSuspenseQuery(projectsQuery()).data);
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
  if (lanes.length === 0) {
    return (
      <div className="board-view">
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
  // An empty column folds to its icon, unless no column has a card: then
  // every column keeps its width rather than all of them folding.
  const empty = (index: number) => shown.length > 0 && counts[index] === 0;
  const folded = (index: number) =>
    empty(index) || (COLUMNS[index]?.id === "done" && look.doneFolded && (counts[index] ?? 0) > 0);
  const widths = COLUMNS.map((_, index) =>
    folded(index) ? `${MINI_WIDTH}px` : `minmax(${FULL_WIDTH}px,1fr)`,
  ).join(" ");
  const full = COLUMNS.filter((_, index) => !folded(index)).length;
  const minWidth =
    32 + (COLUMNS.length - 1) * GAP + full * FULL_WIDTH + (COLUMNS.length - full) * MINI_WIDTH;

  return (
    <div className="board-view">
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
        data-testid="board"
      >
        <div className="bhead">
          {COLUMNS.map((column, index) => {
            const isDone = column.id === "done";
            if (empty(index)) {
              const label = `${column.name} ${counts[index]}`;
              return (
                <div key={column.id} className="colh mini" data-empty="true">
                  <Tooltip.Root>
                    <Tooltip.Trigger
                      delay={250}
                      render={
                        <span className="colh-t" role="img" tabIndex={0} aria-label={label} />
                      }
                    >
                      <Status mark={COLUMN_MARK[column.id]} />
                    </Tooltip.Trigger>
                    <Tooltip.Portal>
                      <Tooltip.Positioner className="colh-tip-pos" side="bottom" sideOffset={6}>
                        <Tooltip.Popup className="colh-tip">
                          <b>{column.name}</b> {counts[index]}
                        </Tooltip.Popup>
                      </Tooltip.Positioner>
                    </Tooltip.Portal>
                  </Tooltip.Root>
                </div>
              );
            }
            if (isDone && folded(index)) {
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
          const owner = machineOf(project, machines.machines);
          return (
            <Lane
              key={key}
              project={project}
              color={projectColor(projects.projects.findIndex((p) => projectKey(p) === key))}
              tiles={byProject.get(key) ?? []}
              open={!look.collapsed.includes(key)}
              focusMode={focusMode}
              orchestrator={orchestrator}
              orchestratorsKnown={agentsRead.data !== undefined}
              remoteHost={hostOfOrchestrator?.kind === "remote" ? hostOfOrchestrator : undefined}
              health={
                standingOf(project, machines.machines).peer
                  ? { kind: "ok" }
                  : laneHealth(orchestrator, hostOfOrchestrator)
              }
              standing={standingOf(project, machines.machines)}
              hostLabel={hostOfOrchestrator?.label ?? "unknown"}
              owner={owner}
              bugProject={bugProjectOf(owner, projects.projects)}
              doneFolded={look.doneFolded}
              filterName={FILTER_NAME[filter]}
            />
          );
        })}
      </div>
    </div>
  );
}
