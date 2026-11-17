import { createFileRoute } from "@tanstack/react-router";

import { ChatView } from "../components/chat/chat-view";
import { threadQuery } from "../lib/queries";

export interface ChatSearch {
  // The peer that owns the project; absent for this host's own.
  owner?: string;
}

export const Route = createFileRoute("/chat/$project")({
  validateSearch: (search): ChatSearch => ({
    owner: typeof search.owner === "string" && search.owner !== "" ? search.owner : undefined,
  }),
  loaderDeps: ({ search }) => ({ owner: search.owner }),
  // The thread is read before the page shows, so it opens at its newest item.
  // A thread that cannot be read still opens the page, which says why.
  loader: async ({ context, params, deps }) => {
    await context.queryClient.prefetchQuery(threadQuery(params.project, deps.owner));
  },
  component: ChatRoute,
});

function ChatRoute() {
  const { project } = Route.useParams();
  const { owner } = Route.useSearch();
  // Keyed, so moving from one chat to another starts a fresh one.
  return (
    <ChatView key={`${owner ?? ""}/${project}`} projectId={project} owner={owner} variant="page" />
  );
}
