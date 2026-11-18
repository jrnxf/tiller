// The small part of Markdown a chat message uses, parsed to plain data the
// view renders as React elements: no HTML is ever read from the text.
// Paragraphs, lists, code, quotes and short headings; inside them, code,
// bold, italic and links.

export type Block =
  | { kind: "p"; text: string }
  | { kind: "h"; text: string }
  | { kind: "code"; text: string }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "list"; ordered: boolean; start: number; items: Block[][] };

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "br" }
  | { kind: "code"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "em"; children: Inline[] }
  | { kind: "link"; href: string; children: Inline[] };

const FENCE = /^[ \t]*```/;
const HEADING = /^[ \t]*#{1,6}[ \t]+(.*)$/;
const QUOTE = /^[ \t]*>[ \t]?(.*)$/;
const MARKER = /^([ \t]*)(?:([-*+])|(\d{1,3})[.)])[ \t]+(.*)$/;

interface Marker {
  indent: number;
  // Where the item's own text starts, so its later lines can be dedented.
  content: number;
  ordered: boolean;
  number: number;
  text: string;
}

const indentOf = (line: string) => line.length - line.trimStart().length;

function markerOf(line: string): Marker | null {
  const match = MARKER.exec(line);
  if (!match) return null;
  const [whole, lead = "", bullet, number, text = ""] = match;
  return {
    indent: lead.length,
    content: whole.length - text.length,
    ordered: bullet === undefined,
    number: Number(number ?? 1),
    text,
  };
}

const startsBlock = (line: string) =>
  FENCE.test(line) || HEADING.test(line) || QUOTE.test(line) || markerOf(line) !== null;

function dedent(line: string, by: number): string {
  return line.slice(Math.min(indentOf(line), by));
}

// "Three options: 1) wait 2) retry 3) skip", which a model writes on one
// line, is a list: the intro, then each numbered step. Only a run that counts
// up from 1 with at least two steps is one.
const STEP = /(^|\s)(\d{1,2})[.)]\s+/g;

function inlineSteps(text: string): Block[] | null {
  const marks = [...text.matchAll(STEP)].map((match) => ({
    number: Number(match[2]),
    at: (match.index ?? 0) + (match[1]?.length ?? 0),
    end: (match.index ?? 0) + match[0].length,
  }));
  const first = marks.findIndex((mark) => mark.number === 1);
  if (first === -1) return null;
  const run = [marks[first] as (typeof marks)[number]];
  for (const mark of marks.slice(first + 1)) {
    if (mark.number === run.length + 1) run.push(mark);
  }
  if (run.length < 2) return null;
  const items = run.map((mark, index) => {
    const next = run[index + 1];
    return text.slice(mark.end, next ? next.at : undefined).trim();
  });
  if (items.some((item) => item === "")) return null;
  const intro = text.slice(0, run[0]?.at).trim();
  const list: Block = {
    kind: "list",
    ordered: true,
    start: 1,
    items: items.map((item) => [{ kind: "p", text: item.replace(/[,;]$/, "") }]),
  };
  return intro ? [{ kind: "p", text: intro }, list] : [list];
}

export function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let index = 0;
  const at = (offset = 0) => lines[index + offset];
  while (index < lines.length) {
    const line = at() ?? "";
    if (line.trim() === "") {
      index += 1;
      continue;
    }
    if (FENCE.test(line)) {
      const body: string[] = [];
      index += 1;
      while (index < lines.length && !FENCE.test(at() ?? "")) {
        body.push(at() ?? "");
        index += 1;
      }
      index += 1;
      blocks.push({ kind: "code", text: body.join("\n") });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      blocks.push({ kind: "h", text: heading[1] ?? "" });
      index += 1;
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (index < lines.length && QUOTE.test(at() ?? "")) {
        body.push(QUOTE.exec(at() ?? "")?.[1] ?? "");
        index += 1;
      }
      blocks.push({ kind: "quote", blocks: parseBlocks(body.join("\n")) });
      continue;
    }
    const marker = markerOf(line);
    if (marker) {
      const items: string[][] = [];
      let content = marker.content;
      while (index < lines.length) {
        const current = at() ?? "";
        const next = markerOf(current);
        if (next && next.indent === marker.indent && next.ordered === marker.ordered) {
          items.push([next.text]);
          content = next.content;
          index += 1;
          continue;
        }
        if (current.trim() === "") {
          // A blank line ends the list, unless the item goes on after it.
          const after = at(1);
          const goesOn =
            after !== undefined &&
            after.trim() !== "" &&
            (indentOf(after) > marker.indent || markerOf(after)?.ordered === marker.ordered) &&
            indentOf(after) >= marker.indent;
          if (!goesOn) break;
          items.at(-1)?.push("");
          index += 1;
          continue;
        }
        if (indentOf(current) > marker.indent || !startsBlock(current)) {
          items.at(-1)?.push(dedent(current, content));
          index += 1;
          continue;
        }
        break;
      }
      blocks.push({
        kind: "list",
        ordered: marker.ordered,
        start: marker.number,
        items: items.map((item) => parseBlocks(item.join("\n"))),
      });
      continue;
    }
    const body: string[] = [];
    while (index < lines.length && (at() ?? "").trim() !== "" && !startsBlock(at() ?? "")) {
      body.push((at() ?? "").trim());
      index += 1;
    }
    const text = body.join("\n");
    blocks.push(...((body.length === 1 ? inlineSteps(text) : null) ?? [{ kind: "p", text }]));
  }
  return blocks;
}

// Code first, so nothing inside it is read as Markdown; then links, bold and
// italic. An address alone is a link too. Only web addresses link.
const INLINE =
  /(`+)([\s\S]*?[^`])\1(?!`)|\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)|\*\*(?=\S)([\s\S]*?\S)\*\*|__(?=\S)([\s\S]*?\S)__|(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])|(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])|(https?:\/\/[^\s<>]*[^\s<>.,;:!?)\]'"])/g;

