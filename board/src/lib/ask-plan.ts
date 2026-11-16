// What the Ask dialog sends. The owner types one sentence with @project chips;
// each mentioned project gets its own ask. The chips are the address, so they
// are not part of what the orchestrator reads.

export type AskPiece =
  | { kind: "text"; text: string }
  | { kind: "mention"; key: string; name: string };

// The projects the text mentions, once each, in the order they first appear.
export function mentionedKeys(pieces: AskPiece[]): string[] {
  const keys: string[] = [];
  for (const piece of pieces) {
    if (piece.kind === "mention" && !keys.includes(piece.key)) keys.push(piece.key);
  }
  return keys;
}

// The words an orchestrator reads. Chips and spaces at the start are dropped,
// so "@web-shop @docs-site optimize fonts" reads "optimize fonts". A chip later
// in the sentence stays as the plain project name so the sentence still reads.
export function messageFor(pieces: AskPiece[]): string {
  let leading = true;
  let body = "";
  for (const piece of pieces) {
    if (piece.kind === "mention") {
      if (leading) continue;
      body += piece.name;
      continue;
    }
    const text = leading ? piece.text.replace(/^\s+/, "") : piece.text;
    if (text === "") continue;
    body += text;
    leading = false;
  }
  return body.replace(/[ \t]{2,}/g, " ").trim();
}

// The footer that tells an orchestrator who else got the same ask. Kept apart
// from `messageFor` so the wording is one place to change.
export function footerFor(others: string[]): string | null {
  return others.length > 0 ? `Also asked of: ${others.join(", ")}` : null;
}

// The ask one project receives: the message, then the footer naming the other
// projects it went to.
export function askFor(message: string, others: string[]): string {
  const footer = footerFor(others);
  return footer ? `${message}\n\n${footer}` : message;
}

// A paired host keeps the text of an ask and its answer (protocol.md section
// 10), so this board cannot show the answer of a project that host owns. The
// owner is told so before the send and after it, never left to wait. `owner`
// is the paired host's label, or undefined for this host's own project.
export function answersElsewhere(projects: { name: string; owner?: string }[]): string | null {
  const peers = projects.filter((project) => project.owner !== undefined);
  if (peers.length === 0) return null;
  const hosts = [...new Set(peers.map((project) => project.owner))].join(" and ");
  if (projects.length === 1) return `The answer shows on the board of ${hosts}, not in this Inbox.`;
  const names = peers.map((project) => project.name).join(", ");
  return `The answer from ${names} shows on the board of ${hosts}, not in this Inbox.`;
}
