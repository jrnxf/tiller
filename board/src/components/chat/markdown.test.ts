import { expect, test } from "vitest";

import { untagged } from "../../lib/orchestrator-text";
import { parseBlocks, parseInline } from "./markdown";

test("paragraphs split on blank lines, and a single newline stays a break", () => {
  expect(parseBlocks("One\ntwo\n\nThree")).toEqual([
    { kind: "p", text: "One\ntwo" },
    { kind: "p", text: "Three" },
  ]);
  expect(parseInline("One\ntwo")).toEqual([
    { kind: "text", text: "One" },
    { kind: "br" },
    { kind: "text", text: "two" },
  ]);
});

test("bullets and numbers are lists, a number keeps its start, and an item goes on", () => {
  expect(parseBlocks("Options:\n- wait\n- retry\n  with backoff\n\n3. three\n4) four")).toEqual([
    { kind: "p", text: "Options:" },
    {
      kind: "list",
      ordered: false,
      start: 1,
      items: [[{ kind: "p", text: "wait" }], [{ kind: "p", text: "retry\nwith backoff" }]],
    },
    {
      kind: "list",
      ordered: true,
      start: 3,
      items: [[{ kind: "p", text: "three" }], [{ kind: "p", text: "four" }]],
    },
  ]);
});

test("a nested list stays inside its item", () => {
  const [list] = parseBlocks("1. Build\n   - tests\n   - lint\n2. Ship");
  expect(list).toMatchObject({ kind: "list", ordered: true });
  if (list?.kind !== "list") throw new Error("not a list");
  expect(list.items[0]).toEqual([
    { kind: "p", text: "Build" },
    {
      kind: "list",
      ordered: false,
      start: 1,
      items: [[{ kind: "p", text: "tests" }], [{ kind: "p", text: "lint" }]],
    },
  ]);
  expect(list.items[1]).toEqual([{ kind: "p", text: "Ship" }]);
});

test("a numbered run on one line becomes a list, and a lone number does not", () => {
  expect(parseBlocks("Two ways: 1) retry the job, 2) skip it.")).toEqual([
    { kind: "p", text: "Two ways:" },
    {
      kind: "list",
      ordered: true,
      start: 1,
      items: [[{ kind: "p", text: "retry the job" }], [{ kind: "p", text: "skip it." }]],
    },
  ]);
  expect(parseBlocks("Version 1. ships today")).toEqual([
    { kind: "p", text: "Version 1. ships today" },
  ]);
});

test("code, quotes and headings are blocks of their own", () => {
  expect(parseBlocks("## Plan\n> quoted\n```\n- not a list\n```")).toEqual([
    { kind: "h", text: "Plan" },
    { kind: "quote", blocks: [{ kind: "p", text: "quoted" }] },
    { kind: "code", text: "- not a list" },
  ]);
});

test("inline code keeps its words, and only web addresses link", () => {
  expect(
    parseInline("Run `**not bold**` then **bold _and em_** at https://example.com/a."),
  ).toEqual([
    { kind: "text", text: "Run " },
    { kind: "code", text: "**not bold**" },
    { kind: "text", text: " then " },
    {
      kind: "strong",
      children: [
        { kind: "text", text: "bold " },
        { kind: "em", children: [{ kind: "text", text: "and em" }] },
      ],
    },
    { kind: "text", text: " at " },
    {
      kind: "link",
      href: "https://example.com/a",
      children: [{ kind: "text", text: "https://example.com/a" }],
    },
    { kind: "text", text: "." },
  ]);
  expect(parseInline("[x](javascript:alert(1))")).toEqual([
    { kind: "text", text: "[x](javascript:alert(1))" },
  ]);
  expect(parseInline("snake_case_name and 2*3*4")).toEqual([
    { kind: "text", text: "snake_case_name and 2*3*4" },
  ]);
});

test("the tag and project name an orchestrator line starts with are dropped", () => {
  expect(untagged("[chat] alpha: the export runs at 02:00.")).toBe("the export runs at 02:00.");
  expect(untagged("[alpha-41] alpha: picked up\n[chat] beta: and this")).toBe(
    "picked up\nand this",
  );
  expect(untagged("[x] a checkbox stays")).toBe("[x] a checkbox stays");
  expect(untagged("See [alpha-41] inline")).toBe("See [alpha-41] inline");
});
