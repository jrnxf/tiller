import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createAppRouter } from "../../lib/router";
import { resetBoardLook } from "./board-prefs";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

const CARD_43 = "01JCARD0000000000000000043";

async function renderApp(path: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const body = fixtureResource(String(input));
      return body === undefined
        ? new Response("{}", { status: 404 })
        : new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot());
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return router;
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
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
});

test("project filter lists published PRs", async () => {
  const router = await renderApp("/contributions");
  const all = screen.getAllByTestId("contribution");
  expect(all.length).toBeGreaterThan(1);
  const projectsShown = new Set(
    all.map((row) => /example\/(\w+)#/.exec(row.textContent ?? "")?.[1]),
  );
  expect(projectsShown.size).toBeGreaterThan(1);

  const [filter] = screen.getAllByRole("group", { name: "Project" });
  if (!filter) throw new Error("no project filter");
  fireEvent.click(within(filter).getByRole("button", { name: "alpha" }));
  await waitFor(() => expect(router.state.location.search).toEqual({ project: "alpha" }));
  await waitFor(() => {
    const rows = screen.getAllByTestId("contribution");
    expect(rows.length).toBeLessThan(all.length);
    for (const row of rows) expect(row.textContent).toContain("example/alpha#");
  });
  // Each row links to the PR itself and to its card.
  const link = within(screen.getAllByTestId("contribution")[0] as HTMLElement).getAllByRole("link");
  expect(link[0]?.getAttribute("href")).toMatch(
    /^https:\/\/github\.com\/example\/alpha\/pull\/\d+$/,
  );
});

test("a project filter in the URL is applied on the first render, and an empty one says so", async () => {
  await renderApp("/contributions?project=nobody");
  expect(screen.queryAllByTestId("contribution")).toHaveLength(0);
  expect(screen.getByText("This project has published no PRs yet.")).toBeTruthy();
});

test("card detail lists its published pages", async () => {
  await renderApp(`/cards/${CARD_43}`);
  const pages = screen.getByTestId("card-pages");
  const link = within(pages).getByRole("link", { name: /^Mockup for / });
  expect(link.getAttribute("href")).toMatch(/^https:\/\/pages\.example\.com\/.+\.html$/);
});
