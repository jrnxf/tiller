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
