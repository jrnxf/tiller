import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { DaemonHealth, PollerHealth } from "../../lib/api-types";
import type { Scenario } from "../../lib/fixtures";
import { fixtureResource } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { signedOutStore } from "../../lib/session";
import { SettingsPage } from "./settings-page";
import { createTestQueryClient } from "../../test-app";

// The fixture host's health, doctor report and hosts list. Run now on a
// poller answers with that poller polled, and health lists it so after.
function serve(scenario: Scenario, { owner = true, doctor = true } = {}) {
  const ran = new Map<string, PollerHealth>();
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") return new Response(JSON.stringify({ owner }));
    if (path === "/api/v1/doctor" && !doctor) {
      const error = { code: "unavailable", message: "the checks are not wired" };
      return new Response(JSON.stringify({ ok: false, error }), { status: 503 });
    }
    const run = /^\/api\/v1\/pollers\/([^/]+)\/run$/.exec(path);
    if (run && init?.method === "POST") {
      const source = decodeURIComponent(run[1] ?? "");
      const at = new Date().toISOString();
      const after = {
        source,
        last_attempt_at: at,
        last_success_at: at,
        consecutive_failures: 0,
        last_error: null,
      };
      ran.set(source, after);
      return new Response(
        JSON.stringify({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: at, outbox_id: null, approval_id: null },
          data: after,
        }),
      );
    }
    if (path === "/api/v1/health") {
      const health = fixtureResource(path, scenario) as DaemonHealth;
      const pollers = health.pollers.map((poller) => ran.get(poller.source) ?? poller);
      return new Response(JSON.stringify({ ...health, pollers }));
    }
    if (path === "/api/v1/machines" || path === "/api/v1/doctor") {
      return new Response(JSON.stringify(fixtureResource(path, scenario)));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function renderSettings() {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <SettingsPage />
    </QueryClientProvider>,
  );
}

async function openHost(label: string) {
  const panel = await screen.findByRole("region", { name: "Hosts" });
  const toggle = within(within(panel).getByTestId(`host-${label}`)).getByRole("button", {
    name: label,
  });
  fireEvent.click(toggle);
  return { toggle, card: await within(panel).findByTestId(`host-card-${label}`) };
}

const calls = (fetch: ReturnType<typeof serve>, path: string) =>
  fetch.mock.calls.filter(([called]) => String(called) === path).length;

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  signedOutStore.set(false);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("the host row says how many checks need you, and opens into a card with them", async () => {
  serve("default");
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Hosts" });
  const row = within(panel).getByTestId("host-host-a");
  expect((await within(row).findByTestId("host-needs-you")).textContent).toBe("1 check needs you");
  expect(within(panel).queryByTestId("host-card-host-a")).toBeNull();

  const { toggle, card } = await openHost("host-a");
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  expect(toggle.getAttribute("aria-controls")).toBe(card.id);
  const reconcile = await within(card).findByTestId("check-poller:reconcile");
  expect(reconcile.textContent).toContain("5 errors in a row");
  expect(within(reconcile).getByRole("button", { name: "Run now" })).toBeTruthy();
  expect(within(card).getByTestId("check-tools").textContent).toContain("All found");
  expect(within(card).getByTestId("check-check:owner marker")).toBeTruthy();
  expect(card.textContent).toMatch(/Last run \w{3} \d{1,2} \w{3}, \d{2}:\d{2}/);
  expect(card.textContent).not.toMatch(/firstmate/i);

  fireEvent.click(toggle);
  expect(within(panel).queryByTestId("host-card-host-a")).toBeNull();
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
});

test("Run now polls the failing poller, and its line and the row count say it is fixed", async () => {
  const fetch = serve("default");
  renderSettings();
  const { card } = await openHost("host-a");
  const reconcile = await within(card).findByTestId("check-poller:reconcile");
  fireEvent.click(within(reconcile).getByRole("button", { name: "Run now" }));

  await waitFor(() =>
    expect(within(card).getByTestId("check-poller:reconcile").textContent).toContain("Fixed"),
  );
  const call = fetch.mock.calls.find(([path]) => String(path) === "/api/v1/pollers/reconcile/run");
  expect(call?.[1]?.method).toBe("POST");
  expect(JSON.parse(String(call?.[1]?.body))).toEqual({ idempotency_key: expect.any(String) });
  const row = screen.getByTestId("host-host-a");
  await waitFor(() => expect(within(row).queryByTestId("host-needs-you")).toBeNull());
});

test("Check again runs the checks once more", async () => {
  const fetch = serve("default");
  renderSettings();
  const { card } = await openHost("host-a");
  await within(card).findByTestId("check-tools");
  const before = calls(fetch, "/api/v1/doctor");
  fireEvent.click(within(card).getByRole("button", { name: "Check again" }));
  await waitFor(() => expect(calls(fetch, "/api/v1/doctor")).toBe(before + 1));
});

test("a browser that is not the owner reads the checks and cannot run a poller", async () => {
  serve("default", { owner: false });
  renderSettings();
  const { card } = await openHost("host-a");
  const reconcile = await within(card).findByTestId("check-poller:reconcile");
  expect(within(reconcile).queryByRole("button", { name: "Run now" })).toBeNull();
  expect(within(reconcile).getByText("Owner only")).toBeTruthy();
});

test("checks that cannot be run from the board say so and point to the shell", async () => {
  serve("default", { doctor: false });
  renderSettings();
  const { card } = await openHost("host-a");
  const line = await within(card).findByTestId("check-unreadable");
  expect(line.textContent).toContain("Run tiller doctor in a shell on this host.");
  // The pollers still read from health.
  expect(within(card).getByTestId("check-poller:reconcile")).toBeTruthy();
});

test("a peer's card points to its own board, and a herdr machine's to this host", async () => {
  serve("federated");
  renderSettings();
  const peer = (await openHost("host-d")).card;
  expect(within(peer).getByTestId("check-elsewhere").textContent).toContain(
    "Open the board host-d serves to see its checks.",
  );
  const herdr = (await openHost("host-b")).card;
  expect(within(herdr).getByTestId("check-elsewhere").textContent).toBe(
    "No daemon to checkhost-b runs agents through herdr only. Its herdr poller is in the checks of host-a.",
  );
  // A peer's own checks are not this board's to count.
  expect(within(screen.getByTestId("host-host-d")).queryByTestId("host-needs-you")).toBeNull();
});

test("the Merge App problem shows once, in its own section, not again in the host card", async () => {
  serve("default");
  renderSettings();
  const { card } = await openHost("host-a");
  await within(card).findByTestId("check-check:config drift");
  expect(within(card).queryByTestId("check-check:merge app")).toBeNull();
  expect(within(card).queryByTestId("check-merge-gates")).toBeNull();
  expect(screen.getAllByTestId("merge-app")).toHaveLength(1);
});
