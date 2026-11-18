import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { HandoffPage } from "./handoff-page";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function answer(status: number, body: unknown) {
  const fetch = vi.fn(
    async (_path: string, _init: RequestInit) => new Response(JSON.stringify(body), { status }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

test("it names the board it would sign in, and does nothing until asked", () => {
  const fetch = answer(200, {});
  render(<HandoffPage to="7440.board.example.com" path="/inbox" />);
  expect(screen.getByRole("heading").textContent).toBe("Sign in on 7440.board.example.com?");
  const yes = screen.getByRole("button", { name: "Sign in there" });
  expect(document.activeElement).toBe(yes);
  expect(screen.getByRole("link", { name: "Cancel" }).getAttribute("href")).toBe("/");
  expect(fetch).not.toHaveBeenCalled();
});

test("a yes asks for the login link and opens it", async () => {
  const fetch = answer(200, { url: "https://7440.board.example.com/login/ab12?to=%2Finbox" });
  const assign = vi.fn();
  vi.stubGlobal("location", { ...window.location, assign });
  render(<HandoffPage to="7440.board.example.com" path="/inbox" />);
  fireEvent.click(screen.getByRole("button", { name: "Sign in there" }));
  await waitFor(() =>
    expect(assign).toHaveBeenCalledWith("https://7440.board.example.com/login/ab12?to=%2Finbox"),
  );
  expect(JSON.parse(String(fetch.mock.calls[0]?.[1].body))).toEqual({
    host: "7440.board.example.com",
    path: "/inbox",
  });
});

test("a refusal shows, and the button works again", async () => {
  answer(400, { ok: false, error: { code: "usage", message: "no" } });
  render(<HandoffPage to="evil.example.com" />);
  fireEvent.click(screen.getByRole("button", { name: "Sign in there" }));
  expect((await screen.findByRole("alert")).textContent).toBe(
    "This daemon does not allow evil.example.com, so it cannot sign it in.",
  );
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Sign in there" }).disabled).toBe(
    false,
  );
});

test("a link with no board says so", () => {
  render(<HandoffPage />);
  expect(screen.getByRole("heading").textContent).toBe("This link names no board");
  expect(screen.queryByRole("button")).toBeNull();
});
