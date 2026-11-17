import { useRouterState } from "@tanstack/react-router";
import { Suspense, useEffect } from "react";

import { dialogOpen } from "../../lib/shortcuts";
import { useStore } from "../../lib/store";
import { chatDrawerStore, closeChatDrawer } from "./chat-drawer-store";
import { ChatView } from "./chat-view";

// The chat over whatever page is open, docked on the right. Its box takes
// the focus as it opens. The chat page itself needs no drawer, so going
// there closes it.
export function ChatDrawer() {
  const target = useStore(chatDrawerStore);
  const onChatPage = useRouterState({
    select: (state) => state.location.pathname.startsWith("/chat/"),
  });
  const shown = target !== null && !onChatPage;

  useEffect(() => {
    if (onChatPage) closeChatDrawer();
  }, [onChatPage]);

  // Esc closes it, unless a dialog or a popover in it takes the key first.
  useEffect(() => {
    if (!shown) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || dialogOpen()) return;
      const from = event.target;
      if (from instanceof Element && from.closest(".ask-ed, .ask-pop")) return;
      closeChatDrawer();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shown]);

  if (!shown) return null;
  return (
    <aside className="chat-drawer" aria-label="Chat" data-testid="chat-drawer">
      <Suspense fallback={null}>
        <ChatView
          key={`${target.owner ?? ""}/${target.project}`}
          projectId={target.project}
          owner={target.owner}
          variant="drawer"
        />
      </Suspense>
    </aside>
  );
}
