import { createFileRoute } from "@tanstack/react-router";

import { MemoryPage, type MemoryTab } from "../components/memory/memory-page";
import { memoryQuery, projectsQuery } from "../lib/queries";

interface MemorySearch {
  // The project whose notes are open; without it Pending lists every project.
  project?: string;
  // The role whose launch Live shows; the daemon answers for builder without it.
  role?: string;
  // The open tab; without it Pending opens when it has notes, else Live.
  tab?: MemoryTab;
}

const text = (value: unknown) => (typeof value === "string" && value !== "" ? value : undefined);

export const Route = createFileRoute("/memory")({
  validateSearch: (search): MemorySearch => ({
    project: text(search.project),
    role: text(search.role),
    tab: search.tab === "pending" || search.tab === "live" ? search.tab : undefined,
  }),
  loaderDeps: ({ search }) => ({ project: search.project, role: search.role }),
  // The view is read once, with no retry (the client's default is one): a
  // failed open shows the section's note at once.
  loader: ({ context, deps }) =>
    Promise.allSettled([
      context.queryClient.ensureQueryData({
        ...memoryQuery(deps.project, deps.role),
        retry: false,
      }),
      context.queryClient.ensureQueryData(projectsQuery()),
    ]),
  component: MemoryRoute,
});

function MemoryRoute() {
  const { project, role, tab } = Route.useSearch();
  return <MemoryPage project={project} role={role} tab={tab} />;
}
