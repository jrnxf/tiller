import { describe, expect, it } from "vitest";

import { lineCount, parseAnsi, stripAnsi, tidy, trimChrome } from "./ansi";

const e = (code: string) => `\u001b[${code}m`;

describe("parseAnsi", () => {
  it("keeps 24-bit foreground, bold, dim and italic, and drops backgrounds", () => {
    const segments = parseAnsi(
      `${e("38;2;1;2;3")}${e("48;2;9;9;9")}${e("1")}hi${e("0")} ${e("2")}${e("3")}x${e("22")}${e("23")}y`,
    );
    expect(segments).toEqual([
      { text: "hi", fg: "1,2,3", bold: true, dim: false, italic: false },
      { text: " ", fg: null, bold: false, dim: false, italic: false },
      { text: "x", fg: null, bold: false, dim: true, italic: true },
      { text: "y", fg: null, bold: false, dim: false, italic: false },
    ]);
  });

  it("reads a background's parameters without taking them as a foreground", () => {
    const [segment] = parseAnsi(`${e("48;2;38;79;120")}text`);
    expect(segment).toMatchObject({ text: "text", fg: null });
  });

  it("drops cursor and erase sequences and keeps the text", () => {
    expect(
      parseAnsi("a\u001b[2Kb\u001b[1;1Hc")
        .map((s) => s.text)
        .join(""),
    ).toBe("abc");
  });
});

describe("trimChrome", () => {
  it("cuts at the first line of 20 or more box-drawing dashes, whatever its colour", () => {
    const rule = `${e("38;2;136;136;136")}${"─".repeat(20)}${e("0")}`;
    expect(trimChrome(`reply\nmore\n${rule}\n❯ draft\n${rule}\nstatus`)).toBe("reply\nmore");
  });

  it("keeps a shorter run of dashes", () => {
    const text = `a\n${"─".repeat(19)}\nb`;
    expect(trimChrome(text)).toBe(text);
  });
});

describe("tidy", () => {
  it("strips trailing space, collapses blank runs and trims", () => {
    expect(tidy("\n\na  \n\n\n\nb\t\n")).toBe("a\n\nb");
  });

  it("counts the lines a reader sees", () => {
    const tidied = tidy(`${e("1")}one${e("0")}\n\n\n\ntwo`);
    expect(lineCount(tidied)).toBe(3);
    expect(stripAnsi(tidied)).toBe("one\n\ntwo");
  });
});
