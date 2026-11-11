import { createFileRoute } from "@tanstack/react-router";

import { MemoryPage } from "../components/memory/memory-page";
import { memoryQuery, projectsQuery } from "../lib/queries";

interface MemorySearch {
  // The project whose document is open; without it the shared one is.
  doc?: string;
}

export const Route = createFileRoute("/memory")({
  // The shared document is the one with no `doc`, so `?doc=fleet` is the same
  // search and reads it once, not once as `fleet` and once as the default.
  validateSearch: (search): MemorySearch => ({
    doc:
      typeof search.doc === "string" && search.doc !== "" && search.doc !== "fleet"
        ? search.doc
        : undefined,
  }),
  loaderDeps: ({ search }) => ({ doc: search.doc }),
  // The document is read once, with no retry (the client's default is one):
  // a failed open shows the section's note at once. Its Retry reads the
  // document again, and the page says it is reading until the read answers.
  loader: ({ context, deps }) =>
    Promise.allSettled([
      context.queryClient.ensureQueryData({ ...memoryQuery(deps.doc), retry: false }),
      context.queryClient.ensureQueryData(projectsQuery()),
    ]),
  component: MemoryRoute,
});

function MemoryRoute() {
  const { doc } = Route.useSearch();
  return <MemoryPage scope={doc ?? "fleet"} />;
}
