import { createFileRoute } from "@tanstack/react-router";

import { ScreenPlaceholder } from "../components/shell/screen-placeholder";

export const Route = createFileRoute("/inbox")({
  component: () => <ScreenPlaceholder title="Inbox" source="task T3.3" />,
});
