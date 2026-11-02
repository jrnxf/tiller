import type { AttentionItem } from "../../lib/api-types";
import { createStore } from "../../lib/store";

// Items answered from this tab. The open queue drops an item once it is
// answered, but the inbox keeps showing it, with its receipt, until reload.
interface Answered {
  item: AttentionItem;
  answer: string;
  // The option picked, so a form mounted again shows what was sent.
  option: string | null;
}

export const answeredStore = createStore<Record<string, Answered>>({});

export function markAnswered(item: AttentionItem, answer: string, option: string | null = null) {
  answeredStore.update((current) => ({ ...current, [item.id]: { item, answer, option } }));
}
