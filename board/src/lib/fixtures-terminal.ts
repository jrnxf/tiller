// A stand-in herdr pane for the fixture server: the screen an agent might
// have up, as the bytes a terminal would get. The live terminal draws it the
// way it draws a real pane, so its looks can be checked without herdr. A Fit
// draws the same screen at the size asked for, as an agent would redraw.

export const FIXTURE_COLS = 100;
export const FIXTURE_ROWS = 28;

export interface PaneSize {
  cols: number;
  rows: number;
}

const OWN: PaneSize = { cols: FIXTURE_COLS, rows: FIXTURE_ROWS };

const ESC = "\x1b";
const dim = (text: string) => `${ESC}[2m${text}${ESC}[22m`;
const color = (code: number, text: string) => `${ESC}[${code}m${text}${ESC}[39m`;
const bold = (text: string) => `${ESC}[1m${text}${ESC}[22m`;

// The row the thinking line is on, and the prompt's, 1-based: near the
// bottom, like an agent's input box.
const thinkingRow = (size: PaneSize) => size.rows - 6;
const promptRow = (size: PaneSize) => size.rows - 3;

// A row cut to the pane's width, color codes kept: an agent redraws for the
// size it is given, so a narrow pane (a phone's Fit) never wraps a row. Wrapped
// rows would push the screen up, and a diff that clears one row would leave
// the rest of its line behind.
function clip(text: string, cols: number): string {
  let shown = 0;
  let out = "";
  // oxlint-disable-next-line no-control-regex
  for (const part of text.split(/(\x1b\[[0-9;]*m)/)) {
    if (part.startsWith(`${ESC}[`)) {
      out += part;
      continue;
    }
    const room = cols - shown;
    const chars = [...part];
    if (chars.length <= room) {
      out += part;
      shown += chars.length;
      continue;
    }
    out += chars.slice(0, Math.max(0, room - 1)).join("") + (room > 0 ? "\u2026" : "");
    shown = cols;
  }
  return out;
}

function lines(size: PaneSize): string[] {
  const rule = "─".repeat(size.cols - 4);
  const top = [
    ` ${color(35, "✻")} ${bold("build")} ${dim("on")} beta-7 ${dim("· Rewrite the importer's dry run")}`,
    "",
    ` ${color(32, "●")} Read ${color(36, "src/importer/dry_run.rs")} ${dim("(214 lines)")}`,
    ` ${color(32, "●")} Edit ${color(36, "src/importer/dry_run.rs")}`,
    `   ${dim("│")} ${color(32, "+ fn print_rows(rows: &[Row]) {")}`,
    `   ${dim("│")} ${color(32, "+     for row in rows {")}`,
    `   ${dim("│")} ${color(32, '+         println!("{}", row.summary());')}`,
    `   ${dim("│")} ${color(32, "+     }")}`,
    `   ${dim("│")} ${color(32, "+ }")}`,
    `   ${dim("│")} ${color(31, "- // TODO: print what would be written")}`,
    ` ${color(32, "●")} Bash ${dim("cargo nextest run -p importer")}`,
    `   ${dim("│")} ${color(32, "PASS")} importer::dry_run::prints_each_row`,
    `   ${dim("│")} ${color(32, "PASS")} importer::dry_run::writes_nothing`,
    `   ${dim("│")} ${color(32, "PASS")} importer::dry_run::keeps_the_order`,
    `   ${dim("│")} ${dim("42 tests run: 42 passed, 0 skipped")}`,
    ` ${color(32, "●")} Edit ${color(36, "docs/importer.md")}`,
    `   ${dim("│")} ${color(32, "+ `--dry-run` prints each row it would write, and writes nothing.")}`,
    "",
    ` ${color(33, "●")} Bash ${dim("cargo clippy -p importer -- -D warnings")}`,
    `   ${dim("│")} ${dim("Checking importer v0.4.0")}`,
    "",
  ].slice(0, Math.max(0, size.rows - 7));
  return [
    ...top,
    ...Array.from({ length: size.rows - 7 - top.length }, () => ""),
    thinking(0, size),
    "",
    ` ${dim(`╭${rule}╮`)}`,
    prompt("", size),
    ` ${dim(`╰${rule}╯`)}`,
    `  ${dim("? for shortcuts")}${" ".repeat(Math.max(1, size.cols - 36))}${dim("auto-accept on")}`,
  ].map((row) => clip(row, size.cols));
}

function thinking(seconds: number, size: PaneSize): string {
  return clip(
    ` ${color(35, "✻")} ${color(35, "Checking the lint")} ${dim(`(${seconds}s · esc to interrupt)`)}`,
    size.cols,
  );
}

function prompt(typed: string, size: PaneSize): string {
  const room = size.cols - 6;
  const shown = typed.length >= room ? typed.slice(1 - room) : typed;
  return ` ${dim("│")} ${color(36, ">")} ${shown}${" ".repeat(room - shown.length - 1)}${dim("│")}`;
}

// The cursor sits after what was typed at the prompt.
function cursor(typed: string, size: PaneSize): string {
  const room = size.cols - 6;
  return `${ESC}[${promptRow(size)};${6 + Math.min(typed.length, room - 1)}H`;
}

const encoder = new TextEncoder();

function base64(text: string): string {
  let binary = "";
  for (const byte of encoder.encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

// The first frame of a run: the whole screen, cleared first, the way herdr
// sends one.
export function fullFrame(typed: string, seconds: number, size: PaneSize = OWN): string {
  const rows = lines(size);
  rows[thinkingRow(size) - 1] = thinking(seconds, size);
  rows[promptRow(size) - 1] = prompt(typed, size);
  return base64(
    `${ESC}[?2026h${ESC}[2J${ESC}[1;1H${rows.join("\r\n")}${cursor(typed, size)}${ESC}[?2026l`,
  );
}

// A diff: the thinking line's clock moved.
export function tickFrame(typed: string, seconds: number, size: PaneSize = OWN): string {
  return base64(
    `${ESC}[${thinkingRow(size)};1H${ESC}[2K${thinking(seconds, size)}${cursor(typed, size)}`,
  );
}

// A diff: the prompt line, after a key.
export function promptFrame(typed: string, size: PaneSize = OWN): string {
  return base64(
    `${ESC}[${promptRow(size)};1H${ESC}[2K${prompt(typed, size)}${cursor(typed, size)}`,
  );
}

// The pane's scrollback as herdr's recent read returns it: earlier work
// first, the screen last, rows split by a bare newline.
export function historyText(typed: string, seconds: number, size: PaneSize = OWN): string {
  const earlier: string[] = [];
  for (let step = 1; step <= 24; step += 1) {
    const file = `src/importer/step_${String(step).padStart(2, "0")}.rs`;
    earlier.push(
      ` ${color(32, "●")} Read ${color(36, file)} ${dim(`(${40 + step * 7} lines)`)}`,
      ` ${color(32, "●")} Bash ${dim(`cargo nextest run -p importer step_${step}`)}`,
      `   ${dim("│")} ${color(32, "PASS")} importer::step_${step}::keeps_the_order`,
      `   ${dim("│")} ${dim(`${step + 3} tests run: ${step + 3} passed, 0 skipped`)}`,
      "",
    );
  }
  return `${[...earlier.map((row) => clip(row, size.cols)), ...screenRows(typed, seconds, size)].join("\n")}\n`;
}

// The pane's screen as herdr's visible read returns it, for a tail: rows
// split by a bare newline.
export function screenText(typed: string, seconds: number, size: PaneSize = OWN): string {
  return `${screenRows(typed, seconds, size).join("\n")}\n`;
}

function screenRows(typed: string, seconds: number, size: PaneSize): string[] {
  const screen = lines(size);
  screen[thinkingRow(size) - 1] = thinking(seconds, size);
  screen[promptRow(size) - 1] = prompt(typed, size);
  return screen;
}

// What a key does to the prompt: printable keys type, backspace takes one
// back, and enter sends it (the prompt clears).
export function typeInto(typed: string, input: { keys?: unknown; text?: unknown }): string {
  if (typeof input.text === "string") return typed + input.text.replace(/\s+/g, " ");
  if (!Array.isArray(input.keys)) return typed;
  let next = typed;
  for (const key of input.keys) {
    if (key === "enter" || key === "ctrl+c") next = "";
    else if (key === "backspace") next = next.slice(0, -1);
    else if (key === "space") next += " ";
    else if (typeof key === "string" && [...key].length === 1) next += key;
  }
  return next;
}

// The text a row shows, without its color codes.
// oxlint-disable-next-line no-control-regex
const visible = (text: string) => [...text.replace(/\x1b\[[0-9;]*m/g, "")];

// A menu Claude Code draws that the board has no layout for, as herdr's
// visible read returns it: the work above, then the menu in its box.
export function menuText(size: PaneSize = OWN): string {
  const rule = "─".repeat(size.cols - 4);
  const row = (text: string) =>
    ` ${dim("│")} ${text}${" ".repeat(size.cols - 5 - visible(text).length)}${dim("│")}`;
  return `${[
    ...lines(size).slice(0, size.rows - 9),
    ` ${dim(`╭${rule}╮`)}`,
    row(bold("Select a model for this session:")),
    row(""),
    row(color(36, "❯ 1. Default (recommended)")),
    row("  2. Fast"),
    row("  3. Large context"),
    row(""),
    row(dim("Enter to confirm · Esc to cancel")),
    ` ${dim(`╰${rule}╯`)}`,
  ].join("\n")}\n`;
}
