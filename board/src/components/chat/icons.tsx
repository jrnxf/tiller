import type { ReactNode } from "react";

// Icons the chat needs that the board has none of, on the board's 16 px grid
// and stroke.
function Glyph({ children, className = "icon" }: { children: ReactNode; className?: string }) {
  return (
    <svg
      className={className}
      width={16}
      height={16}
      viewBox="0 0 16 16"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.3,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

export const ChatIcon = () => (
  <Glyph>
    <path
      d="M3 3.5h10a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H7.5L4.5 14v-2.5H3a1 1 0 0 1-1-1v-6a1 1 0 0 1 1-1Z"
      {...stroke}
    />
  </Glyph>
);

export const CopyIcon = () => (
  <Glyph>
    <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" {...stroke} />
    <path
      d="M3.5 10.5h-.2A1.3 1.3 0 0 1 2 9.2V3.3A1.3 1.3 0 0 1 3.3 2h5.9a1.3 1.3 0 0 1 1.3 1.3v.2"
      {...stroke}
    />
  </Glyph>
);

export const ReplyToIcon = () => (
  <Glyph>
    <path d="M6 11 2.5 7.5 6 4" {...stroke} />
    <path d="M13.5 12.5v-1.5a3.5 3.5 0 0 0-3.5-3.5H2.5" {...stroke} />
  </Glyph>
);

export const StepsIcon = () => (
  <Glyph>
    <path d="M2.5 4h.01M2.5 8h.01M2.5 12h.01M5.5 4h8M5.5 8h8M5.5 12h8" {...stroke} />
  </Glyph>
);

export const HashIcon = () => (
  <Glyph>
    <path d="M3 6h10.5M2.5 10H13M6.8 2.5 5.3 13.5M10.7 2.5 9.2 13.5" {...stroke} />
  </Glyph>
);

// A tick, then two: delivered, then read by the orchestrator.
export const TickIcon = () => (
  <Glyph>
    <path d="m3 8.5 3 3 7-7" {...stroke} />
  </Glyph>
);

export const TicksIcon = () => (
  <Glyph>
    <path d="m1.5 8.5 3 3 7-7M9 11l.5.5 5-5" {...stroke} />
  </Glyph>
);

export const RestartIcon = () => (
  <Glyph>
    <path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9L2.5 5.6" {...stroke} />
    <path d="M2.5 2.5v3.1h3.1" {...stroke} />
  </Glyph>
);

export const DoneIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="6" {...stroke} />
    <path d="m5.6 8 1.6 1.6 3.2-3.2" {...stroke} />
  </Glyph>
);

export const FailedIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="6" {...stroke} />
    <path d="M6 6l4 4M10 6l-4 4" {...stroke} />
  </Glyph>
);

export const DotIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="2" fill="currentColor" />
  </Glyph>
);
