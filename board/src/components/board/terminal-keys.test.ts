import { expect, test } from "vitest";

import { pressTypes, typedKeys } from "./terminal-keys";

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
])("%j is a key herdr cannot send, so it is reported as %s", (data, name) => {
  expect(typedKeys(data)).toEqual({ keys: [], unsent: [name] });
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
