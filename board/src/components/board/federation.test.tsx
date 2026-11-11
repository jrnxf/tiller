import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import type { AgentsResponse } from "../../lib/api-types";
import { agentsPath, queryKeyFor } from "../../lib/queries";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { answeredStore, withdrawnStore } from "../attention/answered";
import { choose } from "../../test-select";
import { resetBoardLook } from "./board-prefs";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

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
  const queryClient = createTestQueryClient();
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
    // xterm still listens the old way, for the device pixel ratio. A peer's
    // pane mounts a real terminal here, and a throw would unmount the app.
    addListener: () => {},
    removeListener: () => {},
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

// The Start orchestrator in a lane's header.
const startIn = (lane: string) =>
  within(screen.getByTestId(lane).querySelector(".lane-h") as HTMLElement).getByRole("button", {
    name: "Start orchestrator",
  }) as HTMLButtonElement;

// The federated board with no peer orchestrator running, so each peer lane
// offers Start orchestrator.
async function renderWithoutPeerOrchestrators() {
  const agents = fixtureSnapshot("federated").queries[agentsPath()] as AgentsResponse;
  const { queryClient } = await renderApp("federated");
  queryClient.setQueryData(queryKeyFor(agentsPath()), {
    ...agents,
    orchestrators: agents.orchestrators.filter((agent) => !agent.host),
  });
  await waitFor(() => expect(startIn("lane-host-d/gamma")).toBeDefined());
}

test("a peer lane with no orchestrator starts one on its host, and is off where the pairing cannot act", async () => {
  const posted = stubApi("federated");
  await renderWithoutPeerOrchestrators();

  // host-f only reads, and host-e is down: each says why it is off.
  expect(startIn("lane-host-f/atlas").disabled).toBe(true);
  expect(startIn("lane-host-f/atlas").title).toBe("view only pairing");
  expect(startIn("lane-host-e/ledger").disabled).toBe(true);
  expect(startIn("lane-host-e/ledger").title).toContain("host-e is unreachable since");

  fireEvent.click(startIn("lane-host-d/gamma"));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]).toMatchObject({
    method: "POST",
    path: "/api/v1/projects/gamma/orchestrator",
    body: { host: "host-d" },
  });
  expect(typeof posted[0]?.body.idempotency_key).toBe("string");
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain(
      "Started the gamma orchestrator on host-d",
    ),
  );
});

test("a peer start refused by this host's cookie asks for a login, and the peer's own refusal is its words", async () => {
  stubApi("federated");
  const served = globalThis.fetch;
  let error: Record<string, unknown> = {
    code: "forbidden",
    message: "the board is not signed in as the owner",
  };
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) =>
    String(input) === "/api/v1/projects/gamma/orchestrator"
      ? Promise.resolve(new Response(JSON.stringify({ ok: false, error }), { status: 403 }))
      : served(input, init),
  );
  await renderWithoutPeerOrchestrators();

  fireEvent.click(startIn("lane-host-d/gamma"));
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("Run `tiller board --login`"),
  );

  error = { code: "forbidden", message: "host-d refuses this start", host: "host-d" };
  await waitFor(() => expect(startIn("lane-host-d/gamma").disabled).toBe(false));
  fireEvent.click(startIn("lane-host-d/gamma"));
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("host-d refuses this start"),
  );

  // This host's owner marker names another system: not a cookie problem.
  error = {
    code: "owner_marker",
    message: 'read only: the owner marker names "other", not "tiller"',
    hint: "tiller owner set tiller",
  };
  await waitFor(() => expect(startIn("lane-host-d/gamma").disabled).toBe(false));
  fireEvent.click(startIn("lane-host-d/gamma"));
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain(
      'read only: the owner marker names "other"',
    ),
  );
  expect(screen.getByTestId("toast").textContent).not.toContain("tiller board --login");
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

