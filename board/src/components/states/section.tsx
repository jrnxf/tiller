import { QueryErrorResetBoundary, useQueryClient } from "@tanstack/react-query";
import { Component, Suspense, useEffect, useRef } from "react";
import type { ReactNode } from "react";

import { connectionStore } from "../../lib/connection";
import { ReadError } from "../../lib/queries";

// How often a failed section asks again on its own. The stream coming back
// live (a restarted daemon), or another part of the screen reading the same
// path, asks at once.
export const SECTION_RETRY_MS = 15_000;

export interface SectionFailure {
  label: string;
  reason: string;
  retry: () => void;
}

// Why a read failed, as one sentence.
export function failureReason(error: unknown): string {
  const reason =
    error instanceof ReadError
      ? (error.reason ?? `The daemon answered ${error.status}`)
      : error instanceof Error
        ? error.message
        : "It could not be read";
  return /[.!?]$/.test(reason) ? reason : `${reason}.`;
}

// One clock for every failed part of the screen, so parts that failed on the
// same read retry together and the query layer sends one request, not one per
// part.
const ticks = new Set<() => void>();
let clock: ReturnType<typeof setInterval> | undefined;

function onRetryTick(listener: () => void): () => void {
  ticks.add(listener);
  clock ??= setInterval(() => {
    for (const tick of ticks) tick();
  }, SECTION_RETRY_MS);
  return () => {
    ticks.delete(listener);
    if (ticks.size === 0) {
      clearInterval(clock);
      clock = undefined;
    }
  };
}

// Calls `retry` on the shared clock, when the stream reconnects, and when the
// read that failed (`error`, when it is a ReadError) gets an answer elsewhere,
// so a failed part of the screen heals by itself once the daemon answers again.
export function useAutoRetry(retry: () => void, error: unknown) {
  const queryClient = useQueryClient();
  const failedPath = error instanceof ReadError ? error.path : null;
  // The latest `retry`, so a caller that makes a new one each render does not
  // restart the subscriptions.
  const latest = useRef(retry);
  useEffect(() => {
    latest.current = retry;
  });
  useEffect(() => {
    const again = () => latest.current();
    const offTick = onRetryTick(again);
    let was = connectionStore.get();
    const offConnection = connectionStore.subscribe(() => {
      const now = connectionStore.get();
      if (now === "live" && was !== "live") again();
      was = now;
    });
    // A query's key is its path (queries.ts), the same path the error names.
    const offCache = queryClient.getQueryCache().subscribe((event) => {
      if (
        failedPath !== null &&
        event.type === "updated" &&
        event.action.type === "success" &&
        event.query.queryKey[1] === failedPath
      ) {
        again();
      }
    });
    return () => {
      offTick();
      offConnection();
      offCache();
    };
  }, [queryClient, failedPath]);
}

function WarnIcon() {
  return (
    <svg className="icon" width={14} height={14} viewBox="0 0 16 16" aria-hidden="true">
      <path
        d="M8 2.5 14 13H2zM8 6.5v3M8 11.4v.1"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.3}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// The degraded state of one part of the screen: what is missing, why, and a
// retry. The rest of the board stays usable.
export function SectionNote({ label, reason, retry }: SectionFailure) {
  return (
    <div className="sec-note" role="status" data-testid="section-unavailable">
      <WarnIcon />
      <div className="tx">
        <b>{label} unavailable.</b> {reason}
      </div>
      <button type="button" className="lnk" onClick={retry}>
        Retry
      </button>
    </div>
  );
}

function Failed({
  failure,
  error,
  render,
}: {
  failure: SectionFailure;
  error: unknown;
  render: (failure: SectionFailure) => ReactNode;
}) {
  useAutoRetry(failure.retry, error);
  return render(failure);
}

interface CatchProps {
  label: string;
  onReset: () => void;
  render: (failure: SectionFailure) => ReactNode;
  children: ReactNode;
}

interface CatchState {
  failed: boolean;
  // Kept through a retry, so the note stays up while the read is in flight
  // instead of flickering away and back; cleared once the children show.
  error: { value: unknown } | null;
}

// Mounts only when the children inside the same Suspense have rendered.
function Shown({ onShown }: { onShown: () => void }) {
  useEffect(onShown, [onShown]);
  return null;
}

class Catch extends Component<CatchProps, CatchState> {
  override state: CatchState = { failed: false, error: null };

  static getDerivedStateFromError(value: unknown): CatchState {
    return { failed: true, error: { value } };
  }

  retry = () => {
    this.props.onReset();
    this.setState({ failed: false });
  };

  healed = () => {
    if (this.state.error) this.setState({ error: null });
  };

  override render() {
    const { label, render, children } = this.props;
    const { failed, error } = this.state;
    const failure = error && { label, reason: failureReason(error.value), retry: this.retry };
    if (failed && failure) return <Failed failure={failure} error={error.value} render={render} />;
    return (
      <Suspense fallback={failure ? render(failure) : null}>
        {children}
        {error ? <Shown onShown={this.healed} /> : null}
      </Suspense>
    );
  }
}

// One part of the screen that reads its own queries. When a read fails it
// shows `fallback` (by default a note naming `label`) in place of `children`,
// never the whole app's error screen, and tries again by itself.
export function Section({
  label,
  fallback = SectionNote,
  children,
}: {
  label: string;
  fallback?: (failure: SectionFailure) => ReactNode;
  children: ReactNode;
}) {
  return (
    <QueryErrorResetBoundary>
      {({ reset }) => (
        <Catch label={label} onReset={reset} render={fallback}>
          {children}
        </Catch>
      )}
    </QueryErrorResetBoundary>
  );
}
