import { afterEach, expect, test } from "vitest";

import { shortcutsBlocked } from "./shortcuts";

afterEach(() => {
  document.body.replaceChildren();
});

function keyOn(target: Element): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key: "a", bubbles: true });
  target.dispatchEvent(event);
  return event;
}

test("a key on the page is free", () => {
  const button = document.createElement("button");
  document.body.append(button);
  expect(shortcutsBlocked()).toBe(false);
  expect(shortcutsBlocked(keyOn(button))).toBe(false);
});

test("an open modal dialog blocks every shortcut, wherever focus is", () => {
  const button = document.createElement("button");
  const dialog = document.createElement("div");
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-modal", "true");
  document.body.append(button, dialog);
  expect(shortcutsBlocked()).toBe(true);
  expect(shortcutsBlocked(keyOn(button))).toBe(true);
});

test("a popover that is not modal blocks nothing", () => {
  const dialog = document.createElement("div");
  dialog.setAttribute("role", "dialog");
  document.body.append(dialog);
  expect(shortcutsBlocked()).toBe(false);
});

test("a native dialog blocks every shortcut when it is modal, not when it is only open", () => {
  const dialog = document.createElement("dialog");
  dialog.setAttribute("open", "");
  document.body.append(dialog);
  expect(shortcutsBlocked()).toBe(false);
  // jsdom has no showModal(): stand in for a dialog it opened.
  Object.defineProperty(dialog, "matches", {
    value: (selector: string) => selector === ":modal",
  });
  expect(shortcutsBlocked()).toBe(true);
});

test.each([
  ["an input", () => document.createElement("input")],
  ["a text area", () => document.createElement("textarea")],
  ["a select", () => document.createElement("select")],
  [
    "an editor",
    () => {
      const editor = document.createElement("div");
      editor.setAttribute("contenteditable", "true");
      editor.append(document.createElement("p"));
      return editor;
    },
  ],
])("a key typed into %s is the field's own", (_name, make) => {
  const field = make();
  document.body.append(field);
  const target = field.firstElementChild ?? field;
  expect(shortcutsBlocked(keyOn(target))).toBe(true);
  // Without the event, only a dialog blocks: Escape still works from a field.
  expect(shortcutsBlocked()).toBe(false);
});
