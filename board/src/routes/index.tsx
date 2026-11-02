import { createFileRoute } from "@tanstack/react-router";

import { ScreenPlaceholder } from "../components/shell/screen-placeholder";

export const Route = createFileRoute("/")({
  component: () => <ScreenPlaceholder title="Board" source="task T3.2" />,
});
