import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { DaemonHealth } from "../../lib/api-types";
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
  const row = (await within(panel).findByText("Signed in as the owner")).closest(".set-row");
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
  expect(await within(panel).findByText("Not signed in")).toBeTruthy();
  expect(within(panel).queryByRole("button", { name: "Sign out" })).toBeNull();
});

test("Settings lists every host in one table, this one a row like the others", async () => {
  serve(true);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Hosts" });
  const row = (label: string) => within(panel).getByTestId(`host-${label}`);
  const cell = (label: string, column: string) =>
    row(label).querySelector(`[data-label="${column}"]`) as HTMLElement;

  // No card of its own: the host serving the board is a row, sorted by label.
  expect(screen.queryByTestId("host-facts")).toBeNull();
  expect(screen.queryByText("This host")).toBeNull();
  const labels = [...panel.querySelectorAll("tbody th")].map((th) => th.textContent);
  const sorted = [...labels];
  sorted.sort((a, b) => (a ?? "").localeCompare(b ?? ""));
  expect(labels).toEqual(sorted);
  expect([...panel.querySelectorAll("thead th")].map((th) => th.textContent)).toEqual([
    "Host",
    "Version",
    "Started",
    "Process",
    "Status",
  ]);

  // Its row has the start and the process; a peer reports neither yet.
  expect(within(row("host-a")).getByText("0.1.0-pre.17")).toBeTruthy();
  expect(cell("host-a", "Process").textContent).toBe("4242");
  expect(cell("host-a", "Started").textContent).toMatch(/^\w{3} \d{1,2} \w{3}, \d{2}:\d{2}up 3 h/);
  expect(within(row("host-a")).getByText("Serves this board")).toBeTruthy();
  expect(cell("host-a", "Version").querySelector("small")?.textContent).toMatch(/^Schema \d+$/);
  expect(cell("host-d", "Version").querySelector("small")).toBeNull();
  expect(cell("host-d", "Process").textContent).toBe("-");
  expect(cell("host-d", "Started").dataset.empty).toBe("true");

  // Same version: no mark.
  expect(within(row("host-d")).getByText("0.1.0-pre.17")).toBeTruthy();
  expect(within(row("host-d")).getByText("Same as host-a")).toBeTruthy();
  expect(row("host-d").querySelector(".set-status")).toBeNull();

  // Older and newer, in plain words, against the serving host by its label.
  expect(within(row("host-e")).getByText("0.1.0-pre.15")).toBeTruthy();
  expect(within(row("host-e")).getByText("older")).toBeTruthy();
  expect(within(row("host-f")).getByText("Newer than host-a (0.1.0-pre.17)")).toBeTruthy();

  // No report: no version and no guess.
  expect(within(row("host-g")).getByText("Not reported")).toBeTruthy();
  expect(within(row("host-b")).getByText("herdr machine, no version to read")).toBeTruthy();
});

test("without the hosts list, the host serving this board still has its row", async () => {
  serve(true);
  const served = globalThis.fetch;
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) =>
    String(input) === "/api/v1/machines"
      ? new Response("{}", { status: 500 })
      : served(input, init),
  );
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Hosts" });
  const row = await within(panel).findByTestId("host-This host");
  const health = fixtureResource("/api/v1/health", "federated") as DaemonHealth;
  expect(panel.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(within(row).getByText(health.version)).toBeTruthy();
  expect(within(row).getByText(`Schema ${health.schema}`)).toBeTruthy();
  expect(within(row).getByText(String(health.pid))).toBeTruthy();
  expect(within(row).getByText("Serves this board")).toBeTruthy();
  // Whether another host is paired is not known yet.
  expect(within(panel).queryByTestId("no-hosts")).toBeNull();
});

