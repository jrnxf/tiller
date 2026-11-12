// What the owner typed into the mirror, as the key names `herdr pane
// send-keys` takes. xterm turns each key press into the bytes a terminal
// would send; this reads them back into names. herdr 0.9.5 has no names for
// Home, End, Delete, Insert, Page Up and Page Down, and a key name has no
// Meta, so those are reported instead of sent. Any other escape sequence (a
// terminal's answer to a query, a mouse or focus report) is dropped: only
// keys a person pressed reach the agent.

export interface TypedKeys {
  keys: string[];
  // Keys herdr cannot send, by the name a person knows them by.
  unsent: string[];
}

const ARROWS: Record<string, string> = { A: "up", B: "down", C: "right", D: "left" };

// SS3 and CSI final bytes for F1 to F4, and the `~` numbers for the rest.
const SS3_F: Record<string, string> = { P: "f1", Q: "f2", R: "f3", S: "f4" };
const TILDE: Record<string, string> = {
  "11": "f1",
  "12": "f2",
  "13": "f3",
  "14": "f4",
  "15": "f5",
  "17": "f6",
  "18": "f7",
  "19": "f8",
  "20": "f9",
  "21": "f10",
  "23": "f11",
  "24": "f12",
};
const TILDE_UNSENT: Record<string, string> = {
  "1": "Home",
  "2": "Insert",
  "3": "Delete",
  "4": "End",
  "5": "Page Up",
  "6": "Page Down",
  "7": "Home",
  "8": "End",
};
const LETTER_UNSENT: Record<string, string> = { H: "Home", F: "End" };

// xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4) + meta(8).
function modifiers(param: string | undefined): string {
  const value = Number(param ?? "1") - 1;
  if (!Number.isInteger(value) || value <= 0) return "";
  return (value & 4 ? "ctrl+" : "") + (value & 2 ? "alt+" : "") + (value & 1 ? "shift+" : "");
}

// Whether the parameter holds xterm's meta bit (Cmd on a Mac). A key name has
// no meta prefix (terminal-holder.md 4.3), and the key without it is another
// key, so a key with Meta is reported, not sent.
function hasMeta(param: string | undefined): boolean {
  const value = Number(param ?? "1") - 1;
  return Number.isInteger(value) && value >= 8;
}

// A modified key: its name, or the name a person knows it by with Meta.
function withModifiers(param: string | undefined, key: string): { key?: string; unsent?: string } {
  if (!hasMeta(param)) return { key: modifiers(param) + key };
  const name = key.length > 1 ? key[0]?.toUpperCase() + key.slice(1) : key;
  return { unsent: `Meta+${name}` };
}

// One printable character, as herdr names it.
function printable(char: string): string {
  return char === " " ? "space" : char;
}

// A control byte: Ctrl with a letter, or one of the keys that send one.
function control(code: number): string | null {
  if (code === 0x0d) return "enter";
  if (code === 0x09) return "tab";
  if (code === 0x7f) return "backspace";
  if (code === 0x00) return "ctrl+space";
  if (code >= 0x01 && code <= 0x1a) return `ctrl+${String.fromCharCode(code + 0x60)}`;
  if (code === 0x1c) return "ctrl+\\";
  if (code === 0x1d) return "ctrl+]";
  if (code === 0x1f) return "ctrl+/";
  return null;
}

// The keys a phone lacks, which the stage shows as buttons.
export const SOFT_KEYS = ["esc", "tab", "ctrl+c", "left", "up", "down", "right", "enter"] as const;
export type SoftKey = (typeof SOFT_KEYS)[number];

// Every name the board sends: what `typedKeys` reads, which takes in each
// soft key, but a printable character past ASCII, which goes as itself (and
// with alt+). tiller-panes parses each name in
// crates/panes/tests/fixtures/board-key-names.json, and a test here keeps
// that file equal to this list.
export function keyNames(): string[] {
  const prefixes = Array.from({ length: 8 }, (_, value) => modifiers(String(value + 1)));
  const modified = new Set([
    ...Object.values(ARROWS),
    ...Object.values(SS3_F),
    ...Object.values(TILDE),
  ]);
  const named = [...modified].flatMap((key) => prefixes.map((prefix) => prefix + key));
  const controls: string[] = [];
  for (let code = 0; code < 0x80; code++) {
    const key = control(code);
    if (key) controls.push(key);
  }
  const chars: string[] = [];
  for (let code = 0x20; code < 0x7f; code++) chars.push(printable(String.fromCharCode(code)));
  const alts = [...controls, ...chars].map((key) => `alt+${key}`);
  return [...named, "shift+tab", "esc", ...controls, ...chars, ...alts];
}

