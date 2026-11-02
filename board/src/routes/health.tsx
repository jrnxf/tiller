import { createFileRoute } from "@tanstack/react-router";

import { ScreenPlaceholder } from "../components/shell/screen-placeholder";

export const Route = createFileRoute("/health")({
  component: () => <ScreenPlaceholder title="Health" source="task T3.5" />,
});
