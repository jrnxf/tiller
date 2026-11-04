import type { ReactNode } from "react";

// The inbox's icons from the approved mocks (docs/design/mocks.html).
function Glyph({ children, box = 16 }: { children: ReactNode; box?: number }) {
  return (
    <svg
      className="icon"
      width="14"
      height="14"
      viewBox={`0 0 ${box} ${box}`}
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

const stroke = { fill: "none", stroke: "currentColor", strokeWidth: 1.3 } as const;

export const LockIcon = () => (
  <Glyph>
    <rect x="3.5" y="7" width="9" height="6.5" rx="1.5" {...stroke} />
    <path d="M5.5 7V5.2a2.5 2.5 0 0 1 5 0V7" {...stroke} />
  </Glyph>
);

export const SendIcon = () => (
  <Glyph>
    <path d="M2.5 8 13.5 2.5 9 13.5 7.5 9z" strokeLinejoin="round" {...stroke} />
  </Glyph>
);

export const PaneIcon = () => (
  <Glyph>
    <rect x="2" y="3" width="12" height="10" rx="1.5" {...stroke} />
    <path
      d="M4.8 6.5 6.8 8l-2 1.5M8 10h3"
      strokeLinecap="round"
      strokeLinejoin="round"
      {...stroke}
    />
  </Glyph>
);

export const InfoIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="5.8" {...stroke} />
    <path d="M8 7.3v3.4" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" />
    <circle cx="8" cy="5.2" r=".8" fill="currentColor" />
  </Glyph>
);

export const FileIcon = () => (
  <Glyph>
    <path d="M4 2.5h5.5L12 5v8.5H4z M9.5 2.5V5H12" strokeLinejoin="round" {...stroke} />
  </Glyph>
);

export const GitIcon = () => (
  <Glyph>
    <circle cx="8" cy="8" r="2.3" {...stroke} />
    <path d="M2 8h3.7M10.3 8H14" stroke="currentColor" strokeWidth={1.3} />
  </Glyph>
);

export const BackIcon = () => (
  <Glyph box={12}>
    <path
      d="M7.5 3 4.5 6 7.5 9"
      strokeLinecap="round"
      strokeLinejoin="round"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
    />
  </Glyph>
);

export const NeedsIcon = () => (
  <svg className="icon si" width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <circle cx="7" cy="7" r="6.75" fill="var(--orange)" />
    <path d="M7 3.6v4.1" stroke="var(--bg-card)" strokeWidth="1.6" strokeLinecap="round" />
    <circle cx="7" cy="10.1" r=".95" fill="var(--bg-card)" />
  </svg>
);

export const OkIcon = () => (
  <svg className="icon si" width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <circle cx="7" cy="7" r="6" fill="none" stroke="var(--green)" strokeWidth="1.5" />
    <path
      d="M4.4 7.2 6.2 9 9.6 5.3"
      fill="none"
      stroke="var(--green)"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

export const RetiredIcon = () => (
  <svg className="icon si" width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <circle cx="7" cy="7" r="6" fill="none" stroke="var(--fg-4)" strokeWidth="1.5" />
    <path d="M4.5 7h5" stroke="var(--fg-4)" strokeWidth="1.5" strokeLinecap="round" />
  </svg>
);

export const ChevronIcon = () => (
  <Glyph box={12}>
    <path
      d="M4.5 2.5 8 6l-3.5 3.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
    />
  </Glyph>
);
