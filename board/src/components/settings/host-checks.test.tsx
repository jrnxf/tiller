import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { DaemonHealth, DoctorReport, Machine, PollerHealth } from "../../lib/api-types";
import type { Scenario } from "../../lib/fixtures";
import { FixtureRefusal, fixtureResource } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { signedOutStore } from "../../lib/session";
import { SettingsPage } from "./settings-page";
import { createTestQueryClient } from "../../test-app";

// The fixture host's health, doctor report and hosts list. Run now on a
// poller answers with that poller polled, and health lists it so after.
function serve(
  scenario: Scenario,
  {
    owner = true,
    doctor = true,
    startedAt,
    peer,
  }: {
    owner?: boolean;
    doctor?: boolean;
    startedAt?: string;
    // Answers a peer's checks in place of the fixture: the read's number,
    // from 1, and the fixture's report.
    peer?: (read: number, report: DoctorReport) => Response;
  } = {},
) {
  const ran = new Map<string, PollerHealth>();
  let peerReads = 0;
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") return new Response(JSON.stringify({ owner }));
    if (peer && path === "/api/v1/doctor?host=host-d") {
      peerReads += 1;
      return peer(peerReads, fixtureResource(path, scenario) as DoctorReport);
    }
    if (path.startsWith("/api/v1/doctor?")) {
      const read = fixtureResource(path, scenario);
      if (read instanceof FixtureRefusal) {
        return new Response(JSON.stringify(read.body), { status: read.status });
      }
      return read === undefined
        ? new Response(JSON.stringify({ ok: false, error: { code: "not_found" } }), { status: 404 })
        : new Response(JSON.stringify(read));
    }
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
      return new Response(
        JSON.stringify({ ...health, pollers, started_at: startedAt ?? health.started_at }),
      );
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
  // The failing poller, and the Merge App section's problem counted once.
  expect((await within(row).findByTestId("host-needs-you")).textContent).toBe("2 checks need you");
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
  // The Merge App problem stays; Run now does not fix it.
  const row = screen.getByTestId("host-host-a");
  await waitFor(() =>
    expect(within(row).getByTestId("host-needs-you").textContent).toBe("1 check needs you"),
  );
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

test("a peer's card shows the checks it sends, read-only, and a herdr machine's points to this host", async () => {
  const fetch = serve("federated");
  renderSettings();
  const peer = (await openHost("host-d")).card;
  const daemon = await within(peer).findByTestId("check-tool:no-mistakes daemon");
  expect(daemon.textContent).toContain("CLI is v1.106.1");
  expect(calls(fetch, "/api/v1/doctor?host=host-d")).toBe(1);
  // The peer's merge App and gates are in its card: this board's Merge App
  // section is this host's.
  expect(within(peer).getByTestId("check-check:merge app").textContent).toContain("App 1234567");
  expect(within(peer).getByTestId("check-merge-gates").textContent).toContain(
    "gamma, fleet (enforced): the ruleset check holds.",
  );
  expect(peer.textContent).toMatch(/Read from host-d, last run \w{3} \d{1,2} \w{3}, \d{2}:\d{2}/);
  // It says where the checks ran: the link's SSH session, not the daemon.
  expect(within(peer).getByTestId("check-environment").textContent).toContain(
    "They ran in the SSH session of host-d's link, not in its daemon.",
  );
  // Read-only: no Run now, no fix. Check again only reads the checks again.
  expect(within(within(peer).getByTestId("peer-checks")).queryByRole("button")).toBeNull();
  fireEvent.click(within(peer).getByRole("button", { name: "Check again" }));
  await waitFor(() => expect(calls(fetch, "/api/v1/doctor?host=host-d")).toBe(2));
  expect(peer.textContent).not.toMatch(/firstmate/i);

  const down = (await openHost("host-e")).card;
  expect((await within(down).findByTestId("check-unreadable")).textContent).toBe(
    "Checkshost-e did not send them: host-e is unreachable; its link is not live. They show again once its link is live.",
  );

  const herdr = (await openHost("host-b")).card;
  expect(within(herdr).getByTestId("check-elsewhere").textContent).toBe(
    "No daemon to checkhost-b runs agents through herdr only. Its herdr poller is in the checks of host-a.",
  );
  // A peer's own checks are not this board's to count.
  expect(within(screen.getByTestId("host-host-d")).queryByTestId("host-needs-you")).toBeNull();
});

test("the host row counts the Merge App problem once, and gamma on detect is one problem", async () => {
  serve("merge-ready");
  renderSettings();
  const row = await screen.findByTestId("host-host-a");
  // The App is ready and alpha and beta are enforced; gamma still runs as
  // detect, which is one problem however many repos wait.
  expect((await within(row).findByTestId("host-needs-you")).textContent).toBe("1 check needs you");
  const section = screen.getByTestId("merge-app");
  expect(await within(section).findAllByText("Enforced")).toHaveLength(2);
  expect(within(section).getAllByText("Detect")).toHaveLength(1);
});

test("the uptime in the host row moves with the clock", async () => {
  // 59 min 30 s up: a minute later the row reads 1 h without a new read.
  serve("merge-ready", { startedAt: new Date(Date.now() - 59.5 * 60_000).toISOString() });
  renderSettings();
  const row = await screen.findByTestId("host-host-a");
  await within(row).findByText("up 59 min");
  vi.useFakeTimers({ toFake: ["Date"] });
  try {
    vi.setSystemTime(Date.now() + 60_000);
    await within(row).findByText("up 1 h", undefined, { timeout: 2500 });
  } finally {
    vi.useRealTimers();
  }
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

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

test("a peer's missing tool says to check again, and the card can", async () => {
  const fetch = serve("federated", {
    peer: (_read, report) =>
      json({
        ...report,
        checks: report.checks.map((check) =>
          check.name === "gh"
            ? { ...check, status: "fail", version: null, detail: "gh is not on PATH" }
            : check,
        ),
      }),
  });
  renderSettings();
  const { card } = await openHost("host-d");
  const gh = await within(card).findByTestId("check-tool:gh");
  expect(gh.textContent).toContain("Add it to the host's Nix config, then check again.");
  fireEvent.click(within(card).getByRole("button", { name: "Check again" }));
  await waitFor(() => expect(calls(fetch, "/api/v1/doctor?host=host-d")).toBe(2));
});

test("a peer's failed read again keeps its last report, marked as the last good read", async () => {
  serve("federated", {
    peer: (read, report) =>
      read === 1
        ? json(report)
        : json(
            {
              ok: false,
              error: {
                code: "peer_unreachable",
                message: "host-d is unreachable; its link is not live",
              },
            },
            503,
          ),
  });
  renderSettings();
  const { card } = await openHost("host-d");
  await within(card).findByTestId("check-tools:found");
  fireEvent.click(within(card).getByRole("button", { name: "Check again" }));
  const line = await within(card).findByTestId("check-unreadable", undefined, { timeout: 3000 });
  expect(line.textContent).toContain("The lines below are its last good read.");
  expect(card.textContent).toMatch(/Last good read from host-d, \w{3} \d{1,2} \w{3}, \d{2}:\d{2}/);
  expect(card.textContent).not.toMatch(/Read from host-d, last run/);
  expect(within(card).getByTestId("check-tools:found")).toBeTruthy();
});

test("a peer on an older tiller that cannot send its checks says so, not the parser's words", async () => {
  serve("federated", {
    peer: () =>
      json(
        {
          ok: false,
          error: {
            code: "usage",
            message: "a query is cards/{id}, cards/{id}/timeline or runs/{id}",
          },
        },
        400,
      ),
  });
  renderSettings();
  const { card } = await openHost("host-d");
  const line = await within(card).findByTestId("check-unreadable", undefined, { timeout: 3000 });
  expect(line.textContent).toBe(
    "Checkshost-d runs an older tiller that cannot send its checks. Run tiller doctor in a shell on that host.",
  );
});

test("the stand-in server answers a peer's checks by its id as by its label", () => {
  const machines = fixtureResource("/api/v1/machines", "federated") as { machines: Machine[] };
  const id = machines.machines.find((machine) => machine.label === "host-d")?.id ?? "";
  expect(id).not.toBe("host-d");
  const byId = fixtureResource(`/api/v1/doctor?host=${id}`, "federated") as DoctorReport;
  expect(byId?.checks.length).toBeGreaterThan(0);
  expect(byId.merge_gates.map((gate) => gate.project)).toEqual(["gamma", "fleet"]);
});
