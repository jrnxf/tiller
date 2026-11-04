// A pane tail is raw terminal output. It shows as styled text: 24-bit
// foreground colour, bold, dim and italic are kept; backgrounds are dropped so
// the block stays legible in both themes.

export interface AnsiSegment {
  text: string;
  // "r,g,b", or null for the block's own colour.
  fg: string | null;
  bold: boolean;
  dim: boolean;
  italic: boolean;
}

interface Style {
  fg: string | null;
  bold: boolean;
  dim: boolean;
  italic: boolean;
}

const PLAIN: Style = { fg: null, bold: false, dim: false, italic: false };
const ESC = String.fromCharCode(27);
const SGR = new RegExp(`${ESC}\\[([0-9;]*)m`, "g");
// Every other control sequence (cursor moves, erase, modes) carries no text.
const OTHER = new RegExp(`${ESC}\\[[0-9;?]*[A-Za-ln-z]`, "g");

function channel(value: number | undefined): number | null {
  return value !== undefined && Number.isInteger(value) && value >= 0 && value <= 255
    ? value
    : null;
}

function apply(style: Style, params: number[]): Style {
  let next = { ...style };
  for (let index = 0; index < params.length; index += 1) {
    const code = params[index];
    if (code === 0 || Number.isNaN(code)) next = { ...PLAIN };
    else if (code === 1) next.bold = true;
    else if (code === 2) next.dim = true;
    else if (code === 3) next.italic = true;
    else if (code === 22) {
      next.bold = false;
      next.dim = false;
    } else if (code === 23) next.italic = false;
    else if (code === 39) next.fg = null;
    else if (code === 38 || code === 48) {
      const kind = params[index + 1];
      if (kind === 2) {
        const [r, g, b] = [2, 3, 4].map((offset) => channel(params[index + offset]));
        // A background is read and dropped; a foreground needs all three.
        if (code === 38) next.fg = r !== null && g !== null && b !== null ? `${r},${g},${b}` : null;
        index += 4;
      } else if (kind === 5) {
        // The 256-colour palette is not carried: skip its index.
        index += 2;
      }
    }
  }
  return next;
}

export function stripAnsi(text: string): string {
  return text.replace(SGR, "").replace(OTHER, "");
}

export function parseAnsi(input: string): AnsiSegment[] {
  const text = input.replace(OTHER, "");
  const segments: AnsiSegment[] = [];
  let style = PLAIN;
  let last = 0;
  const push = (chunk: string) => {
    if (chunk) segments.push({ text: chunk, ...style });
  };
  for (const match of text.matchAll(SGR)) {
    const at = match.index ?? 0;
    push(text.slice(last, at));
    last = at + match[0].length;
    const params = match[1] ?? "";
    style = apply(style, params === "" ? [0] : params.split(";").map(Number));
  }
  push(text.slice(last));
  return segments;
}

// The first line of 20 or more box-drawing dashes is the top of the agent's
// input box. It and everything below it (the draft, the status bar) is chrome,
// not the last reply.
export function trimChrome(text: string): string {
  const lines = text.split("\n");
  const cut = lines.findIndex((line) => /^\s*─{20,}/.test(stripAnsi(line)));
  return (cut > 0 ? lines.slice(0, cut) : lines).join("\n");
}

export function tidy(text: string): string {
  return trimChrome(text.replace(/\r/g, ""))
    .split("\n")
    .map((line) => line.replace(/\s+$/, ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function lineCount(tidied: string): number {
  return tidied === "" ? 0 : stripAnsi(tidied).split("\n").length;
}
