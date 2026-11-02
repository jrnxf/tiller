import { useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";

import type { BoardFilter } from "../../lib/api-types";
import { boardQuery, projectsQuery } from "../../lib/queries";
import { prefsStore } from "../../lib/prefs";
import { scopedProjects } from "../../lib/scope";
import { useStore } from "../../lib/store";
import { countsByFilter, onHost, parseFilter, tilesOf } from "./model";
import "./board.css";

// Only rendered on the board route, where its search is always present.
const boardRoute = getRouteApi("/");

const FILTERS: { value: BoardFilter; label: string; tab: string }[] = [
  { value: "all", label: "All", tab: "All cards" },
  { value: "active", label: "Active", tab: "Active" },
  { value: "needs", label: "Needs you", tab: "Needs you" },
];

// The filter and its counts, shared by the header segment (desktop) and the tab
// row (phones). It lives in the URL, so a filtered board can be linked and the
// back button undoes it.
function useBoardFilter() {
  const search = boardRoute.useSearch();
  const navigate = useNavigate();
  const { data: board } = useSuspenseQuery(boardQuery());
  const { data: projects } = useSuspenseQuery(projectsQuery());
  const prefs = useStore(prefsStore);

  const filter = parseFilter(search.filter);
  const lanes = new Set(
    scopedProjects(prefs, projects.projects, board.rows, search.host).map((p) => p.id),
  );
  const tiles = tilesOf(board).filter(
    (tile) => lanes.has(tile.projectId) && onHost(tile, search.host),
  );
  const counts = countsByFilter(tiles);

  const select = (value: BoardFilter) =>
    void navigate({
      to: "/",
      search: (prev) => ({ ...prev, filter: value === "all" ? undefined : value }),
    });
  return { filter, counts, select };
}

export function BoardFilterSegment() {
  const { filter, counts, select } = useBoardFilter();
  return (
    <div className="seg bseg hide-sm" role="group" aria-label="Cards shown">
      {FILTERS.map(({ value, label }) => (
        <button
          key={value}
          type="button"
          aria-pressed={filter === value}
          onClick={() => select(value)}
        >
          {label}
          <span className="tc">{counts[value]}</span>
        </button>
      ))}
    </div>
  );
}

export function BoardFilterTabs() {
  const { filter, counts, select } = useBoardFilter();
  return (
    <div className="tabs show-sm" role="group" aria-label="Cards shown">
      {FILTERS.map(({ value, tab }) => (
        <button
          key={value}
          type="button"
          className="tab"
          aria-pressed={filter === value}
          onClick={() => select(value)}
        >
          {tab}
          <span className="tc">{counts[value]}</span>
        </button>
      ))}
    </div>
  );
}