test("a merge App that is not ready is a section above the hosts, named by its host", async () => {
  serve(true);
  renderSettings();
  const section = await screen.findByTestId("merge-app");
  expect(section.getAttribute("aria-label")).toBe("Merge App");
  await waitFor(() => expect(within(section).getByText("host-a")).toBeTruthy());
  expect(within(section).getByTestId("merge-app-status").textContent).toBe("Not set up");
  expect(within(section).getByText(/^Create the merge App and put its app-id/)).toBeTruthy();
  const page = screen.getByTestId("settings");
  expect(page.firstElementChild).toBe(section);
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
  const panel = await screen.findByRole("region", { name: "Hosts" });
  fireEvent.click(within(panel).getByRole("button", { name: "Add a host" }));
  const form = within(panel).getByRole("form", { name: "Add a host" });
  const field = (name: string) => within(form).getByLabelText<HTMLInputElement>(name);
  return { panel, form, field };
}

test("with no other host, Hosts lists this one, says none is paired and offers Add a host", async () => {
  serveUnpaired(async () => [200, PAIRED]);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Hosts" });
  expect(within(panel).getByText("No other host is paired")).toBeTruthy();
  expect(within(panel).getByTestId("host-host-a")).toBeTruthy();
  const head = panel.querySelector(".set-sec-h") as HTMLElement;
  expect(head.lastElementChild).toBe(within(head).getByRole("button", { name: "Add a host" }));
  expect(within(panel).queryByText("host-d")).toBeNull();
});

test("a browser not signed in as the owner is told to sign in, with no Add a host", async () => {
  serveUnpaired(async () => [200, PAIRED], false);
  renderSettings();
  const panel = await screen.findByRole("region", { name: "Hosts" });
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

// The start settings of this host, served beside the rest.
function serveSettings(owner: boolean) {
  let pool: string | null = null;
  const posts: Record<string, unknown>[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") return new Response(JSON.stringify({ owner }));
    if (path === "/api/v1/start-settings") {
      return new Response(
        JSON.stringify({
          pool_root: pool,
          checkouts_root: "/home/user/src",
          orchestrator_home: "/home/user/.tiller/orchestrators",
          scratch_root: "/home/user/.tiller/scratch",
          problems: pool
            ? []
            : [{ key: "spawn.pool_root", kind: "missing", problem: "spawn.pool_root is not set" }],
          config_error: null,
        }),
      );
    }
    if (path === "/api/v1/config" && init?.method === "POST") {
      const body = JSON.parse(String(init.body));
      posts.push(body);
      if (body.value === "relative") {
        const error = { code: "usage", message: "spawn.pool_root must be absolute, not relative" };
        return new Response(JSON.stringify({ ok: false, error }), { status: 400 });
      }
      pool = body.value;
      return new Response(
        JSON.stringify({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
          data: { key: body.key, value: body.value, changed: true },
        }),
      );
    }
    if (path === "/api/v1/health" || path === "/api/v1/machines") {
      return new Response(JSON.stringify(fixtureResource(path, "federated")));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
  return posts;
}

test("the owner sets a start folder from Settings and a refusal says why", async () => {
  const posts = serveSettings(true);
  renderSettings();
  const section = within(await screen.findByRole("region", { name: "Starting agents" }));
  const pool = within(section.getByTestId("setting-spawn.pool_root"));
  expect(pool.getByText("spawn.pool_root is not set")).toBeTruthy();
  // The roots follow from these two, so they are shown and cannot be edited.
  expect(section.getByText("/home/user/.tiller/scratch")).toBeTruthy();

  fireEvent.change(pool.getByRole("textbox"), { target: { value: "relative" } });
  fireEvent.click(pool.getByRole("button", { name: "Save" }));
  expect((await pool.findByRole("alert")).textContent).toContain("must be absolute");

  fireEvent.change(pool.getByRole("textbox"), { target: { value: "/home/user/pool" } });
  fireEvent.click(pool.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(pool.queryByText("spawn.pool_root is not set")).toBeNull());
  expect(posts.map((post) => post.value)).toEqual(["relative", "/home/user/pool"]);
});

test("a browser that is not the owner reads the start folders and cannot change them", async () => {
  serveSettings(false);
  renderSettings();
  const section = within(await screen.findByRole("region", { name: "Starting agents" }));
  expect(section.getByText("/home/user/src")).toBeTruthy();
  expect(section.getByText("Not set")).toBeTruthy();
  expect(section.queryAllByRole("textbox")).toHaveLength(0);
});
