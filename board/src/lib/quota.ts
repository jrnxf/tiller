import type { QuotaDot, QuotaResponse, QuotaWindow } from "./api-types";
import { formatLisbonRounded, serverNow } from "./clock";

// The footer reads the weekly window; the server computes its dot
// (protocol.md section 8). With no weekly window there is nothing to show.
export function weeklyWindow(quota: QuotaResponse): QuotaWindow | undefined {
  return quota.windows.find((window) => window.kind === "weekly");
}

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

// "3d 7h", "5h 12m", "42m": the two largest units, as `quota-axi --tui` writes them.
export function formatSpan(ms: number): string {
  const minutes = Math.max(0, Math.round(ms / 60_000));
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes % 60}m`;
  return `${minutes}m`;
}

function until(iso: string | null, now: number): number | null {
  if (!iso) return null;
  const at = Date.parse(iso);
  return Number.isNaN(at) ? null : at - now;
}

export interface QuotaFooterView {
  dot: QuotaDot;
  percentLeft: number | null;
  paceMarker: number | null;
  label: string;
  resets: string | null;
  // "empty in 3d 7h", only when the pace runs the week out before it resets.
  runway: string | null;
  // Why the dot has its colour.
  reason: string;
  // What the card says beside the percent: the runway, else "out of date" for a
  // stale reading, else the reset.
  note: string;
}

function paceMarker(window: QuotaWindow): number | null {
  return window.elapsed_percent === null ? null : clampPercent(100 - window.elapsed_percent);
}

function runwayOf(window: QuotaWindow, now: number): string | null {
  if (window.stale || window.percent_remaining === null) return null;
  const left = until(window.projected_exhaustion_at, now);
  if (left === null || left <= 0) return null;
  const reset = until(window.resets_at, now);
  if (reset !== null && left >= reset) return null;
  return `empty in ${formatSpan(left)}`;
}

function reasonOf(window: QuotaWindow, runway: string | null): string {
  if (window.stale) return "Reading is out of date";
  if (window.percent_remaining === null) return "No reading yet";
  const tail = runway ? `, ${runway}` : "";
  switch (window.dot) {
    case "red":
      return `Near the floor${tail}`;
    case "amber":
      return `Spending ahead of pace${tail}`;
    case "grey":
      return "Reading is out of date";
    default:
      return "On pace";
  }
}

export function quotaFooterView(quota: QuotaResponse, now = serverNow()): QuotaFooterView | null {
  const window = weeklyWindow(quota);
  if (!window) return null;
  const percent = window.percent_remaining;
  const rounded = percent === null ? null : Math.round(percent);
  const resets = formatLisbonRounded(window.resets_at);
  const runway = runwayOf(window, now);
  const reason = reasonOf(window, runway);
  return {
    dot: window.dot,
    percentLeft: percent === null ? null : clampPercent(percent),
    paceMarker: paceMarker(window),
    label: rounded === null ? "week unknown" : `${rounded}% week left`,
    resets: resets ? `resets ${resets}` : null,
    runway,
    reason,
    note: runway ?? (window.dot === "grey" ? "out of date" : resets ? `resets ${resets}` : ""),
  };
}

export function daemonTooltip(
  connection: "connecting" | "live" | "reconnecting",
  quota: QuotaWindow | undefined,
): string {
  const daemon =
    connection === "live"
      ? "Daemon live"
      : connection === "reconnecting"
        ? "Daemon not answering, showing the last known state"
        : "Daemon connecting";
  if (!quota) return `${daemon}. No weekly quota window reported.`;
  const dot: Record<QuotaDot, string> = {
    green: "on pace",
    amber: "spending ahead of pace",
    red: "near exhaustion",
    grey: "quota data is stale",
  };
  return `${daemon}. Quota ${dot[quota.dot]}.`;
}
