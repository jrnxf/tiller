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

interface MachineRow {
  id: string;
  kind: string;
  label: string;
}

// Settings with only this host paired. `pair` answers `POST /api/v1/peers`;
// once it answers 200 the machines list holds the new host.
function serveUnpaired(pair: () => Promise<[number, unknown]>, owner = true) {
  const fetch = serve(owner, undefined, false);
  const federated = fetch.getMockImplementation()!;
  const body = fixtureResource("/api/v1/machines", "federated") as { machines: MachineRow[] };
  const local = body.machines.filter((machine) => machine.kind === "local");
  const peer = body.machines.find((machine) => machine.label === "host-g")!;
  let machines = local;
  fetch.mockImplementation(async (input, init) => {
    const path = String(input);
    if (path === "/api/v1/machines") {
      return new Response(JSON.stringify({ ...body, machines }));
    }
    if (path === "/api/v1/peers" && init?.method === "POST") {
      const [status, answer] = await pair();
      if (status === 200) machines = [...local, { ...peer, id: "h-z", label: "host-z" }];
      return new Response(JSON.stringify(answer), { status });
    }
    return federated(input, init);
  });
  return fetch;
}

const PAIRED = {
  ok: true,
  replayed: false,
  events: [41, 42],
  receipt: { recorded_at: "2026-11-07T17:00:00Z", outbox_id: null, approval_id: null },
  data: { label: "host-z", ssh: "z.example.com", fingerprint: "SHA256:abc" },
};

async function openForm() {
  const panel = await screen.findByRole("region", { name: "Other hosts" });
  fireEvent.click(within(panel).getByRole("button", { name: "Add a host" }));
  const form = within(panel).getByRole("form", { name: "Add a host" });
  const field = (name: string) => within(form).getByLabelText<HTMLInputElement>(name);
  return { panel, form, field };
}

test("with no other host, Settings says none is paired and offers Add a host", async () => {
  serveUnpaired(async () => [200, PAIRED]);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Other hosts" });
  expect(within(panel).getByText("No other host is paired")).toBeTruthy();
  const row = within(panel).getByTestId("add-host");
  expect(row.lastElementChild).toBe(within(row).getByRole("button", { name: "Add a host" }));
  expect(within(panel).queryByText("host-d")).toBeNull();
});

test("a browser not signed in as the owner is told to sign in, with no Add a host", async () => {
  serveUnpaired(async () => [200, PAIRED], false);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Other hosts" });
  await within(panel).findByText("Sign this browser in as the owner to add one");
  expect(within(panel).getByText("No other host is paired")).toBeTruthy();
  expect(within(panel).queryByRole("button", { name: "Add a host" })).toBeNull();
});

test("Add a host opens a form that Pair arms only with a label and an alias, and Cancel closes", async () => {
  const fetch = serveUnpaired(async () => [200, PAIRED]);
  renderSettings();
  const { panel, form, field } = await openForm();
  expect(document.activeElement).toBe(field("Label"));
  const pair = within(form).getByRole<HTMLButtonElement>("button", { name: "Pair" });
  expect(pair.disabled).toBe(true);
  fireEvent.change(field("Label"), { target: { value: "host-z" } });
  expect(pair.disabled).toBe(true);
  fireEvent.change(field("SSH alias"), { target: { value: "z.example.com" } });
  expect(pair.disabled).toBe(false);
  expect(within(form).getByText("ssh z.example.com tiller peer accept")).toBeTruthy();

  fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));
  expect(within(panel).queryByRole("form")).toBeNull();
  expect(within(panel).getByRole("button", { name: "Add a host" })).toBeTruthy();
  expect(fetch).not.toHaveBeenCalledWith("/api/v1/peers", expect.anything());
});

test("Pair sends the pairing, shows its progress, and lists the new host", async () => {
  let answer: ((value: [number, unknown]) => void) | undefined;
  const fetch = serveUnpaired(() => new Promise((resolve) => (answer = resolve)));
  renderSettings();
  const { panel, form, field } = await openForm();
  fireEvent.change(field("Label"), { target: { value: " host-z " } });
  fireEvent.change(field("SSH alias"), { target: { value: "z.example.com" } });
  fireEvent.click(within(form).getByRole("button", { name: "Read only" }));
  fireEvent.click(within(form).getByRole("button", { name: "Pair" }));

  const progress = await within(form).findByRole("status");
  expect(progress.textContent).toBe("Pairing with host-z through ssh z.example.com...");
  expect(field("Label").disabled).toBe(true);
  expect(within(form).getByRole<HTMLButtonElement>("button", { name: "Cancel" }).disabled).toBe(
    true,
  );
  const call = fetch.mock.calls.find(([path]) => String(path) === "/api/v1/peers");
  const sent = JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
  expect(sent).toEqual({
    label: "host-z",
    ssh: "z.example.com",
    scope: "read",
    idempotency_key: expect.any(String),
  });

  answer?.([200, PAIRED]);
  expect(await within(panel).findByText("Paired host-z")).toBeTruthy();
  expect(within(panel).getByText(/Key SHA256:abc/)).toBeTruthy();
  await waitFor(() => expect(within(panel).getByText("host-z")).toBeTruthy());
  expect(within(panel).queryByRole("form")).toBeNull();
  expect(within(panel).queryByText("No other host is paired")).toBeNull();
});

test.each([
  [
    503,
    "peer_unreachable",
    "z.example.com is not reachable through ssh: Host key verification failed. (ssh exit 255)",
    "run ssh z.example.com once in a terminal on this host: it must connect with no password or question",
    "Run ssh z.example.com once in a terminal on this host: it must connect with no password or question",
  ],
  [
    502,
    "peer_refused",
    "z.example.com refuses pairing: its config has federation.accept = false",
    undefined,
    null,
  ],
  [
    422,
    "label_taken",
    "host-z is already paired; pick another label, or run tiller peer remove host-z in a terminal first",
    undefined,
    null,
  ],
])(
  "a %i %s pairing says why in plain words and keeps the form",
  async (status, code, message, hint, shown) => {
    serveUnpaired(async () => [status, { ok: false, error: { code, message, hint } }]);
    renderSettings();
    const { form, field } = await openForm();
    fireEvent.change(field("Label"), { target: { value: "host-z" } });
    fireEvent.change(field("SSH alias"), { target: { value: "z.example.com" } });
    fireEvent.click(within(form).getByRole("button", { name: "Pair" }));

    const alert = await within(form).findByRole("alert");
    const lines = [...alert.querySelectorAll("p")].map((line) => line.textContent);
    expect(lines).toEqual(shown ? [message, shown] : [message]);
    expect(field("Label").disabled).toBe(false);
    expect(field("Label").value).toBe("host-z");
    expect(within(form).getByRole<HTMLButtonElement>("button", { name: "Pair" }).disabled).toBe(
      false,
    );
  },
);
