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
