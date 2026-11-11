import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { HostFields } from "../../lib/api-types";
import type { Scenario } from "../../lib/fixtures";
import { fixtureResource } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { signedOutStore } from "../../lib/session";
import { SettingsPage } from "./settings-page";
import { createTestQueryClient } from "../../test-app";

const GB = 1024;

function hostA(over: Partial<HostFields> = {}): HostFields {
  return {
    id: "01JHOSTA00000000000000000A",
    label: "host-a",
    boundary: "personal",
    boundary_recorded_by: null,
    lane_capacity: 5,
    ram_floor_mb: 8 * GB,
    disk_floor_mb: 20 * GB,
    lanes_busy: 3,
    ram_free_mb: 21 * GB + 310,
    disk_free_mb: 140 * GB + 520,
    rev: 4,
    ...over,
  };
}

interface Serve {
  scenario?: Scenario;
  owner?: boolean;
  host?: HostFields;
  // What a PATCH answers instead of a save: a status and its error.
  refuse?: { status: number; code: string; message: string; host?: HostFields };
}

// The fixture host, with `GET /api/v1/host` read from `host` and each
// `PATCH /api/v1/host` saved into it as the daemon saves it.
function serve({ scenario = "default", owner = true, host = hostA(), refuse }: Serve = {}) {
  const state = { host };
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") return new Response(JSON.stringify({ owner }));
    if (path === "/api/v1/host" && init?.method === "PATCH") {
      if (refuse) {
        if (refuse.host) state.host = refuse.host;
        const error = { code: refuse.code, message: refuse.message };
        return new Response(JSON.stringify({ ok: false, error }), { status: refuse.status });
      }
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      const { rev: _rev, idempotency_key: _key, ...edit } = body;
      state.host = { ...state.host, ...edit, rev: state.host.rev + 1 } as HostFields;
      return new Response(
        JSON.stringify({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: new Date().toISOString(), outbox_id: null, approval_id: null },
          data: { id: state.host.id, label: state.host.label, rev: state.host.rev },
        }),
      );
    }
    if (path === "/api/v1/host") return new Response(JSON.stringify(state.host));
    if (["/api/v1/health", "/api/v1/machines", "/api/v1/doctor"].includes(path)) {
      return new Response(JSON.stringify(fixtureResource(path, scenario)));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

const patches = (fetch: ReturnType<typeof serve>) =>
  fetch.mock.calls
    .filter(([path, init]) => String(path) === "/api/v1/host" && init?.method === "PATCH")
    .map(([, init]) => JSON.parse(String(init?.body)) as Record<string, unknown>);

async function openHostA() {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <SettingsPage />
    </QueryClientProvider>,
  );
  const panel = await screen.findByRole("region", { name: "Hosts" });
  const row = within(panel).getByTestId("host-host-a");
  fireEvent.click(within(row).getByRole("button", { name: "host-a" }));
  const card = await within(panel).findByTestId("host-card-host-a");
  await within(card).findByText("Now: 3 of 5 lanes busy, 21 GB RAM free, 140 GB disk free");
  return card;
}

// A section of the card, by its title.
const section = (card: HTMLElement, title: string) =>
  within(card).getByRole("region", { name: title });

