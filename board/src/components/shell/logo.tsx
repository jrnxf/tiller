// The Tiller lockup: the Unfurl leaf in the brand moss green (`--logo`) and the
// wordmark in the text color. Paths are copied from assets/brand, not redrawn.
export function Logo() {
  return (
    <svg
      className="logo"
      viewBox="0 0 823 256"
      preserveAspectRatio="xMinYMid slice"
      role="img"
      aria-label="tiller"
      xmlns="http://www.w3.org/2000/svg"
    >
      <g fill="var(--logo)">
        <path d="M32 224V128A96 96 0 0 1 128 32H224C224 84 183 112 136 128C89 144 64 176 64 224Z" />
        <path d="M88 224C88 189 112 167 148 154C184 141 209 123 224 104V128A96 96 0 0 1 128 224Z" />
      </g>
      <g fill="currentColor" transform="translate(280 0)">
        <g transform="translate(16 40)">
          <path d="M28 16H56V60H84V84H56V130A18 22 0 0 0 74 152H86V176H74A46 46 0 0 1 28 130V84H0V60H28Z" />
        </g>
        <g transform="translate(108 40)">
          <path d="M8 60H36V176H8ZM8 12H36V40H8Z" />
        </g>
        <g transform="translate(160 40)">
          <path d="M8 0H36V134A14 18 0 0 0 50 152H56V176H50A42 42 0 0 1 8 134Z" />
        </g>
        <g transform="translate(226 40)">
          <path d="M8 0H36V134A14 18 0 0 0 50 152H56V176H50A42 42 0 0 1 8 134Z" />
        </g>
        <g transform="translate(294 40)">
          <path
            fillRule="evenodd"
            d="M120 122H30C34 143 48 155 68 155C82 155 94 150 103 141L120 158C107 172 83 180 61 180A59 62 0 0 1 2 118A59 62 0 0 1 61 56A59 62 0 0 1 120 118V122ZM32 101H90C85 86 75 78 61 78C47 78 37 86 32 101Z"
          />
        </g>
        <g transform="translate(424 40)">
          <path d="M8 60H36V78C45 64 58 56 75 56H87V85H75C53 85 36 99 36 121V176H8Z" />
        </g>
      </g>
    </svg>
  );
}
