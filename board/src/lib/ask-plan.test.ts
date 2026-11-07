import { describe, expect, it } from "vitest";

import { askFor, footerFor, mentionedKeys, messageFor } from "./ask-plan";
import type { AskPiece } from "./ask-plan";

const text = (value: string): AskPiece => ({ kind: "text", text: value });
const chip = (key: string, name = key): AskPiece => ({ kind: "mention", key, name });

describe("messageFor", () => {
  it("drops the chips and spaces at the start", () => {
    const pieces = [chip("web-shop"), text(" "), chip("docs-site"), text(" optimize fonts")];
    expect(messageFor(pieces)).toBe("optimize fonts");
  });

  it("keeps a chip in the middle of the sentence as the plain project name", () => {
    const pieces = [chip("web-shop"), text(" match what "), chip("design-system"), text(" ships")];
    expect(messageFor(pieces)).toBe("match what design-system ships");
  });

  it("uses the project name, not the host-qualified key", () => {
    const pieces = [chip("web-shop"), text(" copy from "), chip("agents/docs-site", "docs-site")];
    expect(messageFor(pieces)).toBe("copy from docs-site");
  });

  it("collapses runs of spaces and keeps line breaks", () => {
    const pieces = [chip("a"), text("  first   line\nsecond  line  ")];
    expect(messageFor(pieces)).toBe("first line\nsecond line");
  });

  it("is empty when there are only chips", () => {
    expect(messageFor([chip("a"), text(" "), chip("b")])).toBe("");
  });
});

describe("mentionedKeys", () => {
  it("lists each project once in the order it first appears", () => {
    const pieces = [chip("b"), text(" "), chip("a"), text(" and "), chip("b")];
    expect(mentionedKeys(pieces)).toEqual(["b", "a"]);
  });
});

describe("footerFor and askFor", () => {
  it("names the other projects", () => {
    expect(footerFor(["docs-site", "design-system"])).toBe(
      "Also asked of: docs-site, design-system",
    );
    expect(askFor("optimize fonts", ["docs-site"])).toBe(
      "optimize fonts\n\nAlso asked of: docs-site",
    );
  });

  it("adds nothing for a single project", () => {
    expect(footerFor([])).toBeNull();
    expect(askFor("optimize fonts", [])).toBe("optimize fonts");
  });
});
