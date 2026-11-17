import { createStore } from "../../lib/store";

// The project whose chat is open in the drawer over the page, by its owner:
// `fleet` exists on every host. Null while the drawer is closed.
export interface ChatTarget {
  project: string;
  owner?: string;
}

export const chatDrawerStore = createStore<ChatTarget | null>(null);

export function openChatDrawer(target: ChatTarget) {
  chatDrawerStore.set(target);
}

export function closeChatDrawer() {
  chatDrawerStore.set(null);
}
