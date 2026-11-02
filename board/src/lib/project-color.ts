// Each project gets one of nine hues from the design tokens (`--p-1` to
// `--p-9` in styles.css), in the order the projects are listed.
const HUES = 9;

export function projectColor(index: number): string {
  return `var(--p-${(index % HUES) + 1})`;
}
