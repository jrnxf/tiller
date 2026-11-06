import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { loadPrefs, prefsStore } from "../../lib/prefs";
import { SignedOutScreen } from "./signed-out-screen";

function answers(...replies: [number, unknown][]) {
  const fetch = vi.fn(async (_path: string, _init: RequestInit) => {
    const [status, body] = replies.shift() ?? [500, {}];
    return new Response(JSON.stringify(body), { status });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function field() {
  return screen.getByRole<HTMLInputElement>("textbox", { name: "Sign-in code" });
}

function sent(fetch: ReturnType<typeof answers>): string[] {
  return fetch.mock.calls.map(([, init]) => String(init.body));
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("the screen is one numeric field, focused, that asks for the printed code", () => {
  render(<SignedOutScreen onSignedIn={() => {}} />);
  const input = field();
  expect(input.inputMode).toBe("numeric");
  expect(input.autocomplete).toBe("one-time-code");
  expect(document.activeElement).toBe(input);
  expect(screen.getByRole("heading", { name: "Sign in to this board" })).toBeTruthy();
  expect(screen.getByText("tiller board --login")).toBeTruthy();
  expect(screen.getAllByRole("textbox")).toHaveLength(1);
});

test("the code is sent as the eighth digit lands, and not before", async () => {
  const fetch = answers([200, { owner: true }]);
  const onSignedIn = vi.fn();
  render(<SignedOutScreen onSignedIn={onSignedIn} />);

  fireEvent.change(field(), { target: { value: "1234567" } });
  expect(fetch).not.toHaveBeenCalled();
  fireEvent.change(field(), { target: { value: "12345678" } });

  await waitFor(() => expect(onSignedIn).toHaveBeenCalledTimes(1));
  expect(sent(fetch)).toEqual(['{"code":"12345678"}']);
});

test("a pasted code with a space or dash is read as its eight digits", async () => {
  const fetch = answers([200, { owner: true }]);
  const onSignedIn = vi.fn();
  render(<SignedOutScreen onSignedIn={onSignedIn} />);

  fireEvent.change(field(), { target: { value: "1234 5678" } });
  await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
  expect(sent(fetch)).toEqual(['{"code":"12345678"}']);
});

test("a wrong code clears the field and says so in plain text, and the next try goes out", async () => {
  const fetch = answers(
    [403, { ok: false, error: { code: "wrong_code", message: "wrong" } }],
    [200, { owner: true }],
  );
  const onSignedIn = vi.fn();
  render(<SignedOutScreen onSignedIn={onSignedIn} />);

  fireEvent.change(field(), { target: { value: "00000000" } });
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(
    "That code is wrong or has expired. Run tiller board --login again for a new code.",
  );
  expect(alert.querySelector("code")?.textContent).toBe("tiller board --login");
  expect(field().value).toBe("");
  expect(field().getAttribute("aria-invalid")).toBe("true");
  expect(onSignedIn).not.toHaveBeenCalled();

  fireEvent.change(field(), { target: { value: "1" } });
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.change(field(), { target: { value: "12345678" } });
  await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
  expect(fetch).toHaveBeenCalledTimes(2);
});