function textParts(value: string): Inline[] {
  const parts: Inline[] = [];
  value.split("\n").forEach((line, index) => {
    if (index > 0) parts.push({ kind: "br" });
    if (line) parts.push({ kind: "text", text: line });
  });
  return parts;
}

export function parseInline(source: string): Inline[] {
  const parts: Inline[] = [];
  let last = 0;
  for (const match of source.matchAll(INLINE)) {
    const start = match.index ?? 0;
    parts.push(...textParts(source.slice(last, start)));
    const [whole, , code, label, href, strong, strongToo, em, emToo, bare] = match;
    if (code !== undefined) {
      parts.push({
        kind: "code",
        text: code.trim() === "" ? code : code.replace(/^ (.*) $/, "$1"),
      });
    } else if (label !== undefined && href !== undefined) {
      parts.push({ kind: "link", href, children: parseInline(label) });
    } else if (strong !== undefined || strongToo !== undefined) {
      parts.push({ kind: "strong", children: parseInline(strong ?? strongToo ?? "") });
    } else if (em !== undefined || emToo !== undefined) {
      parts.push({ kind: "em", children: parseInline(em ?? emToo ?? "") });
    } else if (bare !== undefined) {
      parts.push({ kind: "link", href: bare, children: [{ kind: "text", text: bare }] });
    }
    last = start + whole.length;
  }
  parts.push(...textParts(source.slice(last)));
  return parts;
}

function inlineText(parts: Inline[]): string {
  return parts
    .map((part) => {
      switch (part.kind) {
        case "text":
        case "code":
          return part.text;
        case "br":
          return " ";
        default:
          return inlineText(part.children);
      }
    })
    .join("");
}

function blockText(blocks: Block[]): string[] {
  return blocks.flatMap((value) => {
    switch (value.kind) {
      case "p":
      case "h":
        return [inlineText(parseInline(value.text))];
      case "code":
        return [value.text];
      case "quote":
        return blockText(value.blocks);
      case "list":
        // Each item keeps its mark, so the steps still read as steps.
        return value.items.map((item, at) =>
          [value.ordered ? `${value.start + at}.` : "•", ...blockText(item)].join(" "),
        );
    }
  });
}

// A message's words on one line with its Markdown taken out, as a preview
// shows them: "Run `nix build` **now**" reads "Run nix build now", and a
// list keeps its numbers.
export function plainText(source: string): string {
  return blockText(parseBlocks(source)).join(" ").replace(/\s+/g, " ").trim();
}
