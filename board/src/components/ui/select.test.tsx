import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, test } from "vitest";

import { Select } from "./select";

afterEach(cleanup);

function Fixture({ onChange }: { onChange: (value: string) => void }) {
  const [value, setValue] = useState("b");
  return (
    <>
      <label htmlFor="pick">Pick</label>
      <Select
        id="pick"
        value={value}
        onValueChange={(next) => {
          setValue(next);
          onChange(next);
        }}
        options={[
          { value: "a", label: "Alpha" },
          { value: "b", label: "Bravo" },
          { value: "c", label: "Charlie", disabled: true },
        ]}
      />
    </>
  );
}

test("the select shows the chosen label, opens a list and reports the pick", async () => {
  const picked: string[] = [];
  render(<Fixture onChange={(value) => picked.push(value)} />);
  const trigger = screen.getByLabelText("Pick");
  expect(trigger.textContent).toContain("Bravo");

  fireEvent.click(trigger);
  const list = await screen.findByRole("listbox");
  expect(within(list).getAllByRole("option")).toHaveLength(3);
  expect(within(list).getByRole("option", { name: "Charlie" }).getAttribute("aria-disabled")).toBe(
    "true",
  );
  fireEvent.pointerDown(within(list).getByRole("option", { name: "Alpha" }));
  fireEvent.click(within(list).getByRole("option", { name: "Alpha" }));

  expect(picked).toEqual(["a"]);
  expect(screen.getByLabelText("Pick").textContent).toContain("Alpha");
});

test("groups show their headings and a disabled group cannot be picked", async () => {
  render(
    <Select
      label="Project"
      value="x"
      onValueChange={() => {}}
      groups={[
        { label: "host-a", options: [{ value: "x", label: "one" }] },
        { label: "host-b - down", disabled: true, options: [{ value: "y", label: "two" }] },
      ]}
    />,
  );
  fireEvent.click(screen.getByLabelText("Project"));
  const list = await screen.findByRole("listbox");
  expect(within(list).getByText("host-b - down")).toBeTruthy();
  expect(within(list).getByRole("option", { name: "two" }).getAttribute("aria-disabled")).toBe(
    "true",
  );
});
