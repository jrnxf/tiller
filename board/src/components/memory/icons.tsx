import type { ReactNode } from "react";

// The memory page's icons from the approved mocks (docs/design/mocks.html).
// Their size comes from memory.css.
function Glyph({ children }: { children: ReactNode }) {
  return (
    <svg
      className="icon"
      width="14"
      height="14"
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

export const PlusIcon = () => (
  <Glyph>
    <path d="M8 3.5v9M3.5 8h9" {...stroke} strokeWidth={1.4} />
  </Glyph>
);

export const EditIcon = () => (
  <Glyph>
    <path d="M10.8 2.7a1.4 1.4 0 0 1 2 2L6.5 11l-2.7 .7.7-2.7z" {...stroke} />
  </Glyph>
);

export const TrashIcon = () => (
  <Glyph>
    <path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5" {...stroke} />
  </Glyph>
);

export const LinesIcon = () => (
  <Glyph>
    <path d="M3 4.5h10M3 8h10M3 11.5h6" {...stroke} />
  </Glyph>
);

export const SparkIcon = () => (
  <Glyph>
    <path
      d="M8 2v3M8 11v3M2 8h3M11 8h3M4 4l2 2M10 10l2 2M12 4l-2 2M6 10l-2 2"
      {...stroke}
      strokeWidth={1.2}
    />
  </Glyph>
);

export const ClockIcon = () => (
  <Glyph>
    <path d="M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 5v3l2 1.5" {...stroke} />
  </Glyph>
);

export const PlayIcon = () => (
  <Glyph>
    <path d="M5 3.5v9l7-4.5z" {...stroke} />
  </Glyph>
);
