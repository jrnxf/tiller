import type { ReactNode } from "react";

// The icon set from the approved mocks (docs/design/mocks.html), one 16 px
// grid, drawn with the current text color.
function Icon({ children, size = 16 }: { children: ReactNode; size?: number }) {
  return (
    <svg
      className="icon"
      width={size}
      height={size}
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
} as const;

export const BoardIcon = () => (
  <Icon>
    <rect x="2" y="2.5" width="3.5" height="11" rx="1" {...stroke} />
    <rect x="6.25" y="2.5" width="3.5" height="7" rx="1" {...stroke} />
    <rect x="10.5" y="2.5" width="3.5" height="9" rx="1" {...stroke} />
  </Icon>
);

export const AgentsIcon = () => (
  <Icon>
    <circle cx="4" cy="3.5" r="1.6" {...stroke} />
    <circle cx="10.5" cy="8" r="1.6" {...stroke} />
    <circle cx="10.5" cy="12.5" r="1.6" {...stroke} />
    <path d="M4 5.2v5.3a2 2 0 0 0 2 2h2.9M4 8h4.9" strokeLinecap="round" {...stroke} />
  </Icon>
);

export const InboxIcon = () => (
  <Icon>
    <path
      d="M2 9.5h3l1 2h4l1-2h3M3.2 3.5h9.6l1.7 6v3a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-3z"
      strokeLinejoin="round"
      {...stroke}
    />
  </Icon>
);

export const MemoryIcon = () => (
  <Icon>
    <path
      d="M8 2.5c-2.6 0-4.5 1.6-4.5 3.8 0 1.3.6 2.2 1.5 2.9V12a1.5 1.5 0 0 0 1.5 1.5h3A1.5 1.5 0 0 0 11 12V9.2c.9-.7 1.5-1.6 1.5-2.9 0-2.2-1.9-3.8-4.5-3.8zM6 11h4"
      strokeLinejoin="round"
      {...stroke}
    />
  </Icon>
);

export const PulseIcon = () => (
  <Icon>
    <path
      d="M1.5 8.5h3l1.8-4.5 3.2 8 1.8-3.5h3.2"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </Icon>
);

export const LayersIcon = () => (
  <Icon>
    <path
      d="M8 2.5 14 6 8 9.5 2 6zM2 9.3l6 3.5 6-3.5"
      strokeLinejoin="round"
      strokeLinecap="round"
      {...stroke}
    />
  </Icon>
);

export const FocusIcon = () => (
  <Icon>
    <circle cx="8" cy="8" r="5.2" {...stroke} />
    <circle cx="8" cy="8" r="1.7" fill="currentColor" />
  </Icon>
);

export const EyeIcon = () => (
  <Icon>
    <path
      d="M1.5 8s2.3-4.5 6.5-4.5S14.5 8 14.5 8s-2.3 4.5-6.5 4.5S1.5 8 1.5 8z"
      strokeLinejoin="round"
      {...stroke}
    />
    <circle cx="8" cy="8" r="2" {...stroke} />
  </Icon>
);

export const EyeOffIcon = () => (
  <Icon>
    <path
      d="M1.5 8s2.3-4.5 6.5-4.5S14.5 8 14.5 8s-2.3 4.5-6.5 4.5S1.5 8 1.5 8z"
      strokeLinejoin="round"
      opacity={0.55}
      {...stroke}
    />
    <path d="M3 13 13 3" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" />
  </Icon>
);

export const SearchIcon = () => (
  <Icon>
    <circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" strokeWidth={1.4} />
    <path d="M10.5 10.5 13.5 13.5" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" />
  </Icon>
);

export const MenuIcon = () => (
  <Icon>
    <path
      d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
    />
  </Icon>
);

export const CloseIcon = () => (
  <svg
    className="icon"
    width="12"
    height="12"
    viewBox="0 0 16 16"
    aria-hidden="true"
    focusable="false"
  >
    <path
      d="M4.5 4.5l7 7M11.5 4.5l-7 7"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
    />
  </svg>
);

export const CheckIcon = () => (
  <Icon>
    <path
      d="M3.5 8.5 6.5 11.5 12.5 4.5"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </Icon>
);

export const ServerIcon = () => (
  <svg
    className="icon"
    width="12"
    height="12"
    viewBox="0 0 16 16"
    aria-hidden="true"
    focusable="false"
  >
    <rect x="2.5" y="3" width="11" height="4" rx="1" {...stroke} />
    <rect x="2.5" y="9" width="11" height="4" rx="1" {...stroke} />
    <path d="M5 5h.01M5 11h.01" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" />
  </svg>
);

export const ChevronIcon = () => (
  <svg
    className="icon"
    width="14"
    height="14"
    viewBox="0 0 12 12"
    aria-hidden="true"
    focusable="false"
  >
    <path
      d="M4.5 3 7.5 6 4.5 9"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

export const RefreshIcon = () => (
  <svg className="icon bic spin" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M13 8a5 5 0 1 1-1.6-3.7M13 2.5v2.8h-2.8"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);
