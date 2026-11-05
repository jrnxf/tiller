// A stand-in herdr pane for the fixture server: the screen an agent might
// have up, as the bytes a terminal would get. The live terminal draws it the
// way it draws a real pane, so its looks can be checked without herdr.

export const FIXTURE_COLS = 100;
export const FIXTURE_ROWS = 28;

const ESC = "\x1b";
const dim = (text: string) => `${ESC}[2m${text}${ESC}[22m`;
const color = (code: number, text: string) => `${ESC}[${code}m${text}${ESC}[39m`;
const bold = (text: string) => `${ESC}[1m${text}${ESC}[22m`;

// The row the thinking line is on, and the prompt's, 1-based.
const THINKING_ROW = 22;
const PROMPT_ROW = 25;

const RULE = "─".repeat(FIXTURE_COLS - 4);

function lines(): string[] {
  return [
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
    thinking(0),
    "",
    ` ${dim(`╭${RULE}╮`)}`,
    prompt(""),
    ` ${dim(`╰${RULE}╯`)}`,
    `  ${dim("? for shortcuts")}${" ".repeat(FIXTURE_COLS - 36)}${dim("auto-accept on")}`,
  ];
}

function thinking(seconds: number): string {
  return ` ${color(35, "✻")} ${color(35, "Checking the lint")} ${dim(`(${seconds}s · esc to interrupt)`)}`;
}

function prompt(typed: string): string {
  const room = FIXTURE_COLS - 6;
  const shown = typed.length > room ? typed.slice(-room) : typed;
  return ` ${dim("│")} ${color(36, ">")} ${shown}${" ".repeat(room - shown.length - 1)}${dim("│")}`;
}

// The cursor sits after what was typed at the prompt.
function cursor(typed: string): string {
  const room = FIXTURE_COLS - 6;
  return `${ESC}[${PROMPT_ROW};${6 + Math.min(typed.length, room - 1)}H`;
}

const encoder = new TextEncoder();

function base64(text: string): string {
  let binary = "";
  for (const byte of encoder.encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

// The first frame of a run: the whole screen, cleared first, the way herdr
// sends one.
export function fullFrame(typed: string, seconds: number): string {
  const rows = lines();
  rows[THINKING_ROW - 1] = thinking(seconds);
  rows[PROMPT_ROW - 1] = prompt(typed);
  return base64(
    `${ESC}[?2026h${ESC}[2J${ESC}[1;1H${rows.join("\r\n")}${cursor(typed)}${ESC}[?2026l`,
  );
}

// A diff: the thinking line's clock moved.
export function tickFrame(typed: string, seconds: number): string {
  return base64(`${ESC}[${THINKING_ROW};1H${ESC}[2K${thinking(seconds)}${cursor(typed)}`);
}

// A diff: the prompt line, after a key.
export function promptFrame(typed: string): string {
  return base64(`${ESC}[${PROMPT_ROW};1H${ESC}[2K${prompt(typed)}${cursor(typed)}`);
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
