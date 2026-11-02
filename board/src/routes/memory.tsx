import { createFileRoute } from "@tanstack/react-router";

import { ScreenPlaceholder } from "../components/shell/screen-placeholder";

export const Route = createFileRoute("/memory")({
  component: () => <ScreenPlaceholder title="Memory" source="a later task" />,
});
