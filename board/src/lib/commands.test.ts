import { expect, test } from "vitest";

import { rankCommands, scoreMatch } from "./commands";
import type { Command } from "./commands";

const run = () => {};
const commands: Command[] = [
  { id: "board", group: "Go to", label: "Board", run },
  { id: "health", group: "Go to", label: "Health", keywords: "quota", run },
  { id: "dark", group: "Theme", label: "Theme: dark", run },
  { id: "density", group: "Density", label: "Density: compact", run },
];

test("an empty query lists everything in order", () => {
  expect(rankCommands(commands, "  ").map((c) => c.id)).toEqual([
    "board",
    "health",
    "dark",
    "density",
  ]);
});

test("characters must appear in order, and keywords find a command", () => {
  expect(rankCommands(commands, "hlt").map((c) => c.id)).toEqual(["health"]);
  expect(rankCommands(commands, "quota").map((c) => c.id)).toEqual(["health"]);
  expect(rankCommands(commands, "zzz")).toEqual([]);
});

test("a word-start match outranks a scattered one", () => {
  expect(rankCommands(commands, "d").map((c) => c.id)[0]).toBe("density");
  expect(scoreMatch("Theme: dark", "td")).not.toBeNull();
  expect(scoreMatch("Board", "x")).toBeNull();
});

test("a thing to find is listed only for a query that holds its words", () => {
  const card: Command = {
    id: "card",
    group: "Cards",
    label: "alpha-7 Weekly digest email",
    searchOnly: true,
    run,
  };
  const all = [...commands, card];
  expect(rankCommands(all, "").map((c) => c.id)).not.toContain("card");
  expect(rankCommands(all, "digest").map((c) => c.id)).toEqual(["card"]);
  expect(rankCommands(all, "alpha-7").map((c) => c.id)).toEqual(["card"]);
  // Words in another order still find it; letters scattered over it do not.
  expect(rankCommands(all, "email weekly").map((c) => c.id)).toEqual(["card"]);
  expect(rankCommands(all, "wdl").map((c) => c.id)).not.toContain("card");
});

test("the command a query names comes before a card that only holds the word", () => {
  const all: Command[] = [
    { id: "card", group: "Cards", label: "alpha-3 new thing", searchOnly: true, run },
    { id: "job", group: "New card", label: "New card in Alpha", searchOnly: true, run },
  ];
  expect(rankCommands(all, "new").map((c) => c.id)).toEqual(["job", "card"]);
});

test("a group stays in one piece, in the order of its best match", () => {
  const mixed: Command[] = [
    { id: "a1", group: "A", label: "zz pylon", run },
    { id: "b1", group: "B", label: "pylon", run },
    { id: "a2", group: "A", label: "pylon one", run },
  ];
  expect(rankCommands(mixed, "pylon").map((c) => c.id)).toEqual(["b1", "a2", "a1"]);
});
