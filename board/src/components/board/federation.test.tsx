import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import type { AgentsResponse } from "../../lib/api-types";
import { agentsPath } from "../../lib/queries";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { answeredStore, withdrawnStore } from "../attention/answered";
import { resetBoardLook } from "./board-prefs";

// The cards the `federated` fixture scenario gives each paired peer.
const GAMMA_12_D = "01JPCARDD00000000000000012";
const LEDGER_5_E = "01JPCARDE00000000000000005";
const ASK_D = "01JPATTN00000000000000000D";
const ASK_E = "01JPATTN00000000000000000E";
const ASK_F = "01JPATTN00000000000000000F";
const CAPACITY_D = "01JPATTN00000000000000002D";

interface Posted {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

// Answers the board API from the fixtures and the peer rules of the stand-in
// daemon (502, 503 and a one-time 504), and records every write.
function stubApi(scenario: Scenario) {
  const posted: Posted[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? "GET";
      if (method !== "GET") {
        const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        posted.push({ method, path, body });
        const { reply } = fixtureMutation(method, path, body);
        return new Response(JSON.stringify(reply.body), { status: reply.status });
      }
      const body = fixtureResource(path, scenario);
      return body === undefined
        ? new Response("{}", { status: 404 })
        : new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  return posted;
}

async function renderApp(scenario: Scenario, path = "/") {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, queryClient };
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  answeredStore.set({});
  withdrawnStore.set({});
  resetBoardLook();
  connectionStore.set("live");
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const flush = () => act(async () => {});

// ---- A host with no peers ----

test("a host with no peers shows no host chip, no peer note and no 'as of'", async () => {
  stubApi("default");
  await renderApp("default");
  expect(screen.getByTestId("board")).toBeTruthy();
  expect(screen.queryAllByTestId("host-chip")).toHaveLength(0);
  expect(screen.queryAllByTestId("peer-note")).toHaveLength(0);
  expect(screen.queryByText(/as of/)).toBeNull();
});

// ---- The board ----

test("with peers in the snapshot the board is on screen at once, no spinner", async () => {
  stubApi("federated");
  await renderApp("federated");
  expect(screen.getByTestId("board")).toBeTruthy();
  expect(screen.queryByRole("progressbar")).toBeNull();
  expect(screen.queryByText(/loading/i)).toBeNull();
});

test("each peer project is a lane of its own, even when its slug is one this host uses", async () => {
  stubApi("federated");
  await renderApp("federated");
  expect(screen.getByTestId("lane-gamma")).toBeTruthy();
  expect(screen.getByTestId("lane-host-d/gamma")).toBeTruthy();
  expect(screen.getByTestId("lane-host-e/ledger")).toBeTruthy();
  expect(screen.getByTestId("lane-host-f/atlas")).toBeTruthy();
});

test("every card carries the chip of the host that owns it", async () => {
  stubApi("federated");
  await renderApp("federated");
  const card = screen.getByText("Cache the release manifest").closest<HTMLElement>(".card");
  expect(within(card as HTMLElement).getByTestId("host-chip").textContent).toBe("host-d");
  const own = screen
    .getByText("Add the machines summary to the sidebar")
    .closest<HTMLElement>(".card");
  expect(within(own as HTMLElement).getByTestId("host-chip").textContent).toBe("host-a");
});

test("a peer that is down keeps its lane as of the last frame, dimmed, with the reason", async () => {
  stubApi("federated");
  await renderApp("federated");
  const lane = screen.getByTestId("lane-host-e/ledger");
  const note = within(lane).getByTestId("peer-note");
  expect(note.textContent).toMatch(/as of \w{3} \d{2}:\d{2}, 6 min ago/);
  expect(note.textContent).toContain("host-e is unreachable since");
  const chip = within(lane).getAllByTestId("host-chip")[0] as HTMLElement;
  expect(chip.className).toContain("off");
});

test("a live peer's lane has no note, and a read pairing's says it is view only", async () => {
  stubApi("federated");
  await renderApp("federated");
  expect(within(screen.getByTestId("lane-host-d/gamma")).queryByTestId("peer-note")).toBeNull();
  const note = within(screen.getByTestId("lane-host-f/atlas")).getByTestId("peer-note");
  expect(note.textContent).toBe("view only pairing.");
});

// ---- The Machines section ----

test("Machines shows each peer's link state, the refusal and the view-only pairing", async () => {
  stubApi("federated");
  await renderApp("federated");
  expect(screen.getByTestId("link-host-e").textContent).toMatch(
    /^unreachable since \w{3} \d{2}:\d{2}$/,
  );
  expect(screen.getByTestId("link-host-g").textContent).toBe("refused: key revoked");
  const viewOnly = screen.getByTestId("machine-host-f");
  expect(viewOnly.textContent).toContain("peer");
  expect(viewOnly.textContent).toContain("view only");
  expect(screen.queryByTestId("link-host-d")).toBeNull();
});

// ---- Card page controls ----

test("the card page of a peer's card names its host and, when it is down, says as of when", async () => {
  stubApi("federated");
  await renderApp("federated", `/cards/${LEDGER_5_E}?owner=host-e`);
  const page = await screen.findByTestId("card-page");
  expect(within(page).getAllByTestId("host-chip")[0]?.textContent).toBe("host-e");
  expect(within(page).getAllByText(/as of \w{3} \d{2}:\d{2}/).length).toBeGreaterThan(0);
});

test("a card of a down peer shows the reason and every control is off", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_E}`);
  const actions = await screen.findByTestId("card-actions");
  expect(within(actions).getByTestId("peer-note").textContent).toContain("host-e is unreachable");
  expect((within(actions).getByLabelText("Priority") as HTMLSelectElement).disabled).toBe(true);
  const comment = within(actions).getByLabelText("Comment to the orchestrator") as HTMLInputElement;
  expect(comment.disabled).toBe(true);
  expect(comment.title).toContain("host-e is unreachable");
  expect(
    (within(actions).getByRole("button", { name: "Add card" }) as HTMLButtonElement).disabled,
  ).toBe(true);
  expect(posted).toHaveLength(0);
});

test("a read pairing's card is live but its controls say 'view only pairing'", async () => {
  stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_F}`);
  const actions = await screen.findByTestId("card-actions");
  expect(within(actions).getByTestId("peer-note").textContent).toBe("view only pairing.");
  const comment = within(actions).getByLabelText("Comment to the orchestrator") as HTMLInputElement;
  expect(comment.disabled).toBe(true);
  expect(comment.title).toBe("view only pairing");
});

