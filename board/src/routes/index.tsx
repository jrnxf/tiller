import { createFileRoute } from "@tanstack/react-router";

import { BoardView } from "../components/board/board-view";
import { parseFilter } from "../components/board/model";
import { cardQuery, timelineQuery } from "../lib/queries";

export interface BoardSearch {
  // All is the default and stays out of the URL.
  filter?: "active" | "needs";
  // The card open in the drawer, or else the ask, and the peer that owns it.
  card?: string;
  ask?: string;
  owner?: string;
}

const text = (value: unknown) => (typeof value === "string" && value !== "" ? value : undefined);

export const Route = createFileRoute("/")({
  validateSearch: (search): BoardSearch => {
    const filter = parseFilter(search.filter);
    const card = text(search.card);
    // One drawer at a time: a card opened over an ask takes its place.
    const ask = card ? undefined : text(search.ask);
    return {
      filter: filter === "all" ? undefined : filter,
      card,
      ask,
      owner: card || ask ? text(search.owner) : undefined,
    };
  },
  loaderDeps: ({ search }) => ({ card: search.card, owner: search.owner }),
  loader: async ({ context, deps }) => {
    if (!deps.card) return;
    // A card that cannot be read leaves the drawer to say so; the board still shows.
    await Promise.allSettled([
      context.queryClient.fetchQuery(cardQuery(deps.card, deps.owner)),
      context.queryClient.fetchQuery(timelineQuery(deps.card, deps.owner)),
    ]);
  },
  component: BoardRoute,
});

function BoardRoute() {
  const { filter, host, card, ask, owner } = Route.useSearch();
  return <BoardView host={host} filter={parseFilter(filter)} card={card} ask={ask} owner={owner} />;
}
