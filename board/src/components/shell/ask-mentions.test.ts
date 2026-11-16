import { expect, test } from "vitest";

import { rankMentions } from "./ask-mentions";
import type { MentionOption } from "./ask-mentions";

const option = (name: string): MentionOption => ({
  key: name.toLowerCase(),
  name,
  color: "#888888",
  host: null,
  off: null,
});

const names = (query: string) =>
  rankMentions(["Atlas", "Cobalt", "Lantern", "Basalt"].map(option), query).map((o) => o.name);

test("Enter takes the project whose name starts with the typed letters", () => {
  // Atlas holds "la" too, and is listed first: it must not win over Lantern.
  expect(names("la")).toEqual(["Lantern", "Atlas"]);
  expect(names("ba")).toEqual(["Basalt", "Cobalt"]);
});

test("no query keeps the listed order, and letters out of order find nothing", () => {
  expect(names("")).toEqual(["Atlas", "Cobalt", "Lantern", "Basalt"]);
  expect(names("zz")).toEqual([]);
});
