import { createFileRoute } from "@tanstack/react-router";

import { ContributionsView } from "../components/board/contributions-view";
import { contributionsQuery, projectsQuery } from "../lib/queries";

export interface ContributionsSearch {
  // The project whose PRs are listed; without it every project's are.
  project?: string;
}

export const Route = createFileRoute("/contributions")({
  validateSearch: (search): ContributionsSearch => ({
    project:
      typeof search.project === "string" && search.project !== "" ? search.project : undefined,
  }),
  loaderDeps: ({ search }) => ({ project: search.project }),
  loader: ({ context, deps }) =>
    Promise.all([
      context.queryClient.ensureQueryData(contributionsQuery(deps.project)),
      context.queryClient.ensureQueryData(projectsQuery()),
    ]),
  component: ContributionsRoute,
});

function ContributionsRoute() {
  const { project } = Route.useSearch();
  return <ContributionsView project={project} />;
}
