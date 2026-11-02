import type { QuotaDot, QuotaResponse, QuotaWindow } from "./api-types";
import { formatLisbon } from "./clock";

// The footer reads the weekly window; the server computes its dot
// (protocol.md section 8). With no weekly window there is nothing to show.
export function weeklyWindow(quota: QuotaResponse): QuotaWindow | undefined {
  return quota.windows.find((window) => window.kind === "weekly");
}

export interface QuotaFooterView {
  dot: QuotaDot;
  percentLeft: number | null;
  label: string;
  resets: string | null;
}

export function quotaFooterView(quota: QuotaResponse): QuotaFooterView | null {
  const window = weeklyWindow(quota);
  if (!window) return null;
  const percent = window.percent_remaining;
  const rounded = percent === null ? null : Math.round(percent);
  const resets = formatLisbon(window.resets_at);
  return {
    dot: window.dot,
    percentLeft: percent === null ? null : Math.min(100, Math.max(0, percent)),
    label: rounded === null ? "week unknown" : `${rounded}% week left`,
    resets: resets ? `resets ${resets}` : null,
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
