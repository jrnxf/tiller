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

// Items withdrawn from this tab. Like an answered item, a withdrawn one leaves
// the open queue but stays in the inbox, with its receipt, until reload.
export interface Withdrawn {
  item: AttentionItem;
  at: string;
}

export const withdrawnStore = createStore<Record<string, Withdrawn>>({});

// Prompts and dialogs answered from this tab, by Inbox entry id. An entry id
// names the session, not the prompt, so each mark keeps when the wait it
// answered began: a later wait of the same session drops the mark, and its
// detail never shows an earlier receipt. The agent moves on and the entry
// leaves the inbox, so its detail says the answer went from here, not
// elsewhere.
export interface Sent {
  since: string;
  receipt: string;
}

export const sentStore = createStore<Record<string, Sent>>({});

export function markSent(id: string, since: string, receipt: string) {
  sentStore.update((current) => ({ ...current, [id]: { since, receipt } }));
}

// Drops each mark whose entry is back in the inbox for a later wait.
export function dropStaleSent(shown: { id: string; since: string }[]) {
  const current = sentStore.get();
  const stale = shown.filter((entry) => {
    const mark = current[entry.id];
    return mark !== undefined && mark.since !== entry.since;
  });
  if (stale.length === 0) return;
  sentStore.update((marks) => {
    const next = { ...marks };
    for (const entry of stale) delete next[entry.id];
    return next;
  });
}

export function markWithdrawn(item: AttentionItem, at: string) {
  withdrawnStore.update((current) => ({ ...current, [item.id]: { item, at } }));
}
