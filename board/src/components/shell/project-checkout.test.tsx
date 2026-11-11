import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { Project, ProjectsResponse } from "../../lib/api-types";
import { fixtureResponses } from "../../lib/fixtures";
import { projectsPath } from "../../lib/queries";
import { ProjectDetail } from "./project-stage";
import { createTestQueryClient } from "../../test-app";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

function serve(owner: boolean) {
  let checkout: string | null = null;
  const posts: Record<string, unknown>[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === "/api/v1/session") return json({ owner });
      if (path === "/api/v1/start-settings") {
        return json({
          pool_root: "/home/user/pool",
          checkouts_root: "/home/user/src",
          orchestrator_home: null,
          scratch_root: null,
          problems: [],
          config_error: null,
        });
      }
      if (path.endsWith("/setup")) {
        return json({
          project: "alpha",
          repo: "example/alpha",
          merge_gate: "detect",
          merge_gate_changed_at: null,
          checkout,
          config_error: null,
        });
      }
      if (path === "/api/v1/config" && init?.method === "POST") {
        const body = JSON.parse(String(init.body));
        posts.push(body);
        checkout = body.value;
        return json({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
          data: { key: body.key, value: body.value, changed: true },
        });
      }
      return json({}, 404);
    }),
  );
  return posts;
}

function renderDetail() {
  const project = (fixtureResponses()[projectsPath] as ProjectsResponse).projects.find(
    (row: Project) => row.id === "alpha",
  ) as Project;
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ProjectDetail project={project} id="detail" />
    </QueryClientProvider>,
  );
}

test("the owner sets a project's checkout where the project is edited, starting from the folder default", async () => {
  const posts = serve(true);
  renderDetail();
  fireEvent.click(await screen.findByRole("button", { name: "Set" }));
  const field = within(await screen.findByTestId("setting-checkouts.alpha"));
  const input = field.getByRole("textbox") as HTMLInputElement;
  await waitFor(() => expect(input.value).toBe("/home/user/src/alpha"));
  fireEvent.click(field.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.queryByTestId("setting-checkouts.alpha")).toBeNull());
  expect(posts).toEqual([
    expect.objectContaining({ key: "checkouts.alpha", value: "/home/user/src/alpha" }),
  ]);
  expect(await screen.findByRole("button", { name: "Change" })).toBeTruthy();
});

test("a browser that is not the owner is shown the command instead", async () => {
  serve(false);
  renderDetail();
  expect(await screen.findByText(/tiller config set checkouts.alpha/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Set" })).toBeNull();
});
