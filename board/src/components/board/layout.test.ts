import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { expect, test } from "vitest";

// jsdom does no layout, so these tests read the rules that keep two things
// from colliding. Each assertion names the rule a screenshot at 390 px or
// 1440 px showed was missing.

const css = readFileSync(resolve(import.meta.dirname, "board.css"), "utf8");

// The media query's body, from its opening line to its closing brace.
function media(query: string): string[] {
  const bodies: string[] = [];
  let from = css.indexOf(`@media (${query}) {`);
  while (from !== -1) {
    const end = css.indexOf("\n}\n", from);
    expect(end, `an unclosed @media (${query})`).toBeGreaterThan(from);
    bodies.push(css.slice(from, end));
    from = css.indexOf(`@media (${query}) {`, end);
  }
  return bodies;
}

// The declarations of the rule with this exact selector, inside the text.
function rule(text: string, selector: string): string {
  const start = text.search(new RegExp(`^\\s*${selector.replaceAll(".", "\\.")} \\{`, "m"));
  expect(start, `no rule for ${selector}`).toBeGreaterThanOrEqual(0);
  return text.slice(text.indexOf("{", start) + 1, text.indexOf("}", start));
}

const narrow = media("max-width: 820px").join("\n");

test("the attempt's status takes its own row on a narrow screen, so the buttons cannot cut it", () => {
  const acts = rule(narrow, ".card-page .run .acts");
  expect(acts).toMatch(/flex-wrap:\s*wrap/);
  const status = rule(narrow, ".card-page .run .acts .meta");
  expect(status).toMatch(/flex:\s*1 0 100%/);
});

test("the attempt's buttons stay on one row with the status on a wide screen", () => {
  expect(rule(css, ".card-page .run .acts")).toMatch(/flex-wrap:\s*nowrap/);
});

test("the toast sits at the top, clear of the keyboard hints and the message box", () => {
  const toast = rule(css, ".toast");
  expect(toast).toMatch(/top:\s*8px/);
  expect(toast).not.toMatch(/bottom:/);
  expect(toast).toMatch(/pointer-events:\s*none/);
});

test("the toast stays within the screen on a narrow one", () => {
  const toast = rule(narrow, ".toast");
  expect(toast).toMatch(/left:\s*50%/);
  expect(toast).toMatch(/max-width:\s*calc\(100% - 32px\)/);
});
