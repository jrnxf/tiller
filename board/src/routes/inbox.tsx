import { createFileRoute } from "@tanstack/react-router";

import { Inbox } from "../components/attention/inbox";

interface InboxSearch {
  // The selected decision or permission prompt.
  item?: string;
}

export const Route = createFileRoute("/inbox")({
  validateSearch: (search): InboxSearch => ({
    item: typeof search.item === "string" && search.item !== "" ? search.item : undefined,
  }),
  component: InboxRoute,
});

function InboxRoute() {
  const { item, host } = Route.useSearch();
  return <Inbox selected={item} host={host} />;
}
