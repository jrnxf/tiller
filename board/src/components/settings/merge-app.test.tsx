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

const EXPOSED: MergeAppHealth = {
  status: "fail",
  app_id: null,
  detail:
    "the merge App key is not used, so every project runs as detect: /home/user/.tiller/forge-app/private-key.pem is readable by group or others (mode 644); run chmod 600 on it",
};

const BAD_ID: MergeAppHealth = {
  status: "warn",
  app_id: null,
  detail:
    "the merge App key is not used, so every project runs as detect: /home/user/.tiller/forge-app/app-id: not a numeric App id",
};

const BAD_KEY: MergeAppHealth = {
  status: "warn",
  app_id: null,
  detail:
    "the merge App key is not used, so every project runs as detect: /home/user/.tiller/forge-app/private-key.pem: not a PEM key",
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
  return fetch;
}

function renderSection(app: MergeAppHealth) {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <MergeAppSection app={app} host="host-a" />
    </QueryClientProvider>,
  );
}

function commandLines(): string[] {
  return (screen.getByTestId("merge-commands").textContent ?? "").split("\n");
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
  // Health reads the key, not the App's permissions, so it claims none.
  expect(screen.getByText("App 1234567 has its key on this host.")).toBeTruthy();
  expect(screen.queryByText(/permissions/)).toBeNull();
  expect(within(screen.getByTestId("merge-step-key")).getByText("Done")).toBeTruthy();
  expect(screen.getByTestId("merge-app-status").textContent).toBe("Ready");
  const repo = await screen.findByTestId("merge-repo-alpha");
  expect(within(repo).getAllByText("Done")).toHaveLength(2);
  expect(within(repo).queryByRole("link")).toBeNull();
  expect(screen.getByText("github.com/example/alpha")).toBeTruthy();
  expect(screen.getByText("Enforced")).toBeTruthy();
  // Nothing is open, so there is nothing to check again.
  expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
});

test("a repo on detect links to its GitHub pages, each by its own name", async () => {
  serve(report([["beta", "detect"]]));
  renderSection(READY);
  const repo = await screen.findByTestId("merge-repo-beta");
  await waitFor(() =>
    expect(
      within(repo)
        .getAllByRole("link")
        .map((link) => [link.getAttribute("aria-label"), link.getAttribute("href")]),
    ).toEqual([
      [
        "Open on GitHub: install the App on example/beta",
        "https://github.com/settings/installations",
      ],
      [
        "Open on GitHub: rulesets of example/beta",
        "https://github.com/example/beta/settings/rules",
      ],
    ]),
  );
  expect(screen.getByText("Detect")).toBeTruthy();
  // The board cannot tell which of the two is missing, so it asks to check.
  expect(screen.getByTestId("merge-step-install-beta").textContent).toContain(
    "Check that the App is installed on this repo.",
  );
  // The board only links: nothing here writes.
  expect(screen.queryByRole("button", { name: /install|create|add/i })).toBeNull();
});

test("one Check again in the section head reads health and every repo again", async () => {
  const fetch = serve(
    report([
      ["beta", "detect"],
      ["gamma", "detect"],
    ]),
  );
  renderSection(READY);
  await screen.findByTestId("merge-repo-gamma");
  const buttons = screen.getAllByRole("button", { name: "Check again" });
  expect(buttons).toHaveLength(1);
  expect(buttons[0]?.closest(".set-sec-h")).toBeTruthy();
  const doctorReads = () =>
    fetch.mock.calls.filter(([input]) => String(input) === "/api/v1/doctor").length;
  const before = doctorReads();
  fireEvent.click(buttons[0] as HTMLElement);
  await waitFor(() => expect(doctorReads()).toBe(before + 1));
});

test("a host with no key gets Check again even with no gated repo", async () => {
  serve(report([]));
  renderSection(MISSING);
  expect(await screen.findByRole("button", { name: "Check again" })).toBeTruthy();
});

test("the steps follow the setup doc: the App, each repo, then the key last", async () => {
  serve(report([["beta", "detect"]]));
  renderSection(MISSING);
  await screen.findByTestId("merge-repo-beta");
  const order = [...document.querySelectorAll("[data-testid^='merge-step-']")].map((step) =>
    step.getAttribute("data-testid"),
  );
  expect(order).toEqual([
    "merge-step-app",
    "merge-step-install-beta",
    "merge-step-ruleset-beta",
    "merge-step-key",
  ]);
  // The ruleset can go in before the key, so it does not wait on it.
  const ruleset = screen.getByTestId("merge-step-ruleset-beta");
  expect(ruleset.getAttribute("data-tone")).toBe("amber");
  expect(ruleset.textContent).toContain("Add it before the key.");
  expect(screen.getByTestId("merge-step-key").textContent).toContain(
    "Do this last, after each repo's ruleset exists.",
  );
});

