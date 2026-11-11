import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { fixtureMutation, withoutDeleted } from "../../lib/fixture-mutations";
import type { Scenario } from "../../lib/fixtures";
import { forgetTakenBackups } from "../../lib/fixtures-backups";
import { createQueryClient } from "../../lib/query-client";
import { Backups, sizeText } from "./backups";

function serve(scenario: Scenario, post?: () => Response) {
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (init?.method === "POST") {
      if (post) return post();
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      return new Response(JSON.stringify(fixtureMutation("POST", path, body, scenario).reply.body));
    }
    return new Response(JSON.stringify(withoutDeleted(path, undefined, scenario)));
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function renderBackups() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <Backups host="host-a" />
    </QueryClientProvider>,
  );
}

beforeEach(forgetTakenBackups);

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("sizes read in KB, MB and GB", () => {
  expect(sizeText(300 * 1024)).toBe("300 KB");
  expect(sizeText(48 * 1024 * 1024)).toBe("48 MB");
  expect(sizeText(2048 * 1024 * 1024)).toBe("2.0 GB");
});

test("lists each backup with its Lisbon time, size, maker and verdict, and the folder", async () => {
  serve("default");
  renderBackups();
  const section = await screen.findByRole("region", { name: "Backups" });
  const rows = await within(section).findAllByTestId("backup-row");
  expect(rows[0]?.textContent).toContain("Nightly");
  expect(rows[0]?.textContent).toContain("Tue 10 Nov, 03:00 · 48 MB · by the nightly timer");
  expect(rows[0]?.textContent).toContain("Verified");
  expect(rows[1]?.textContent).toContain("By hand");
  expect(rows[1]?.textContent).toContain("by you");
  expect(within(section).getByText("/home/user/.tiller/backups")).toBeTruthy();
  expect(within(section).getByText("tiller restore <file>")).toBeTruthy();
  expect(within(section).getByRole("button", { name: "Back up now" })).toBeTruthy();
});

test("Back up now shows Backing up... while it runs, then the new backup", async () => {
  // The daemon answers when the test lets it, so the busy state cannot be missed.
  let finish: (() => void) | undefined;
  const finished = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      const reply = new Response(
        JSON.stringify(fixtureMutation("POST", path, body, "default").reply.body),
      );
      return finished.then(() => reply);
    }
    return new Response(JSON.stringify(withoutDeleted(path, undefined, "default")));
  });
  vi.stubGlobal("fetch", fetch);
  renderBackups();
  const button = await screen.findByRole("button", { name: "Back up now" });
  fireEvent.click(button);
  const busy = await screen.findByRole("button", { name: "Backing up..." });
  expect((busy as HTMLButtonElement).disabled).toBe(true);
  finish?.();
  await waitFor(() => expect(screen.getByRole("button", { name: "Back up now" })).toBeTruthy());
  await waitFor(() => expect(screen.getAllByTestId("backup-row")).toHaveLength(4));
  expect(screen.getAllByTestId("backup-row")[0]?.textContent).toContain("By hand");
});

test("a run the daemon reports as running disables the button", async () => {
  serve("backing-up");
  renderBackups();
  const busy = await screen.findByRole("button", { name: "Backing up..." });
  expect((busy as HTMLButtonElement).disabled).toBe(true);
});

test("a failed verify says so and that the previous backup is kept", async () => {
  serve("backup-failed");
  renderBackups();
  const rows = await screen.findAllByTestId("backup-row");
  expect(rows[0]?.getAttribute("data-status")).toBe("failed");
  expect(rows[0]?.textContent).toContain("Failed");
  expect(rows[0]?.textContent).toContain(
    "Not verified: the copy failed its integrity check. The previous verified backup is kept. Try again.",
  );
  expect(rows[1]?.getAttribute("data-status")).toBe("verified");
});

test("a refusal from the daemon shows in the section", async () => {
  serve(
    "default",
    () =>
      new Response(
        JSON.stringify({
          ok: false,
          error: { code: "conflict", message: "a backup is already running" },
        }),
        { status: 409 },
      ),
  );
  renderBackups();
  fireEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  expect((await screen.findByRole("alert")).textContent).toContain("a backup is already running");
});

test("no backup yet says so", async () => {
  serve("empty");
  renderBackups();
  expect(await screen.findByTestId("no-backups")).toBeTruthy();
});
