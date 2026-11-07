import { fireEvent, screen, within } from "@testing-library/react";

// Opens a Select from its trigger and picks an option, as a pointer does.
export async function choose(trigger: HTMLElement, name: string | RegExp) {
  fireEvent.click(trigger);
  const list = await screen.findByRole("listbox");
  const option = within(list).getByRole("option", { name });
  fireEvent.pointerDown(option);
  fireEvent.click(option);
}
