export interface Command {
  id: string;
  group: string;
  label: string;
  // Extra words that find the command but are not shown.
  keywords?: string;
  run: () => void;
}

// Subsequence match: every query character appears in order. A match that
// starts a word or runs without gaps ranks higher. Returns null for no match.
export function scoreMatch(text: string, query: string): number | null {
  const haystack = text.toLowerCase();
  const needle = query.toLowerCase().trim();
  if (needle === "") return 0;

  let score = 0;
  let position = 0;
  let previous = -2;
  for (const char of needle) {
    const found = haystack.indexOf(char, position);
    if (found === -1) return null;
    if (found === previous + 1) score += 3;
    if (found === 0 || haystack[found - 1] === " ") score += 5;
    score -= found - position;
    previous = found;
    position = found + 1;
  }
  return score;
}

export function rankCommands(commands: Command[], query: string): Command[] {
  if (query.trim() === "") return commands;
  const ranked: { command: Command; score: number }[] = [];
  for (const command of commands) {
    const score = scoreMatch(`${command.label} ${command.keywords ?? ""}`, query);
    if (score !== null) ranked.push({ command, score });
  }
  // Array.prototype.sort is stable, so equal scores keep the listed order.
  ranked.sort((a, b) => b.score - a.score);
  return ranked.map((entry) => entry.command);
}
