import { Link, createFileRoute } from "@tanstack/react-router";

import { CardPage } from "../components/board/card-page";
import { cardQuery, timelineQuery } from "../lib/queries";

export const Route = createFileRoute("/cards/$cardId")({
  // fetchQuery answers from the cache unless an event has marked the card
  // stale, so the page is never older than the stream and never waits twice.
  loader: async ({ context, params }) => {
    await Promise.all([
      context.queryClient.fetchQuery(cardQuery(params.cardId)),
      context.queryClient.fetchQuery(timelineQuery(params.cardId)),
    ]);
  },
  errorComponent: CardUnavailable,
  component: CardRoute,
});

function CardRoute() {
  const { cardId } = Route.useParams();
  // Keyed, so moving from one card to another starts a fresh page.
  return <CardPage key={cardId} cardId={cardId} />;
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