test("a card of a live act peer is sent to that host and reads like a local one", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_D}`);
  const actions = await screen.findByTestId("card-actions");
  expect(within(actions).queryByTestId("peer-note")).toBeNull();
  const input = within(actions).getByLabelText("Comment to the orchestrator");
  fireEvent.change(input, { target: { value: "ship it" } });
  fireEvent.click(within(actions).getByRole("button", { name: /Send/ }));
  await flush();
  expect(posted).toHaveLength(1);
  expect(posted[0]?.path).toBe(`/api/v1/cards/${GAMMA_12_D}/comment`);
  expect(posted[0]?.body.host).toBe("host-d");
});

test("this host's own card sends no host field", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", "/inbox?item=01JATTN0000000000000000000");
  const actions = await screen.findByTestId("card-actions");
  fireEvent.change(within(actions).getByLabelText("Comment to the orchestrator"), {
    target: { value: "hello" },
  });
  fireEvent.click(within(actions).getByRole("button", { name: /Send/ }));
  await flush();
  expect(posted).toHaveLength(1);
  expect("host" in (posted[0]?.body ?? {})).toBe(false);
});

test("the card page's comment goes to the card's host, and is off for a down peer", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/cards/${GAMMA_12_D}?owner=host-d`);
  const page = await screen.findByTestId("card-page");
  fireEvent.change(within(page).getByLabelText("Comment to the orchestrator"), {
    target: { value: "ship it" },
  });
  fireEvent.click(within(page).getByRole("button", { name: /Send/ }));
  await flush();
  expect(posted).toHaveLength(1);
  expect(posted[0]?.path).toBe(`/api/v1/cards/${GAMMA_12_D}/comment`);
  expect(posted[0]?.body.host).toBe("host-d");
  cleanup();

  await renderApp("federated", `/cards/${LEDGER_5_E}?owner=host-e`);
  const down = await screen.findByTestId("card-page");
  const comment = within(down).getByLabelText("Comment to the orchestrator") as HTMLInputElement;
  expect(comment.disabled).toBe(true);
  expect(comment.title).toContain("host-e is unreachable");
});

