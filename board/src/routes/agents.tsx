import { createFileRoute } from "@tanstack/react-router";

import { ScreenPlaceholder } from "../components/shell/screen-placeholder";

export const Route = createFileRoute("/agents")({
  component: () => <ScreenPlaceholder title="Agents" source="task T3.2" />,
});
