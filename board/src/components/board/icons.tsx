import type { ReactNode } from "react";

// Icons for the board views, drawn from the approved mocks
// (docs/design/mocks.html). The status marks use the 14 px grid, the rest 16 px.
function Glyph({
  children,
  size = 16,
  grid = 16,
  className = "icon",
}: {
  children: ReactNode;
  size?: number;
  grid?: number;
  className?: string;
}) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox={`0 0 ${grid} ${grid}`}
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

const stroke = { fill: "none", stroke: "currentColor", strokeWidth: 1.3 } as const;
const round = { strokeLinecap: "round", strokeLinejoin: "round" } as const;

export const PrIcon = () => (
  <Glyph>
    <circle cx="4.5" cy="4" r="1.5" {...stroke} />
    <circle cx="4.5" cy="12" r="1.5" {...stroke} />
    <circle cx="11.5" cy="12" r="1.5" {...stroke} />
    <path
      d="M4.5 5.5v5M11.5 10.5V6.5a2 2 0 0 0-2-2H7.5M8.8 3.2 7.5 4.5l1.3 1.3"
      {...stroke}
      {...round}
    />
  </Glyph>
);

export const HelpIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="5.8" {...stroke} />
    <path d="M6.3 6.3a1.8 1.8 0 1 1 2.6 1.6c-.6.3-.9.7-.9 1.3" {...stroke} {...round} />
    <circle cx="8" cy="11.2" r=".75" fill="currentColor" />
  </Glyph>
);

export const PaneIcon = () => (
  <Glyph>
    <rect x="2" y="3" width="12" height="10" rx="1.5" {...stroke} />
    <path d="M4.8 6.5 6.8 8l-2 1.5M8 10h3" {...stroke} {...round} />
  </Glyph>
);

export const ChevronDownIcon = ({ className = "icon chev" }: { className?: string }) => (
  <Glyph size={12} grid={12} className={className}>
    <path d="M3 4.5 6 7.5 9 4.5" {...stroke} strokeWidth={1.4} {...round} />
  </Glyph>
);

export const ChevronLeftIcon = () => (
  <Glyph size={12} grid={12}>
    <path d="M7.5 3 4.5 6 7.5 9" {...stroke} strokeWidth={1.4} {...round} />
  </Glyph>
);

export const ExpandIcon = () => (
  <Glyph size={12} grid={12}>
    <path
      d="M7 2h3v3M5 10H2V7M10 2 6.8 5.2M2 10l3.2-3.2"
      {...stroke}
      strokeWidth={1.3}
      {...round}
    />
  </Glyph>
);

export const ChevronRightIcon = () => (
  <Glyph size={12} grid={12}>
    <path d="M4.5 3 7.5 6 4.5 9" {...stroke} strokeWidth={1.4} {...round} />
  </Glyph>
);

export const WarnIcon = () => (
  <Glyph>
    <path d="M8 2.5 14.2 13H1.8z" {...stroke} strokeLinejoin="round" />
    <path d="M8 6.5v3" {...stroke} strokeWidth={1.4} strokeLinecap="round" />
    <circle cx="8" cy="11.2" r=".8" fill="currentColor" />
  </Glyph>
);

export const UserIcon = () => (
  <Glyph>
    <circle cx="8" cy="5.5" r="2.5" {...stroke} />
    <path d="M3 13.5c.6-2.4 2.6-3.8 5-3.8s4.4 1.4 5 3.8" {...stroke} strokeLinecap="round" />
  </Glyph>
);

export const BoltIcon = () => (
  <Glyph>
    <path d="M9 2 3.5 9H8l-1 5 5.5-7H8z" {...stroke} strokeLinejoin="round" />
  </Glyph>
);

export const SendIcon = () => (
  <Glyph>
    <path d="M2.5 8 13.5 2.5 9 13.5 7.5 9z" {...stroke} strokeLinejoin="round" />
  </Glyph>
);

// ---- status marks (14 px grid), one per column and badge ----