test("a forward the peer did not confirm reads 'Sent, not confirmed' and a retry keeps its key", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_D}`);
  const actions = await screen.findByTestId("card-actions");
  fireEvent.change(within(actions).getByLabelText("Comment to the orchestrator"), {
    target: { value: "uncertain note" },
  });
  const send = within(actions).getByRole("button", { name: /Send/ });
  fireEvent.click(send);
  await flush();

  const notice = within(actions).getByTestId("sent-not-confirmed");
  expect(notice.textContent).toContain("Sent, not confirmed.");
  expect(notice.textContent).toContain("host-d");
  expect(within(actions).queryByText(/did not confirm in time/)).toBeNull();

  fireEvent.click(within(actions).getByRole("button", { name: /Send/ }));
  await flush();
  expect(posted).toHaveLength(2);
  expect(posted[1]?.body.idempotency_key).toBe(posted[0]?.body.idempotency_key);
  expect(within(actions).queryByTestId("sent-not-confirmed")).toBeNull();
});

test("the new-card picker groups projects by host and turns off a group that cannot act", async () => {
  stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_D}`);
  const actions = await screen.findByTestId("card-actions");
  fireEvent.click(within(actions).getByRole("button", { name: "Add card" }));
  const picker = within(actions).getByLabelText("Project") as HTMLSelectElement;
  const groups = [...picker.querySelectorAll("optgroup")];
  expect(groups.map((group) => group.label)).toEqual([
    "host-a",
    "host-d",
    expect.stringContaining("host-e - host-e is unreachable"),
    "host-f - view only pairing",
  ]);
  expect(groups.map((group) => group.disabled)).toEqual([false, false, true, true]);
  // The card's own project is chosen, and it lives on host-d.
  expect(picker.value).toBe("host-d/gamma");
});

// ---- Attention and agents ----

test("an ask names its host, and a display id two hosts use is host-qualified", async () => {
  stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_D}`);
  const row = await screen.findByTestId(`inbox-row-${ASK_D}`);
  expect(within(row).getByTestId("host-chip").textContent).toBe("host-d");
  expect(row.textContent).toContain("host-d · gamma-12");
  expect(screen.getByTestId("inbox").textContent).not.toContain("host-d · fleet-3");
});

test("an ask of a read pairing cannot be answered and says why", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/inbox?item=${ASK_F}`);
  const detail = await screen.findByRole("heading", { name: /Which tile size/ });
  expect(detail).toBeTruthy();
  expect(
    screen.getAllByTestId("peer-note").every((n) => n.textContent === "view only pairing."),
  ).toBe(true);
  for (const option of within(screen.getByRole("group", { name: "Answer" })).getAllByRole(
    "button",
  )) {
    expect((option as HTMLButtonElement).disabled).toBe(true);
  }
  expect(posted).toHaveLength(0);
});

test("an option that changes a peer's project is picked on that host's board", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/inbox?item=${CAPACITY_D}`);
  const group = await screen.findByRole("group", { name: "Answer" });
  const raise = within(group).getByRole("button", { name: /Raise to 3/ }) as HTMLButtonElement;
  expect(raise.disabled).toBe(true);
  expect(screen.getByText(/pick it on host-d's board/)).toBeTruthy();
  fireEvent.keyDown(window, { key: "1" });
  await flush();
  expect(posted).toHaveLength(0);
  fireEvent.click(within(group).getByRole("button", { name: /Keep 2/ }));
  await flush();
  expect(posted).toHaveLength(1);
  expect(posted[0]?.body).toMatchObject({ answer: "Keep 2", host: "host-d" });
});

test("the Agents screen shows the host of every agent, and a peer's pane cannot be opened from here", async () => {
  stubApi("federated");
  const agents = (fixtureSnapshot("federated").queries[agentsPath()] as AgentsResponse)
    .orchestrators;
  const worker = agents.flatMap((a) => a.workers).find((w) => w.host === "host-d");
  if (!worker) throw new Error("no host-d worker in the fixture");
  await renderApp("federated", `/agents?agent=${worker.session_id}`);
  const hosts = new Set(
    (await screen.findAllByTestId("host-chip")).map((chip) => chip.textContent),
  );
  expect(hosts).toEqual(new Set(["host-a", "host-d", "host-e", "host-f"]));
  const detail = document.querySelector<HTMLElement>(".agd");
  if (!detail) throw new Error("no agent detail on screen");
  const open = within(detail).getByRole("button", { name: /Open in herdr/ }) as HTMLButtonElement;
  expect(open.disabled).toBe(true);
  expect(open.title).toBe("host-d owns this pane: open it from that host");
});

test("a link to another screen leaves the card page's owner behind", async () => {
  stubApi("federated");
  await renderApp("federated", `/cards/${LEDGER_5_E}?owner=host-e`);
  await screen.findByTestId("card-page");
  const links = within(screen.getByRole("navigation", { name: "Screens" })).getAllByRole("link");
  expect(links.length).toBeGreaterThan(0);
  for (const link of links) expect(link.getAttribute("href")).not.toContain("owner=");
});
