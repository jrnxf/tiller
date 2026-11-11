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
// where it appears.
export function Inline({ error }: { error: ActionError }) {
  return (
    <p className="refusal inline" role="alert">
      {error.message}
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