const STATUS = {
  asked: (
    <circle
      cx="7"
      cy="7"
      r="6"
      fill="none"
      stroke="var(--todo)"
      strokeWidth="1.5"
      strokeDasharray="1.4 1.74"
    />
  ),
  queued: <circle cx="7" cy="7" r="6" fill="none" stroke="var(--todo)" strokeWidth="1.5" />,
  parked: (
    <>
      <circle cx="7" cy="7" r="6" fill="none" stroke="var(--todo)" strokeWidth="1.5" />
      <path
        d="M5.6 4.8v4.4M8.4 4.8v4.4"
        stroke="var(--todo)"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </>
  ),
  working: (
    <>
      <circle
        className="ring"
        cx="7"
        cy="7"
        r="6"
        fill="none"
        stroke="var(--yellow)"
        strokeWidth="1.5"
      />
      <circle
        className="core"
        cx="7"
        cy="7"
        r="2"
        fill="none"
        stroke="var(--yellow)"
        strokeWidth="4"
        strokeDasharray="6.28 100"
        transform="rotate(-90 7 7)"
      />
    </>
  ),
  needs_you: (
    <>
      <circle cx="7" cy="7" r="6.75" fill="var(--orange)" />
      <path d="M7 3.6v4.1" stroke="var(--bg-card)" strokeWidth="1.6" strokeLinecap="round" />
      <circle cx="7" cy="10.1" r=".95" fill="var(--bg-card)" />
    </>
  ),
  landing: (
    <>
      <circle
        className="ring"
        cx="7"
        cy="7"
        r="6"
        fill="none"
        stroke="var(--green)"
        strokeWidth="1.5"
      />
      <circle
        className="core"
        cx="7"
        cy="7"
        r="2"
        fill="none"
        stroke="var(--green)"
        strokeWidth="4"
        strokeDasharray="9.42 100"
        transform="rotate(-90 7 7)"
      />
    </>
  ),
  done: (
    <>
      <circle cx="7" cy="7" r="6.75" fill="var(--indigo)" />
      <path
        d="M4.3 7.2 6.2 9 9.8 5.2"
        fill="none"
        stroke="#fff"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </>
  ),
  fail: (
    <>
      <circle cx="7" cy="7" r="6.75" fill="var(--red)" />
      <path
        d="M4.8 4.8l4.4 4.4M9.2 4.8l-4.4 4.4"
        stroke="#fff"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </>
  ),
  ok: (
    <>
      <circle cx="7" cy="7" r="6" fill="none" stroke="var(--green)" strokeWidth="1.5" />
      <path
        d="M4.4 7.2 6.2 9 9.6 5.3"
        fill="none"
        stroke="var(--green)"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </>
  ),
  retired: (
    <>
      <circle cx="7" cy="7" r="6" fill="none" stroke="var(--fg-4)" strokeWidth="1.5" />
      <path d="M4.5 7h5" stroke="var(--fg-4)" strokeWidth="1.5" strokeLinecap="round" />
    </>
  ),
} satisfies Record<string, ReactNode>;

export type StatusMark = keyof typeof STATUS;

export function Status({ mark }: { mark: StatusMark }) {
  return (
    <Glyph size={14} grid={14} className="si">
      {STATUS[mark]}
    </Glyph>
  );
}

// The priority marks of the mocks. A card's priority runs from 0 (the most
// urgent) to 4 (state.md, Card): urgent, high, medium, low, lowest.
const BARS = [
  { x: 1, y: 7, h: 4 },
  { x: 4.8, y: 4.5, h: 6.5 },
  { x: 8.6, y: 2, h: 9 },
];

function bars(filled: number) {
  return BARS.map((bar, index) => (
    <rect
      key={bar.x}
      x={bar.x}
      y={bar.y}
      width="2.4"
      height={bar.h}
      rx=".7"
      fill={index < filled ? "var(--fg-2)" : "var(--line-3)"}
    />
  ));
}

export function Priority({ level }: { level: number }) {
  return (
    <Glyph size={12} grid={12} className="icon">
      {level <= 0 ? (
        <>
          <rect x=".5" y=".5" width="11" height="11" rx="2.5" fill="var(--orange)" />
          <path d="M6 3v3.6" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" />
          <circle cx="6" cy="8.7" r=".85" fill="#fff" />
        </>
      ) : level >= 4 ? (
        <path
          d="M1.5 6h2M5 6h2M8.5 6h2"
          stroke="var(--fg-4)"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
      ) : (
        bars(level === 1 ? 3 : level === 2 ? 2 : 1)
      )}
    </Glyph>
  );
}

export const PRIORITY_NAMES = ["Urgent", "High", "Medium", "Low", "Lowest"] as const;