// A dependency change is forwarded to the card's host (protocol.md section 7.4).
test("a peer's card adds a dependency on its own host, and is off for a down peer", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/cards/${GAMMA_12_D}?owner=host-d`);
  const row = await screen.findByTestId("depends-on");
  const add = within(row).getByRole("button", { name: "Add" }) as HTMLButtonElement;
  expect(add.disabled).toBe(false);
  fireEvent.click(add);
  const list = await screen.findByRole("listbox", { name: "Cards" });
  // Only host-d's own cards: this host's gamma cards share the display ids.
  await waitFor(() => expect(within(list).getAllByRole("option").length).toBeGreaterThan(0));
  const options = within(list).getAllByRole("option");
  fireEvent.click(options[0] as HTMLElement);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe(`/api/v1/cards/${GAMMA_12_D}/deps`);
  expect(posted[0]?.method).toBe("POST");
  expect(posted[0]?.body.host).toBe("host-d");
  cleanup();

  await renderApp("federated", `/cards/${LEDGER_5_E}?owner=host-e`);
  const down = await screen.findByTestId("depends-on");
  const off = within(down).getByRole("button", { name: "Add" }) as HTMLButtonElement;
  expect(off.disabled).toBe(true);
  expect(off.parentElement?.title).toContain("host-e is unreachable");
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
  const picker = within(actions).getByLabelText("Project");
  // The card's own project is chosen, and it lives on host-d.
  expect(picker.textContent).toBe("gamma");
  fireEvent.click(picker);
  const list = await screen.findByRole("listbox");
  const groups = [...list.querySelectorAll(".sel-group")].map((group) => ({
    label: group.querySelector(".sel-group-label")?.textContent,
    off: group.querySelector("[role=option]")?.getAttribute("aria-disabled") === "true",
  }));
  expect(groups).toEqual([
    { label: "host-a", off: false },
    { label: "host-d", off: false },
    { label: expect.stringContaining("host-e - host-e is unreachable"), off: true },
    { label: "host-f - view only pairing", off: true },
  ]);
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

test("the Agents screen shows the host of every agent, and a peer's pane is watched here and opened in herdr only on its host", async () => {
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
  const open = (await screen.findByRole("button", { name: /Open in herdr/ })) as HTMLButtonElement;
  expect(open.disabled).toBe(true);
  expect(open.title).toBe("host-d owns this pane: open it from that host");
});

test("a peer's pane streams through its board host, and is never read as a saved machine's tail", async () => {
  stubApi("federated");
  const api = globalThis.fetch;
  const asked: { path: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (!/\/terminal(\/tail)?$/.test(path)) return api(input, init);
    asked.push({ path, body: JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown> });
    return new Promise<Response>(() => {});
  });
  const agents = (fixtureSnapshot("federated").queries[agentsPath()] as AgentsResponse)
    .orchestrators;
  // The peer is a remote host in the Machines read, as a saved machine is;
  // only its link tells the two apart.
  const worker = agents.flatMap((a) => a.workers).find((w) => w.host === "host-d");
  if (!worker) throw new Error("no host-d worker in the fixture");
  await renderApp("federated", `/agents?agent=${worker.session_id}`);
  await waitFor(() => expect(asked).not.toHaveLength(0));
  await flush();
  expect(asked).toEqual([
    { path: `/api/v1/agents/${worker.session_id}/terminal`, body: { host: "host-d" } },
  ]);
});

test("a link to another screen leaves the card page's owner behind", async () => {
  stubApi("federated");
  await renderApp("federated", `/cards/${LEDGER_5_E}?owner=host-e`);
  await screen.findByTestId("card-page");
  const links = within(screen.getByRole("navigation", { name: "Screens" })).getAllByRole("link");
  expect(links.length).toBeGreaterThan(0);
  for (const link of links) expect(link.getAttribute("href")).not.toContain("owner=");
});

// ---- Tiller bugs ----

const FLEET_2 = "01JCARDBUG0000000000000002";
const FLEET_5_D = "01JPCARDD00000000000000005";

test("a Tiller bug in fleet reads Untriaged, and each host lane says where its Tiller bugs go", async () => {
  stubApi("federated");
  await renderApp("federated");
  const fleet = screen.getByTestId("lane-fleet");
  // The route sits in the lane head, and on a line of its own on a narrow screen.
  expect(within(fleet).getAllByText("Tiller bugs go to tiller")).toHaveLength(2);
  const fleet2 = within(fleet).getByRole("link", { name: /fleet-2/ });
  expect(within(fleet2).getByText("Untriaged")).toBeTruthy();
  expect(within(fleet2).getByText("Bug")).toBeTruthy();
  expect(within(fleet).getByTestId("waiting-bugs").textContent).toContain(
    "1 Tiller bug waits here from before tiller took them.",
  );
  // fleet-1 is the standing Host health card, never a Tiller bug to move.
  expect(
    within(within(fleet).getByRole("link", { name: /fleet-1/ })).queryByText("Untriaged"),
  ).toBeNull();

  const peerFleet = screen.getByTestId("lane-host-d/fleet");
  expect(within(peerFleet).getAllByText("No project takes Tiller bugs here")).toHaveLength(2);
  expect(within(peerFleet).queryByTestId("waiting-bugs")).toBeNull();

  const tiller = screen.getByTestId("lane-tiller");
  expect(within(tiller).getByText("Takes Tiller bugs")).toBeTruthy();
  expect(within(tiller).getByText("from alpha")).toBeTruthy();
});

test("Move on a peer's Tiller bug files it in the bug project, shows a close that failed, and a retry reuses both keys", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/cards/${FLEET_5_D}?owner=host-d`);
  const triage = await screen.findByTestId("bug-triage");
  expect(triage.textContent).toContain("host-d has no project that takes Tiller bugs");
  const pick = within(triage).getByRole("button", {
    name: "Pick a bug project for host-d",
  }) as HTMLButtonElement;
  expect(pick.disabled).toBe(true);

  fireEvent.click(within(triage).getByRole("button", { name: /^Move to host-a · tiller/ }));
  await flush();
  expect(posted).toHaveLength(2);
  expect(posted[0]).toMatchObject({ method: "POST", path: "/api/v1/cards" });
  expect(posted[0]?.body).toMatchObject({ project: "tiller", category: "tiller-bug" });
  expect(posted[0]?.body.host).toBeUndefined();
  expect(String(posted[0]?.body.body)).toContain("Moved from host-d · fleet-5, filed from gamma.");
  expect(posted[1]?.path).toBe(`/api/v1/cards/${FLEET_5_D}/close`);
  expect(posted[1]?.body).toMatchObject({ outcome: "superseded", host: "host-d" });
  const made = String(posted[1]?.body.reason).replace("Moved to ", "");
  expect(made).toMatch(/^host-a · tiller-\d+$/);
  const partial = within(triage).getByTestId("move-partial");
  expect(partial.textContent).toContain(`Made ${made}, but could not close host-d · fleet-5.`);

  fireEvent.click(within(triage).getByRole("button", { name: /^Move to host-a · tiller/ }));
  await flush();
  expect(posted).toHaveLength(4);
  expect(posted[2]?.body.idempotency_key).toBe(posted[0]?.body.idempotency_key);
  expect(posted[3]?.body.idempotency_key).toBe(posted[1]?.body.idempotency_key);
  expect(posted[3]?.body.reason).toBe(`Moved to ${made}`);
  await vi.waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain(`Moved to ${made}`),
  );
});

