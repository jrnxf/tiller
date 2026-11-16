export interface Command {
  id: string;
  group: string;
  label: string;
  // Extra words that find the command but are not shown.
  keywords?: string;
  // The keys that run it from anywhere, one by one, such as G then I.
  keys?: string[];
  // A few words at the right edge that say what the row is, such as a card's
  // project and column.
  hint?: string;
  // A thing to find, such as a card, not a command: it is listed only for a
  // query, and only when every word of the query is in its text as typed.
  searchOnly?: boolean;
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

// Whether every word of the query is in the text, in any order.
function hasEveryWord(text: string, query: string): boolean {
  const haystack = text.toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .every((word) => haystack.includes(word));
}

export function rankCommands<T extends Command>(commands: T[], query: string): T[] {
  if (query.trim() === "") return commands.filter((command) => !command.searchOnly);
  const ranked: { command: T; score: number }[] = [];
  for (const command of commands) {
    const text = `${command.label} ${command.keywords ?? ""}`;
    if (command.searchOnly && !hasEveryWord(text, query.trim())) continue;
    const score = scoreMatch(text, query);
    if (score !== null) ranked.push({ command, score });
    // The words are all there, but not in the order typed.
    else if (command.searchOnly) ranked.push({ command, score: -text.length });
  }
  // Array.prototype.sort is stable, so equal scores keep the listed order.
  ranked.sort((a, b) => b.score - a.score);
  // A group stays in one piece, under one heading: the groups go by their
  // best match, and each keeps its own order by score.
  const groups = new Map<string, T[]>();
  for (const { command } of ranked) {
    const group = groups.get(command.group);
    if (group) group.push(command);
    else groups.set(command.group, [command]);
  }
  return [...groups.values()].flat();
}
