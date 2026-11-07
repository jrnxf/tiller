import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { fixtureResource } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { signedOutStore } from "../../lib/session";
import { SettingsPage } from "./settings-page";

// `gated`: an `--allow-host` name, where reads need the cookie.
function serve(owner: boolean, logout: [number, unknown] = [200, { owner: false }], gated = true) {
  let signedIn = owner;
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") {
      if (!signedIn && gated) {
        const refused = { ok: false, error: { code: "signed_out", message: "signed out" } };
        return new Response(JSON.stringify(refused), { status: 403 });
      }
      return new Response(JSON.stringify({ owner: signedIn }));
    }
    if (path === "/api/v1/logout" && init?.method === "POST") {
      if (logout[0] === 200) signedIn = false;
      return new Response(JSON.stringify(logout[1]), { status: logout[0] });
    }
    if (path === "/api/v1/health" || path === "/api/v1/machines") {
      return new Response(JSON.stringify(fixtureResource(path, "federated")));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function renderSettings() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <SettingsPage />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  signedOutStore.set(false);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("a signed-in browser has Sign out at the right of its row, and it ends the sign-in", async () => {
  const fetch = serve(true);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "This browser" });
  const row = within(panel).getByText("Signed in as the owner").closest(".set-row");
  const button = within(panel).getByRole("button", { name: "Sign out" });
  expect(row?.lastElementChild).toBe(button);

  fireEvent.click(button);
  await waitFor(() => expect(signedOutStore.get()).toBe(true));
  expect(fetch).toHaveBeenCalledWith("/api/v1/logout", expect.objectContaining({ method: "POST" }));
});

test("on loopback, signing out keeps the board and shows this browser signed out", async () => {
  serve(true, undefined, false);
  renderSettings();
  fireEvent.click(await screen.findByRole("button", { name: "Sign out" }));
  const panel = screen.getByRole("region", { name: "This browser" });
  expect(await within(panel).findByText("Not signed in")).toBeTruthy();
  expect(signedOutStore.get()).toBe(false);
});

test("a refused sign-out stays on the page and says why", async () => {
  serve(true, [403, { ok: false, error: { code: "not_owner", message: "no" } }]);
  renderSettings();
  fireEvent.click(await screen.findByRole("button", { name: "Sign out" }));
  expect((await screen.findByRole("alert")).textContent).toBe(
    "The daemon answered 403. Try again.",
  );
  expect(signedOutStore.get()).toBe(false);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Sign out" }).disabled).toBe(false);
});

test("on loopback, a browser that is not signed in has nothing to sign out", async () => {
  serve(false, undefined, false);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "This browser" });
  expect(within(panel).getByText("Not signed in")).toBeTruthy();
  expect(within(panel).queryByRole("button")).toBeNull();
});

test("Settings lists each other host's version and marks the ones that differ from this host", async () => {
  serve(true);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Other hosts" });
  const row = (label: string) => within(panel).getByText(label).closest(".set-row") as HTMLElement;

  // Same version: no mark.
  expect(within(row("host-d")).getByText("0.1.0-pre.17")).toBeTruthy();
  expect(within(row("host-d")).getByText("Same as this host")).toBeTruthy();
  expect(row("host-d").querySelector(".set-status")).toBeNull();

  // Older and newer, in plain words, beside the version.
  expect(within(row("host-e")).getByText("0.1.0-pre.15")).toBeTruthy();
  expect(within(row("host-e")).getByText("Older than this host (0.1.0-pre.17)")).toBeTruthy();
  expect(within(row("host-e")).getByText("older")).toBeTruthy();
  expect(within(row("host-f")).getByText("Newer than this host (0.1.0-pre.17)")).toBeTruthy();

  // No report: no version and no guess.
  expect(within(row("host-g")).getByText("Not reported")).toBeTruthy();
  expect(within(row("host-g")).getByText("Paired peer, has not reported a version")).toBeTruthy();
  expect(within(row("host-b")).getByText("herdr machine, no version to read")).toBeTruthy();

  // This host keeps its own facts card with the same version.
  const facts = screen.getByTestId("host-facts");
  expect(within(facts).getByText("0.1.0-pre.17")).toBeTruthy();
});