test("Close as not a bug cancels this host's card with the reason and no host field", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/cards/${FLEET_2}`);
  const triage = await screen.findByTestId("bug-triage");
  expect(triage.textContent).toContain("host-a files Tiller bugs into tiller now");
  fireEvent.click(within(triage).getByRole("button", { name: "Close as not a bug" }));
  await flush();
  expect(posted).toHaveLength(1);
  expect(posted[0]?.path).toBe(`/api/v1/cards/${FLEET_2}/close`);
  expect(posted[0]?.body).toMatchObject({ outcome: "cancelled", reason: "Not a Tiller bug" });
  expect(posted[0]?.body.host).toBeUndefined();
});

test("the owner picks this host's bug project from a Tiller bug, against the host's rev", async () => {
  const posted = stubApi("federated");
  await renderApp("federated", `/cards/${FLEET_2}`);
  const triage = await screen.findByTestId("bug-triage");
  fireEvent.click(within(triage).getByRole("button", { name: "Pick a bug project for host-a" }));
  const dialog = await screen.findByRole("dialog", { name: "Pick a bug project for host-a" });
  const save = within(dialog).getByRole("button", { name: "Save" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  await choose(within(dialog).getByRole("combobox"), "alpha");
  fireEvent.click(save);
  await flush();
  expect(posted).toHaveLength(1);
  expect(posted[0]?.path).toBe("/api/v1/bug-project");
  expect(posted[0]?.body).toMatchObject({ project: "alpha" });
  expect(typeof posted[0]?.body.rev).toBe("number");
  // The daemon answers with the host record, whose field is `bug_project_id`.
  await vi.waitFor(() => {
    const toast = screen.getByTestId("toast").textContent;
    expect(toast).toContain("alpha takes Tiller bugs on host-a");
    expect(toast).toContain("Move the ones that wait in host / host-a.");
  });
});

test("the fleet lane moves the Tiller bugs that wait there to the new bug project", async () => {
  const posted = stubApi("federated");
  await renderApp("federated");
  const strip = screen.getByTestId("waiting-bugs");
  fireEvent.click(within(strip).getByRole("button", { name: /Move 1 to tiller/ }));
  const dialog = await screen.findByRole("dialog", { name: "Move Tiller bugs to host-a · tiller" });
  expect(dialog.textContent).toContain("host-a · fleet-2");
  fireEvent.click(within(dialog).getByRole("button", { name: "Move 1" }));
  await vi.waitFor(() => expect(posted).toHaveLength(2));
  expect(posted[0]?.body).toMatchObject({ project: "tiller", category: "tiller-bug" });
  expect(posted[1]?.path).toBe(`/api/v1/cards/${FLEET_2}/close`);
  expect(posted[1]?.body).toMatchObject({ outcome: "superseded" });
  await vi.waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain(
      "Moved 1 Tiller bug to host-a · tiller",
    ),
  );
});