test("a host with no key shows commands with placeholders, and links the App page", () => {
  serve(report([]));
  renderSection(MISSING);
  const status = screen.getByTestId("merge-app-status");
  expect(status.textContent).toBe("Not set up");
  expect(status.getAttribute("role")).toBe("status");
  const app = screen.getByTestId("merge-step-app");
  expect(app.getAttribute("data-tone")).toBe("amber");
  expect(app.textContent).toContain("Checks read-only");
  expect(
    within(app).getByRole("link", { name: "Open on GitHub: new App" }).getAttribute("href"),
  ).toBe("https://github.com/settings/apps/new");
  expect(screen.queryByTestId("merge-commands")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show commands" }));
  expect(commandLines()).toEqual([
    'dir="${TILLER_HOME:-$HOME/.tiller}/forge-app"',
    'mkdir -p "$dir"',
    'chmod 700 "$dir"',
    'echo <APP_ID> > "$dir/app-id"',
    'mv <DOWNLOADED_KEY.pem> "$dir/private-key.pem"',
    'chmod 600 "$dir/private-key.pem"',
  ]);
  // No example App ID or download path passes for a real one.
  const commands = commandLines().join("\n");
  expect(commands).not.toContain("1234567");
  expect(commands).not.toContain("/path/to");
  expect(screen.getByText(/^Replace <APP_ID> and <DOWNLOADED_KEY\.pem> with yours/)).toBeTruthy();
  expect(screen.getByText(/the default ~\/\.tiller when it does not/)).toBeTruthy();
  expect(screen.getByText(/Run the commands in your own terminal, not in an agent\./)).toBeTruthy();
});

test("an exposed key means the App exists: no new App, only chmod 600 and the rotation", async () => {
  serve(report([["beta", "detect"]]));
  renderSection(EXPOSED);
  expect(screen.getByTestId("merge-app-status").textContent).toBe("Key exposed");
  const app = screen.getByTestId("merge-step-app");
  expect(app.getAttribute("data-tone")).toBe("green");
  expect(within(app).getByText("A key for the App is on this host.")).toBeTruthy();
  expect(within(app).queryByRole("link")).toBeNull();
  const key = screen.getByTestId("merge-step-key");
  expect(key.getAttribute("data-tone")).toBe("red");
  // The daemon's reason reads as a sentence before the board's own.
  expect(key.textContent).toContain("The merge App key is not used");
  expect(key.textContent).toContain("run chmod 600 on it. Run the commands");
  fireEvent.click(screen.getByRole("button", { name: "Show commands" }));
  expect(commandLines()).toEqual([
    'chmod 600 "${TILLER_HOME:-$HOME/.tiller}/forge-app/private-key.pem"',
  ]);
  expect(screen.getByText(/rotate it: generate a new key on the App's page/)).toBeTruthy();
  // The key is already there, so the ruleset step does not ask to come first.
  const ruleset = await screen.findByTestId("merge-step-ruleset-beta");
  expect(ruleset.textContent).not.toContain("before the key");
});

test("an App ID Tiller cannot read gets only the App ID line", () => {
  serve(report([]));
  renderSection(BAD_ID);
  expect(screen.getByTestId("merge-step-app").getAttribute("data-tone")).toBe("green");
  fireEvent.click(screen.getByRole("button", { name: "Show commands" }));
  expect(commandLines()).toEqual([
    'dir="${TILLER_HOME:-$HOME/.tiller}/forge-app"',
    'echo <APP_ID> > "$dir/app-id"',
  ]);
});

test("a key file Tiller cannot use gets only the key lines", () => {
  serve(report([]));
  renderSection(BAD_KEY);
  fireEvent.click(screen.getByRole("button", { name: "Show commands" }));
  expect(commandLines()).toEqual([
    'dir="${TILLER_HOME:-$HOME/.tiller}/forge-app"',
    'mv <DOWNLOADED_KEY.pem> "$dir/private-key.pem"',
    'chmod 600 "$dir/private-key.pem"',
  ]);
});

test("repos that cannot be read say so", async () => {
  serve(null);
  renderSection(READY);
  expect(
    (await screen.findByTestId("merge-repos-unreadable", {}, { timeout: 4000 })).textContent,
  ).toContain("no checks");
});