const lanes = (scope: HTMLElement) => within(scope).getByRole("group", { name: "Lanes" });

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  signedOutStore.set(false);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("the host card shows its lanes and floors, and saves only what changed", async () => {
  const fetch = serve();
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  expect(within(lanes(capacity)).getByText("5")).toBeTruthy();
  const ram = within(capacity).getByLabelText<HTMLInputElement>("RAM floor");
  expect(ram.value).toBe("8");
  expect(within(capacity).getByLabelText<HTMLInputElement>("Disk floor").value).toBe("20");
  expect(capacity.textContent).toContain(
    "No new agent starts while free RAM or disk is under its floor.",
  );
  const save = within(capacity).getByRole<HTMLButtonElement>("button", { name: "Save" });
  const reset = within(capacity).getByRole<HTMLButtonElement>("button", { name: "Reset" });
  expect(save.disabled).toBe(true);
  expect(reset.disabled).toBe(true);
  // Save is the last button in the foot: the primary action on the right.
  expect(reset.compareDocumentPosition(save) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

  fireEvent.click(within(capacity).getByRole("button", { name: "One lane more" }));
  fireEvent.change(ram, { target: { value: "12" } });
  // "8.0" is the 8 GB the host keeps, so the disk floor has not changed.
  fireEvent.change(within(capacity).getByLabelText("Disk floor"), { target: { value: "20.0" } });
  expect(save.disabled).toBe(false);
  fireEvent.click(save);

  await within(capacity).findByText("Now: 3 of 6 lanes busy, 21 GB RAM free, 140 GB disk free");
  const [sent] = patches(fetch);
  expect(sent).toMatchObject({ lane_capacity: 6, ram_floor_mb: 12 * GB, rev: 4 });
  const keys = Object.keys(sent ?? {});
  keys.sort();
  expect(keys).toEqual(["idempotency_key", "lane_capacity", "ram_floor_mb", "rev"]);
  await waitFor(() =>
    expect(within(capacity).getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(
      true,
    ),
  );
});

test("Reset puts back the stored values, and an empty floor clears it", async () => {
  const fetch = serve();
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  const ram = within(capacity).getByLabelText<HTMLInputElement>("RAM floor");
  fireEvent.change(ram, { target: { value: "" } });
  expect(ram.placeholder).toBe("None");
  fireEvent.click(within(capacity).getByRole("button", { name: "Reset" }));
  expect(ram.value).toBe("8");

  fireEvent.change(ram, { target: { value: "" } });
  fireEvent.click(within(capacity).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patches(fetch)).toHaveLength(1));
  expect(patches(fetch)[0]).toMatchObject({ ram_floor_mb: null, rev: 4 });
  expect("disk_floor_mb" in (patches(fetch)[0] ?? {})).toBe(false);
});

test("a floor that is not GB cannot save", async () => {
  serve();
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  const ram = within(capacity).getByLabelText<HTMLInputElement>("RAM floor");
  fireEvent.change(ram, { target: { value: "lots" } });
  expect(ram.getAttribute("aria-invalid")).toBe("true");
  expect(within(capacity).getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(
    true,
  );
});

test("fewer lanes than busy ones says the agents keep running", async () => {
  serve();
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  const fewer = within(capacity).getByRole("button", { name: "One lane fewer" });
  fireEvent.click(fewer);
  fireEvent.click(fewer);
  expect(within(capacity).queryByTestId("lanes-over")).toBeNull();
  fireEvent.click(fewer);
  const warn = await within(capacity).findByTestId("lanes-over");
  expect(warn.textContent).toBe(
    "3 agents run on host-a now. They keep running. No new agent starts until 2 of them end.",
  );
  fireEvent.click(fewer);
  expect(within(capacity).getByTestId("lanes-over").textContent).toContain(
    "No new agent starts until 3 of them end.",
  );
  expect((fewer as HTMLButtonElement).disabled).toBe(true);
});

test("a boundary change asks first, and Keep sends nothing", async () => {
  const fetch = serve();
  const card = await openHostA();
  const identity = section(card, "Label and boundary");
  expect(identity.textContent).toContain(
    "Set at install. It can change until memory, a backup or an export records it.",
  );
  fireEvent.click(within(identity).getByRole("button", { name: "Work" }));
  fireEvent.click(within(identity).getByRole("button", { name: "Save" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the boundary to work?" });
  expect(document.activeElement).toBe(
    within(dialog).getByRole("button", { name: "Keep personal" }),
  );
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep personal" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(patches(fetch)).toHaveLength(0);

  fireEvent.click(within(identity).getByRole("button", { name: "Save" }));
  const again = await screen.findByRole("dialog", { name: "Change the boundary to work?" });
  fireEvent.click(within(again).getByRole("button", { name: "Change to work" }));
  await waitFor(() => expect(patches(fetch)).toHaveLength(1));
  expect(patches(fetch)[0]).toMatchObject({ boundary: "work", rev: 4 });
  expect("label" in (patches(fetch)[0] ?? {})).toBe(false);
  await waitFor(() =>
    expect(
      within(identity).getByRole("button", { name: "Work" }).getAttribute("aria-pressed"),
    ).toBe("true"),
  );
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("a boundary that memory records stays, and says why", async () => {
  serve({ host: hostA({ boundary_recorded_by: "memory" }) });
  const card = await openHostA();
  const identity = section(card, "Label and boundary");
  const work = within(identity).getByRole<HTMLButtonElement>("button", { name: "Work" });
  expect(work.disabled).toBe(true);
  expect(identity.textContent).toContain(
    "Memory already records the personal boundary, so it stays.",
  );
});

test("a label rename saves without asking, and a bad label cannot save", async () => {
  const fetch = serve();
  const card = await openHostA();
  const identity = section(card, "Label and boundary");
  const label = within(identity).getByLabelText<HTMLInputElement>("Label");
  fireEvent.change(label, { target: { value: "-bad" } });
  expect(identity.textContent).toContain("A letter or digit, then letters, digits, _ or -.");
  const save = within(identity).getByRole<HTMLButtonElement>("button", { name: "Save" });
  expect(save.disabled).toBe(true);
  fireEvent.change(label, { target: { value: "studio" } });
  fireEvent.click(save);
  await waitFor(() => expect(patches(fetch)).toHaveLength(1));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(patches(fetch)[0]).toMatchObject({ label: "studio", rev: 4 });
  expect("boundary" in (patches(fetch)[0] ?? {})).toBe(false);
});

test("paired peers keep the label: it says to unpair first", async () => {
  serve({ scenario: "federated" });
  const card = await openHostA();
  const identity = section(card, "Label and boundary");
  expect(within(identity).getByLabelText<HTMLInputElement>("Label").disabled).toBe(true);
  expect(identity.textContent).toMatch(
    /know this host by this label\. Unpair first to rename it\./,
  );
});

test("a save after another change says what moved and loads the new values", async () => {
  const moved = hostA({ lane_capacity: 4, rev: 5 });
  serve({
    refuse: {
      status: 409,
      code: "stale",
      message: "host host-a is at rev 5, you changed rev 4",
      host: moved,
    },
  });
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  fireEvent.change(within(capacity).getByLabelText("RAM floor"), { target: { value: "10" } });
  fireEvent.click(within(capacity).getByRole("button", { name: "Save" }));
  const alert = await within(capacity).findByRole("alert");
  await waitFor(() => expect(alert.textContent).toContain("Lanes is now 4."));
  expect(alert.textContent).toContain("Not saved: host-a changed after you opened it.");
  fireEvent.click(within(capacity).getByRole("button", { name: "Load new values" }));
  expect(within(lanes(capacity)).getByText("4")).toBeTruthy();
  // The owner's own change stays.
  expect(within(capacity).getByLabelText<HTMLInputElement>("RAM floor").value).toBe("10");
  expect(within(capacity).getByRole("button", { name: "Save" })).toBeTruthy();
});

test("a refused save shows the daemon's reason under the fields", async () => {
  serve({
    refuse: {
      status: 422,
      code: "refused",
      message: "a backup already records the personal boundary",
    },
  });
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  fireEvent.click(within(capacity).getByRole("button", { name: "One lane more" }));
  fireEvent.click(within(capacity).getByRole("button", { name: "Save" }));
  expect((await within(capacity).findByRole("alert")).textContent).toBe(
    "a backup already records the personal boundary",
  );
});

test("a viewer who is not the owner sees the values and no Save", async () => {
  serve({ owner: false });
  const card = await openHostA();
  const capacity = section(card, "Capacity");
  expect(within(capacity).getByText("Owner only")).toBeTruthy();
  expect(within(capacity).queryByRole("button", { name: "Save" })).toBeNull();
  expect(within(capacity).getByLabelText<HTMLInputElement>("RAM floor").disabled).toBe(true);
  const identity = section(card, "Label and boundary");
  expect(within(identity).getByLabelText<HTMLInputElement>("Label").disabled).toBe(true);
});
