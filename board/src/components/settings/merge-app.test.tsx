import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { DoctorReport, MergeAppHealth, ProjectSetup } from "../../lib/api-types";
import { MergeAppSection } from "./merge-app";
import { createTestQueryClient } from "../../test-app";

const READY: MergeAppHealth = {
  status: "ok",
  app_id: 1234567,
  detail: "App 1234567 with its key in /home/user/.tiller/forge-app",
};

const MISSING: MergeAppHealth = {
  status: "missing",
  app_id: null,
  detail:
    "Create the merge App and put its app-id and private-key.pem in /home/user/.tiller/forge-app.",
};

function report(gates: [string, "enforced" | "detect"][]): DoctorReport {
  return {
    checked_at: "2026-11-11T16:05:00Z",
    ok: true,
    checks: [],
    merge_gates: gates.map(([project, merge_gate]) => ({
      project,
      merge_gate,
      status: merge_gate === "enforced" ? "ok" : "warn",
      detail: "merge gate",
      ...(merge_gate === "enforced" ? { last_changed_at: "2026-11-10T09:00:00Z" } : {}),
    })),
  };
}

function serve(doctor: DoctorReport | null) {
  const fetch = vi.fn(async (input: RequestInfo | URL) => {
    const path = String(input);
    if (path === "/api/v1/doctor") {
      if (!doctor)
        return new Response(
          JSON.stringify({ ok: false, error: { code: "unavailable", message: "no checks" } }),
          { status: 503 },
        );
      return new Response(JSON.stringify(doctor));
    }
    const setup = /^\/api\/v1\/projects\/([^/]+)\/setup$/.exec(path);
    if (setup) {
      const body: ProjectSetup = {
        project: setup[1] ?? "",
        repo: `example/${setup[1]}`,
        merge_gate: "detect",
        merge_gate_changed_at: null,
        checkout: null,
        config_error: null,
      };
      return new Response(JSON.stringify(body));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
}

function renderSection(app: MergeAppHealth) {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <MergeAppSection app={app} host="host-a" />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("a ready host has both steps done, and an enforced repo is done with no link", async () => {
  serve(report([["alpha", "enforced"]]));
  renderSection(READY);
  expect(within(screen.getByTestId("merge-step-app")).getByText("Done")).toBeTruthy();
  expect(screen.getByText("App 1234567, with the permissions Tiller needs")).toBeTruthy();
  expect(within(screen.getByTestId("merge-step-key")).getByText("Done")).toBeTruthy();
  expect(screen.getByTestId("merge-app-status").textContent).toBe("Ready");
  const repo = await screen.findByTestId("merge-repo-alpha");
  expect(within(repo).getAllByText("Done")).toHaveLength(2);
  expect(within(repo).queryByRole("link")).toBeNull();
  expect(screen.getByText("github.com/example/alpha")).toBeTruthy();
  expect(screen.getByText("Enforced")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
});

test("a repo on detect links to its GitHub pages and can be checked again", async () => {
  serve(report([["beta", "detect"]]));
  renderSection(READY);
  const repo = await screen.findByTestId("merge-repo-beta");
  await waitFor(() =>
    expect(
      within(repo)
        .getAllByRole("link", { name: "Open on GitHub" })
        .map((link) => link.getAttribute("href")),
    ).toEqual([
      "https://github.com/settings/installations",
      "https://github.com/example/beta/settings/rules",
    ]),
  );
  expect(screen.getByText("Detect")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Check again" })).toBeTruthy();
  // The board only links: nothing here writes.
  expect(screen.queryByRole("button", { name: /install|create|add/i })).toBeNull();
});

test("a host with no key shows the commands to run in a terminal, and links the App page", async () => {
  serve(report([]));
  renderSection(MISSING);
  expect(screen.getByTestId("merge-app-status").textContent).toBe("Not set up");
  expect(
    within(screen.getByTestId("merge-step-app"))
      .getByRole("link", { name: "Open on GitHub" })
      .getAttribute("href"),
  ).toBe("https://github.com/settings/apps/new");
  expect(screen.queryByTestId("merge-commands")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show commands" }));
  expect(screen.getByTestId("merge-commands").textContent).toContain(
    "chmod 600 ~/.tiller/forge-app/private-key.pem",
  );
  expect(screen.getByText(/Run the commands in your own terminal, not in an agent\./)).toBeTruthy();
});

test("repos that cannot be read say so", async () => {
  serve(null);
  renderSection(READY);
  expect(
    (await screen.findByTestId("merge-repos-unreadable", {}, { timeout: 4000 })).textContent,
  ).toContain("no checks");
});
