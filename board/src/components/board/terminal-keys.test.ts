import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { env } from "node:process";

import { expect, test } from "vitest";

import { SOFT_KEYS, keyNames, pressTypes, typedKeys, unsentNote } from "./terminal-keys";

test.each([
  ["a", ["a"]],
  ["A", ["A"]],
  [" ", ["space"]],
  ["é€", ["é", "€"]],
  ["\r", ["enter"]],
  ["\x7f", ["backspace"]],
  ["\t", ["tab"]],
  ["\x1b", ["esc"]],
  ["\x03", ["ctrl+c"]],
  ["\x08", ["ctrl+h"]],
  ["\x00", ["ctrl+space"]],
  ["\x1c\x1d\x1f", ["ctrl+\\", "ctrl+]", "ctrl+/"]],
  ["\x1b[A\x1b[B\x1b[C\x1b[D", ["up", "down", "right", "left"]],
  ["\x1bOA", ["up"]],
  ["\x1b[1;2A", ["shift+up"]],
  ["\x1b[1;5D", ["ctrl+left"]],
  ["\x1b[1;4D", ["alt+shift+left"]],
  ["\x1b[1;8B", ["ctrl+alt+shift+down"]],
  ["\x1b[Z", ["shift+tab"]],
  ["\x1bOP\x1bOS", ["f1", "f4"]],
  ["\x1b[15~\x1b[24~", ["f5", "f12"]],
  ["\x1b[15;2~", ["shift+f5"]],
  ["\x1bb", ["alt+b"]],
  ["\x1bX", ["alt+X"]],
  ["\x1b]", ["alt+]"]],
  ["\x1bP", ["alt+P"]],
  ["\x1b^", ["alt+^"]],
  ["\x1b_", ["alt+_"]],
  ["\x1b\x7f", ["alt+backspace"]],
  ["\x1b\r", ["alt+enter"]],
  ["ls\r", ["l", "s", "enter"]],
])("%j is sent as %j", (data, keys) => {
  expect(typedKeys(data)).toEqual({ keys, unsent: [] });
});

test.each([
  ["\x1b[3~", "Delete"],
  ["\x1b[H", "Home"],
  ["\x1bOF", "End"],
  ["\x1b[5~", "Page Up"],
  ["\x1b[6~", "Page Down"],
  ["\x1b[2~", "Insert"],
])("%j is a key with no name in the list, so it is reported as %s", (data, name) => {
  expect(typedKeys(data)).toEqual({ keys: [], unsent: [name] });
});

// xterm sets its meta bit (8) for Cmd on a Mac. A key name has no Meta, and
// the key without it is another key, so it is reported, never sent bare.
test.each([
  ["\x1b[1;9A", "Meta+Up"],
  ["\x1b[1;10B", "Meta+Down"],
  ["\x1b[1;13D", "Meta+Left"],
  ["\x1b[1;16C", "Meta+Right"],
  ["\x1b[1;9P", "Meta+F1"],
  ["\x1b[15;9~", "Meta+F5"],
])("%j has the meta bit, so it is reported as %s and not sent", (data, name) => {
  expect(typedKeys(data)).toEqual({ keys: [], unsent: [name] });
});

test("Meta+Up between two keys sends the two keys and not Up", () => {
  expect(typedKeys("a\x1b[1;9Ab")).toEqual({ keys: ["a", "b"], unsent: ["Meta+Up"] });
});

test("unsent keys point at tiller attach", () => {
  expect(unsentNote(["Meta+Up"])).toEqual({
    title: "The board can't send Meta+Up",
    detail: "Nothing was sent for that key. Use tiller attach for it.",
  });
  expect(unsentNote(["Meta+Up", "F13", "Meta+F1"]).title).toBe(
    "The board can't send Meta+Up, F13 and Meta+F1",
  );
});

test.each([
  ["a cursor position report", "\x1b[12;40R"],
  ["a cursor position report on row 1", "\x1b[1;5R"],
  ["a focus report", "\x1b[I"],
  ["a mouse report", "\x1b[<0;10;5M"],
  ["a colour answer", "\x1b]11;rgb:0d0d/0e0e/1010\x1b\\"],
  ["a device attributes answer", "\x1b[?1;2c"],
])("%s is the terminal talking, not a key, so it is dropped", (_, data) => {
  expect(typedKeys(data)).toEqual({ keys: [], unsent: [] });
});

test("keys around a dropped report still go through", () => {
  expect(typedKeys("a\x1b[12;40Rb")).toEqual({ keys: ["a", "b"], unsent: [] });
});

