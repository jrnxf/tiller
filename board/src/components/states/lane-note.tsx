import type { LaneHealth } from "./degraded";

const iconProps = {
  className: "icon nic",
  width: 14,
  height: 14,
  viewBox: "0 0 16 16",
  "aria-hidden": true,
  focusable: false,
} as const;

const strokes = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.3,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

function UnplugIcon() {
  return (
    <svg {...iconProps}>
      <path d="M5.5 2.5V5M10.5 2.5V5M4 5h8v2a4 4 0 0 1-8 0zM8 11v3" {...strokes} />
      <path d="M2 2l12 12" {...strokes} strokeWidth={1.4} />
    </svg>
  );
}

function ClockIcon() {
  return (
    <svg {...iconProps}>
      <circle cx="8" cy="8" r="5.5" {...strokes} />
      <path d="M8 5v3.2l2 1.3" {...strokes} />
    </svg>
  );
}

// The strip under a lane header when its data cannot be trusted to be live.
export function LaneNote({
  health,
  projectName,
  hostLabel,
}: {
  health: Exclude<LaneHealth, { kind: "ok" }>;
  projectName: string;
  hostLabel: string;
}) {
  const when = health.since ? `${health.since} (${health.ago} ago)` : `${health.ago} ago`;
  if (health.kind === "disconnected") {
    return (
      <div className="lane-note err" role="status" data-testid="lane-note-disconnected">
        <UnplugIcon />
        <div className="tx">
          <b>The terminal holder is not answering</b> on the {hostLabel} host. Showing the last
          known state from {when}. Answers and Open in terminal pause until it is back.
        </div>
      </div>
    );
  }
  return (
    <div className="lane-note warn" role="status" data-testid="lane-note-stale">
      <ClockIcon />
      <div className="tx">
        <b>Stale lane.</b> No events from the {projectName} orchestrator since {when}. Card
        positions may be out of date.
      </div>
    </div>
  );
}
