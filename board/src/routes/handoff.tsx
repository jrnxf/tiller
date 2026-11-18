import { createFileRoute } from "@tanstack/react-router";

import { HandoffPage } from "../components/states/handoff-page";

interface HandoffSearch {
  to?: string;
  path?: string;
}

const text = (value: unknown) => (typeof value === "string" && value !== "" ? value : undefined);

// A live board's "Sign in from" link lands here, on the board already signed
// in: it asks, then hands that name a one-time login link.
export const Route = createFileRoute("/handoff")({
  validateSearch: (search): HandoffSearch => ({ to: text(search.to), path: text(search.path) }),
  component: HandoffRoute,
});

function HandoffRoute() {
  const { to, path } = Route.useSearch();
  return <HandoffPage to={to} path={path} />;
}
