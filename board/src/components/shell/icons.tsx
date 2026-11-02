import type { ReactNode } from "react";

function Icon({ children }: { children: ReactNode }) {
  return (
    <svg
      className="icon"
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export const EyeIcon = () => (
  <Icon>
    <path d="M1.5 8s2.3-4.5 6.5-4.5S14.5 8 14.5 8 12.2 12.5 8 12.5 1.5 8 1.5 8Z" />
    <circle cx="8" cy="8" r="1.8" />
  </Icon>
);

export const EyeOffIcon = () => (
  <Icon>
    <path d="M2 2l12 12M6.6 3.7A6.4 6.4 0 0 1 8 3.5c4.2 0 6.5 4.5 6.5 4.5a11 11 0 0 1-2 2.5M4 5.2A11 11 0 0 0 1.5 8S3.8 12.5 8 12.5a6 6 0 0 0 2.4-.5" />
  </Icon>
);

export const MenuIcon = () => (
  <Icon>
    <path d="M2 4h12M2 8h12M2 12h12" />
  </Icon>
);

export const CloseIcon = () => (
  <Icon>
    <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />
  </Icon>
);

export const SearchIcon = () => (
  <Icon>
    <circle cx="7" cy="7" r="4.5" />
    <path d="M10.5 10.5L14 14" />
  </Icon>
);
