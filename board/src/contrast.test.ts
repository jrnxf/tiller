import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { expect, test } from "vitest";

// The text colors in styles.css are chosen to meet WCAG AA (4.5:1) on every
// surface and tint they sit on. A palette edit that quietly drops one under
// the line is the kind of change no screenshot review catches.

type Rgb = [number, number, number];

const css = readFileSync(resolve(import.meta.dirname, "styles.css"), "utf8");

function block(selector: string): Record<string, string> {
  const start = css.indexOf(`${selector} {`);
  const body = css.slice(start, css.indexOf("\n}", start));
  const vars: Record<string, string> = {};
  for (const match of body.matchAll(/--([\w-]+):\s*(#[0-9a-f]{6})\b/gi)) {
    vars[match[1] as string] = match[2] as string;
  }
  return vars;
}

function rgb(hex: string): Rgb {
  const value = hex.replace("#", "");
  return [0, 2, 4].map((i) => Number.parseInt(value.slice(i, i + 2), 16)) as Rgb;
}

function channel(v: number): number {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance([r, g, b]: Rgb): number {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrast(a: Rgb, b: Rgb): number {
  const [la, lb] = [luminance(a), luminance(b)];
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

function over(top: Rgb, alpha: number, base: Rgb): Rgb {
  return top.map((v, i) => v * alpha + (base[i] as number) * (1 - alpha)) as Rgb;
}

const light = block(":root");
const dark = { ...light, ...block(':root[data-theme="dark"]') };

const SURFACES = ["bg-0", "bg-1", "bg-2", "bg-sidebar", "bg-card", "bg-panel"];
const GREYS = ["fg-1", "fg-2", "fg-3", "fg-4"];
// Text color token, the hue its chip tints the background with, and how strongly.
const HUES = [
  ["green-text", "green", 0.14],
  ["green-text", "green", 0.09],
  ["yellow-text", "yellow", 0.14],
  ["orange-text", "orange", 0.12],
  ["orange-text", "orange", 0.09],
  ["red-text", "red", 0.12],
  ["red-text", "red", 0.09],
  ["m-opus-text", "m-opus", 0.14],
  ["m-sonnet-text", "m-sonnet", 0.14],
  ["m-haiku-text", "m-haiku", 0.14],
] as const;

// board.css owns the model hues; they are the same in both themes' light/dark
// pairs there, so the tint is taken from the text token itself when absent.
const MODEL_HUES: Record<string, { light: string; dark: string }> = {
  "m-opus": { light: "#9a7dff", dark: "#b4a0ff" },
  "m-sonnet": { light: "#3f97f0", dark: "#6db4fb" },
  "m-haiku": { light: "#2fb98f", dark: "#58d3ad" },
};

for (const [name, vars] of [
  ["light", light],
  ["dark", dark],
] as const) {
  test(`${name}: grey text reaches 4.5:1 on every surface`, () => {
    for (const fg of GREYS) {
      for (const surface of SURFACES) {
        const ratio = contrast(rgb(vars[fg] as string), rgb(vars[surface] as string));
        expect(ratio, `${fg} on ${surface}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  test(`${name}: hue text reaches 4.5:1 on its own tint over every surface`, () => {
    for (const [token, hue, amount] of HUES) {
      const tint = rgb(vars[hue] ?? (MODEL_HUES[hue] as Record<string, string>)[name]!);
      for (const surface of SURFACES) {
        const bg = over(tint, amount, rgb(vars[surface] as string));
        const ratio = contrast(rgb(vars[token] as string), bg);
        expect(ratio, `${token} on ${hue} tint over ${surface}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  test(`${name}: white on the solid accent and accent text on its tint reach 4.5:1`, () => {
    expect(contrast([255, 255, 255], rgb(vars["accent-solid"] as string))).toBeGreaterThanOrEqual(
      4.5,
    );
    expect(
      contrast([255, 255, 255], rgb(vars["accent-solid-hover"] as string)),
    ).toBeGreaterThanOrEqual(4.5);
    expect(
      contrast(rgb(vars["accent-text"] as string), rgb(vars["accent-tint"] as string)),
    ).toBeGreaterThanOrEqual(4.5);
  });
}
