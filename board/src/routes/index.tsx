import { createFileRoute } from "@tanstack/react-router";

import { BoardView } from "../components/board/board-view";
import { parseFilter } from "../components/board/model";

export interface BoardSearch {
  // All is the default and stays out of the URL.
  filter?: "active" | "needs";
}

export const Route = createFileRoute("/")({
  validateSearch: (search): BoardSearch => {
    const filter = parseFilter(search.filter);
    return { filter: filter === "all" ? undefined : filter };
  },
  component: BoardRoute,
});

function BoardRoute() {
  const { filter, host } = Route.useSearch();
  return <BoardView host={host} filter={parseFilter(filter)} />;
}