// One escape sequence at the start of `rest` (which begins with ESC): its
// length and what it means.
function escape(rest: string): { length: number; key?: string; unsent?: string } {
  // Escape sequences are made of control bytes, so these patterns match them.
  // oxlint-disable-next-line no-control-regex
  const csi = /^\x1b\[([<=>?]?)([0-9;:]*)[\x20-\x2f]*([\x40-\x7e])/.exec(rest);
  if (csi) {
    const [match, marker = "", params = "", final = ""] = csi;
    // A private marker is a report (mouse, device attributes), never a key.
    if (marker) return { length: match.length };
    const parts = params.split(";");
    if (ARROWS[final]) {
      return { length: match.length, ...withModifiers(parts[1], ARROWS[final]) };
    }
    if (final === "Z") return { length: match.length, key: "shift+tab" };
    // `ESC[1;5R` is Ctrl+F3 and also a cursor position report for row 1, so
    // an `R` is always read as the report. F3 itself comes as `ESC O R`.
    if (SS3_F[final] && final !== "R" && parts[0] === "1") {
      return { length: match.length, ...withModifiers(parts[1], SS3_F[final]) };
    }
    const tilde = final === "~" && parts[0] ? TILDE[parts[0]] : undefined;
    if (tilde) return { length: match.length, ...withModifiers(parts[1], tilde) };
    if (final === "~" && parts[0] && TILDE_UNSENT[parts[0]]) {
      return { length: match.length, unsent: TILDE_UNSENT[parts[0]] };
    }
    if (LETTER_UNSENT[final]) return { length: match.length, unsent: LETTER_UNSENT[final] };
    return { length: match.length };
  }
  // oxlint-disable-next-line no-control-regex
  const ss3 = /^\x1bO([A-Za-z])/.exec(rest);
  if (ss3) {
    const final = ss3[1] ?? "";
    const key = ARROWS[final] ?? SS3_F[final];
    if (key) return { length: 3, key };
    return { length: 3, unsent: LETTER_UNSENT[final] };
  }
  // OSC, DCS and the like end at BEL or ST; drop the whole report. Without
  // the end it is Alt with `]`, `P`, `^` or `_`.
  // oxlint-disable-next-line no-control-regex
  const string = /^\x1b[\]P^_][^\x07\x1b]*(\x07|\x1b\\)/.exec(rest);
  if (string) return { length: string[0].length };
  if (rest.length === 1) return { length: 1, key: "esc" };
  // Alt with a key: ESC, then that key's own byte.
  const next = rest.codePointAt(1) ?? 0;
  const char = String.fromCodePoint(next);
  if (next === 0x1b) return { length: 1, key: "esc" };
  const named = next < 0x20 || next === 0x7f ? control(next) : printable(char);
  return named ? { length: 1 + char.length, key: `alt+${named}` } : { length: 1 + char.length };
}

export function typedKeys(data: string): TypedKeys {
  const keys: string[] = [];
  const unsent: string[] = [];
  let index = 0;
  while (index < data.length) {
    const rest = data.slice(index);
    const code = rest.codePointAt(0) ?? 0;
    if (code === 0x1b) {
      const sequence = escape(rest);
      if (sequence.key) keys.push(sequence.key);
      if (sequence.unsent && !unsent.includes(sequence.unsent)) unsent.push(sequence.unsent);
      index += sequence.length;
      continue;
    }
    const char = String.fromCodePoint(code);
    index += char.length;
    if (code < 0x20 || code === 0x7f) {
      const key = control(code);
      if (key) keys.push(key);
    } else {
      keys.push(printable(char));
    }
  }
  return { keys, unsent };
}

function list(names: string[]): string {
  if (names.length < 2) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
}

// What the board says of keys it did not send. A holder pane takes every key
// through `tiller attach`, which passes the owner's bytes through as typed; a
// herdr pane takes them in herdr itself.
export function unsentNote(unsent: string[], holder: boolean): { title: string; detail: string } {
  return holder
    ? {
        title: `The board can't send ${list(unsent)}`,
        detail: "Nothing was sent for that key. Use tiller attach for it.",
      }
    : {
        title: `herdr can't send ${list(unsent)}`,
        detail: "Nothing was sent for that key. Use herdr itself for it.",
      };
}

// The controls a press on the stage leaves alone.
const CONTROLS = "button, a, input, textarea, select, label, summary, [role=button], [tabindex]";

export interface ScreenPress {
  button: number;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  target: EventTarget | null;
}

// Whether a press on the screen around the pane's cells starts typing: a
// plain primary press on the bare stage. A press on xterm's own element is
// xterm's to handle, and a control on the stage keeps its own.
export function pressTypes(press: ScreenPress, term: Element): boolean {
  if (press.button !== 0) return false;
  if (press.ctrlKey || press.metaKey || press.altKey || press.shiftKey) return false;
  const target = press.target;
  if (!(target instanceof Node) || term.contains(target)) return false;
  return !(target instanceof Element && target.closest(CONTROLS));
}
