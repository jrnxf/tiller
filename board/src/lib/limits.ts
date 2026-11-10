// The longest text the board API takes for each field (docs/spec/protocol.md
// 7.4, "Length caps"). The daemon counts characters and answers longer text with
// 400 `usage`, so every board input that sends one of these fields checks the
// limit first and says so before Send. The ask note has its own cap in the ask
// dialog.
export const LIMITS = {
  cardTitle: 1000,
  cardBody: 100_000,
  comment: 20_000,
  answer: 20_000,
} as const;

// Characters as the daemon counts them (code points, not UTF-16 units).
export function charCount(text: string): number {
  let count = 0;
  for (const _ of text) count += 1;
  return count;
}

export function isTooLong(text: string, limit: number): boolean {
  // Code points are never more than UTF-16 units, so text this short fits.
  return text.length > limit && charCount(text) > limit;
}
