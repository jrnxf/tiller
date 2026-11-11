import { useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";

import { API_KEY_PREFIX } from "../../lib/queries";
import type { ActionError } from "../attention/api";
import { lisbonDay, lisbonShortDay } from "../health/lisbon";

// The small pieces the memory page and its dialogs share.

export const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

// "Wed 4 Nov" and "Wed 4 Nov, 17:48", in Lisbon time.
export const day = (iso: string) => lisbonShortDay(iso);
export const when = (iso: string) => `${lisbonShortDay(iso)}, ${lisbonDay(iso).time}`;

export const oneLine = (text: string | null | undefined) =>
  (text ?? "").replace(/\s+/g, " ").trim();

// A free-text reason as a sentence of its own: a capital letter first and a
// full stop last, so "seen twice" reads "Seen twice.".
export function sentence(text: string): string {
  const trimmed = oneLine(text);
  if (trimmed === "") return "";
  const capital = `${trimmed.charAt(0).toUpperCase()}${trimmed.slice(1)}`;
  return /[.!?]$/.test(capital) ? capital : `${capital}.`;
}

// A refusal or a note shown beside the control it is about, and announced
// where it appears. `about` is the entity the control is, such as
// "proposal <id>": a refusal of it shows its reason alone, as a sentence,
// since the control already says which one it is.
export function Inline({ error, about }: { error: ActionError; about?: string }) {
  const prefix = about ? `${about}: ` : null;
  const message =
    prefix && error.message.startsWith(prefix)
      ? sentence(error.message.slice(prefix.length))
      : error.message;
  return (
    <p className="refusal inline" role="alert">
      {message}
    </p>
  );
}

// Reads every memory query again: the document, the tabs and any plan.
export function useMemoryRefresh(): () => Promise<void> {
  const queryClient = useQueryClient();
  return useCallback(
    () =>
      queryClient.invalidateQueries({
        queryKey: API_KEY_PREFIX,
        predicate: (query) => String(query.queryKey[1] ?? "").startsWith("/api/v1/memory"),
      }),
    [queryClient],
  );
}
