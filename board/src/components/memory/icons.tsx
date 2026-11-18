import type { ReactNode } from "react";

// An icon from the approved mocks (docs/design/mocks.html), shared with
// Health and the project delete dialog.
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

export const ClockIcon = () => (
  <Glyph>
    <path d="M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 5v3l2 1.5" {...stroke} />
  </Glyph>
);