// Every byte alone and after ESC, every SS3 key, and every CSI final with
// no parameter, a first parameter up to 127 alone or with a modifier
// parameter, and xterm's three-parameter keys, so a name a new escape path
// of those shapes sends shows up here.
function presses(): string[] {
  const bytes = Array.from({ length: 0x80 }, (_, code) => String.fromCharCode(code));
  const finals = Array.from({ length: 0x3f }, (_, code) => String.fromCharCode(0x40 + code));
  // xterm's modifier parameter runs 1 to 8, and to 16 with its meta bit.
  const mods = Array.from({ length: 16 }, (_, value) => `;${value + 1}`);
  const params = Array.from({ length: 0x80 }, (_, n) => [
    `${n}`,
    ...mods.map((mod) => `${n}${mod}`),
  ]).flat();
  const three = mods.flatMap((mod) =>
    Array.from({ length: 0x80 }, (_, code) => `27${mod};${code}`),
  );
  return [
    ...bytes,
    ...bytes.map((byte) => `\x1b${byte}`),
    ...finals.flatMap((final) => [
      `\x1bO${final}`,
      `\x1b[${final}`,
      ...params.map((param) => `\x1b[${param}${final}`),
    ]),
    ...three.flatMap((param) => [`\x1b[${param}~`, `\x1b[${param}u`]),
  ];
}

test("the key names are every name a key press sends, each once", () => {
  const names = keyNames();
  const sent = new Set(presses().flatMap((data) => typedKeys(data).keys));
  expect(new Set(names).size).toBe(names.length);
  expect(new Set(names)).toEqual(sent);
});

// Past ASCII a character goes as itself, so the list leaves it out:
// tiller-panes parses these same samples.
test.each(["\u0085", "\u00a0", "é", "ß", "中", "🦀"])("%j goes as itself", (char) => {
  expect(typedKeys(char).keys).toEqual([char]);
  expect(typedKeys(`\x1b${char}`).keys).toEqual([`alt+${char}`]);
});

test("every soft key is a key name", () => {
  const names = keyNames();
  for (const key of SOFT_KEYS) expect(names, key).toContain(key);
});

// tiller-panes parses every name in this file, so it must not fall behind
// the list. UPDATE_KEY_NAMES=1 rewrites it, but not on CI.
const FIXTURE = resolve(
  import.meta.dirname,
  "../../../../crates/panes/tests/fixtures/board-key-names.json",
);

test("the key-name fixture tiller-panes parses is the board's list", () => {
  const list = `${JSON.stringify(keyNames(), null, 2)}\n`;
  // A checkout with autocrlf has CRLF lines; the names are the same.
  const committed = readFileSync(FIXTURE, "utf8").replaceAll("\r\n", "\n");
  const onCi = !!env.CI && env.CI !== "false" && env.CI !== "0";
  if (committed !== list && env.UPDATE_KEY_NAMES === "1" && !onCi) {
    writeFileSync(FIXTURE, list);
    expect.fail("rewrote board-key-names.json: commit it and run the tests again");
  }
  expect(committed, "run UPDATE_KEY_NAMES=1 bun run --cwd board test, with CI unset").toBe(list);
});

function stage(): { root: HTMLElement; term: HTMLElement } {
  const root = document.createElement("div");
  root.innerHTML = `<div class="xterm"><textarea></textarea><span class="cell">x</span></div>
    <button type="button"><svg></svg></button><a href="#">link</a>
    <div tabindex="0">focusable</div><span class="bare">margin</span>`;
  return { root, term: root.querySelector(".xterm") as HTMLElement };
}

const plain = { button: 0, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false };

test("a plain press on the bare stage types", () => {
  const { root, term } = stage();
  expect(pressTypes({ ...plain, target: root }, term)).toBe(true);
  expect(pressTypes({ ...plain, target: root.querySelector(".bare") }, term)).toBe(true);
});

test("a press on xterm or on a control is theirs", () => {
  const { root, term } = stage();
  for (const selector of [".cell", "textarea", "button", "svg", "a", "[tabindex]"]) {
    expect(pressTypes({ ...plain, target: root.querySelector(selector) }, term), selector).toBe(
      false,
    );
  }
});

test("a press with a modifier or another button does not type", () => {
  const { root, term } = stage();
  expect(pressTypes({ ...plain, button: 2, target: root }, term)).toBe(false);
  for (const key of ["ctrlKey", "metaKey", "altKey", "shiftKey"]) {
    expect(pressTypes({ ...plain, [key]: true, target: root }, term), key).toBe(false);
  }
});
