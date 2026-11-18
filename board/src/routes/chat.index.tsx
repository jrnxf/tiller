import { createFileRoute } from "@tanstack/react-router";

import { ChatHome } from "../components/chat/chat-view";

export const Route = createFileRoute("/chat/")({
  component: ChatHome,
});
