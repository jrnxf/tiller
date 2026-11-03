import { Link, createFileRoute } from "@tanstack/react-router";

import { CardPage } from "../components/board/card-page";
import { cardQuery, timelineQuery } from "../lib/queries";

export interface CardSearch {
  // The peer that owns the card; absent for this host's own (protocol.md 7.3).
  owner?: string;
}

export const Route = createFileRoute("/cards/$cardId")({
  validateSearch: (search): CardSearch => ({
    owner: typeof search.owner === "string" && search.owner !== "" ? search.owner : undefined,
  }),
  loaderDeps: ({ search }) => ({ owner: search.owner }),
  // fetchQuery answers from the cache unless an event has marked the card
  // stale, so the page is never older than the stream and never waits twice.
  loader: async ({ context, params, deps }) => {
    await Promise.all([
      context.queryClient.fetchQuery(cardQuery(params.cardId, deps.owner)),
      context.queryClient.fetchQuery(timelineQuery(params.cardId, deps.owner)),
    ]);
  },
  errorComponent: CardUnavailable,
  component: CardRoute,
});

function CardRoute() {
  const { cardId } = Route.useParams();
  const { owner } = Route.useSearch();
  // Keyed, so moving from one card to another starts a fresh page.
  return <CardPage key={`${owner ?? ""}/${cardId}`} cardId={cardId} owner={owner} />;
}

function CardUnavailable() {
  return (
    <div className="empty">
      <div>
        <b>This card could not be loaded</b>
        It may have been removed, or the daemon is not answering.
        <br />
        <Link to="/" className="btn">
          Back to the board
        </Link>
      </div>
    </div>
  );
}
