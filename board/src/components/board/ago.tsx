import { useSyncExternalStore } from "react";

import { formatAge, secondsSince } from "./format";

// One shared one-second ticker: each age re-renders itself, nothing above it.
const listeners = new Set<() => void>();
let tick = 0;
let timer: ReturnType<typeof setInterval> | undefined;

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1) {
    timer = setInterval(() => {
      tick += 1;
      for (const notify of listeners) notify();
    }, 1000);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) clearInterval(timer);
  };
}

const getTick = () => tick;

// Re-renders the caller once a second, for text whose words move with the clock.
export function useTick() {
  useSyncExternalStore(subscribe, getTick, getTick);
}

// "observed 12 s ago": the age text moves with the clock, without an event.
export function Ago({
  at,
  prefix = "",
  oldAfter,
}: {
  at: string | null;
  prefix?: string;
  // Seconds after which the text reads as old (amber).
  oldAfter?: number;
}) {
  useTick();
  const seconds = secondsSince(at);
  if (seconds === null) return <span className="obs">{prefix ? `${prefix} never` : "never"}</span>;
  const old = oldAfter !== undefined && seconds > oldAfter;
  return (
    <span className={`obs${old ? " old" : ""}`}>
      {prefix ? `${prefix} ` : ""}
      {formatAge(seconds)} ago
    </span>
  );
}

// "for 12 min": how long a state has lasted, moving with the clock.
export function Lasting({ since }: { since: string }) {
  useTick();
  const seconds = secondsSince(since);
  return seconds === null ? null : <span className="obs">for {formatAge(seconds)}</span>;
}
