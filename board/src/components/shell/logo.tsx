// The Tiller lockup: the Unfurl leaf in the brand moss green (`--logo`) and the
// wordmark in the text color. Paths are copied from assets/brand, not redrawn.
export function Logo() {
  return (
    <svg
      className="logo"
      viewBox="0 0 829 256"
      preserveAspectRatio="xMinYMid slice"
      role="img"
      aria-label="tiller"
      xmlns="http://www.w3.org/2000/svg"
    >
      <g fill="var(--logo)">
        <path d="M32 224V128A96 96 0 0 1 128 32H224A116.063291139 116.063291139 0 0 1 144.08 114.56A114 114 0 0 0 62 224Z" />
        <path d="M90 224A86 86 0 0 1 151.92 141.44A144.063291139 144.063291139 0 0 0 224 93.23079251V128A96 96 0 0 1 128 224Z" />
      </g>
      <g fill="currentColor" transform="translate(280 0)">
        <g transform="translate(16 40)">
          <path d="M28 16H56V60H84V88H56V130A18 18 0 0 0 74 148H86V176H74A46 46 0 0 1 28 130V88H0V60H28Z" />
        </g>
        <g transform="translate(108 40)">
          <path d="M8 60H36V176H8ZM8 12H36V40H8Z" />
        </g>
        <g transform="translate(160 40)">
          <path d="M8 0H36V134A14 14 0 0 0 50 148H56V176H50A42 42 0 0 1 8 134Z" />
        </g>
        <g transform="translate(226 40)">
          <path d="M8 0H36V134A14 14 0 0 0 50 148H56V176H50A42 42 0 0 1 8 134Z" />
        </g>
        <g transform="translate(294 40)">
          <path
            fillRule="evenodd"
            d="M108.896551724 160.75862069A62 62 0 0 1 2 118A62 62 0 0 1 64 56A62 62 0 0 1 124.398675482 132H33.01613323A34 34 0 0 0 88.620689655 141.448275862ZM33.01613323 104A34 34 0 0 1 94.98386677 104Z"
          />
        </g>
        <g transform="translate(430 40)">
          <path d="M8 60H36V72.553443215A75 75 0 0 1 83 56H87V84H83A47 47 0 0 0 36 131V176H8Z" />
        </g>
      </g>
    </svg>
  );
}
