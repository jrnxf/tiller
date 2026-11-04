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

export const RevertIcon = () => (
  <Glyph>
    <path d="M5.5 4 2.5 7l3 3M3 7h6.5a3.5 3.5 0 0 1 0 7H7" {...stroke} />
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

export const CrossIcon = () => (
  <Glyph>
    <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" {...stroke} strokeWidth={1.4} />
  </Glyph>
);

export const BackIcon = () => (
  <Glyph>
    <path d="M9.5 4 5.5 8l4 4" {...stroke} strokeWidth={1.4} />
  </Glyph>
);
