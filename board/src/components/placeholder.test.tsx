import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";

import { Placeholder } from "./placeholder";

test("placeholder renders the heading", () => {
  render(<Placeholder />);
  expect(screen.getByRole("heading", { name: "Tiller" })).toBeTruthy();
});
