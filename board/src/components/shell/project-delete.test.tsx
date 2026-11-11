import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type {
  Project,
  ProjectAgent,
  ProjectDeletePreview,
  ProjectsResponse,
} from "../../lib/api-types";
import { fixtureResource } from "../../lib/fixtures";
import { projectsPath } from "../../lib/queries";
import { createQueryClient } from "../../lib/query-client";
import { ConfirmDelete } from "./project-delete";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const ORCHESTRATOR = "01JSESSOBETA00000000000000";
const BUILDER = "01JSESSBBETA00000000000007";

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

const recorded = {
  ok: true,
  replayed: false,
  events: [],
  receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
  data: null,
};

function agents(local: boolean): ProjectAgent[] {
  return [
    { role: "orchestrator", session: ORCHESTRATOR, card: null, run: null, local },
    { role: "builder", session: BUILDER, card: "beta-7", run: "01JRUNBETA7", local },
  ];
}

// The daemon's preview for beta, with what blocks it at this step.
function previewOf(step: "card" | "agent" | "clear", local = true): ProjectDeletePreview {
  const base = fixtureResource("/api/v1/projects/beta/delete", "default") as ProjectDeletePreview;
  if (step === "card") {
    return {
      ...base,
      agents: agents(local),
      refused: "builder run on beta-7 is live; end it first",
      unblock: {
        step: "cancel its card",
        command: 'tiller card cancel beta-7 --reason "deleting project beta"',
        action: { id: "card.cancel", target: "beta-7" },
      },
    };
  }
  if (step === "agent") {
    return {
      ...base,
      agents: agents(local).slice(0, 1),
      refused: `orchestrator session ${ORCHESTRATOR} is live; end it first`,
      unblock: {
        step: "stop its agent",
        command: `tiller agent stop ${ORCHESTRATOR}`,
        action: { id: "agent.stop", target: ORCHESTRATOR },
      },
    };
  }
  return { ...base, agents: [], refused: null, unblock: null };
}

// Serves the preview at `step`, and moves it on as the owner's writes
// clear each blocker.
function serve(first: "card" | "agent", local = true, stopWarning: string | null = null) {
  let step: "card" | "agent" | "clear" = first;
  const posted: { path: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === "/api/v1/projects/beta/delete" && (init?.method ?? "GET") === "GET") {
        return json(previewOf(step, local));
      }
      if (init?.method === "POST") {
        posted.push({ path, body: JSON.parse(String(init.body)) as Record<string, unknown> });
        if (path.endsWith("/cancel")) step = "agent";
        // A stop with a warning left the session live.
        if (path.endsWith("/stop") && stopWarning === null) step = "clear";
        return json(path.endsWith("/stop") ? { ...recorded, warning: stopWarning } : recorded);
      }
      return json({}, 404);
    }),
  );
  return posted;
}

function renderDelete() {
  const projects = fixtureResource(projectsPath, "default") as ProjectsResponse;
  const beta = projects.projects.find((row: Project) => row.id === "beta");
  if (!beta) throw new Error("the fixtures lost beta");
  render(
    <QueryClientProvider client={createQueryClient()}>
      <ConfirmDelete project={beta} onClose={() => {}} />
    </QueryClientProvider>,
  );
  return screen.getByRole("dialog", { name: "Delete beta?" });
}

test("a blocked delete clears each blocker from its own row, then asks for the id", async () => {
  const posted = serve("card");
  const dialog = renderDelete();
  const blocks = await within(dialog).findByRole("region", { name: "Blocks the delete" });
  expect(blocks.textContent).toContain("Builder run on beta-7 is live");
  expect(dialog.textContent).not.toContain("tiller ");
  expect(within(dialog).queryByRole("alert")).toBeNull();
  expect(blocks.parentElement?.parentElement?.textContent).toContain(
    "Not deleted: clear what blocks it first.",
  );
  expect(within(dialog).queryByLabelText("Type beta to confirm")).toBeNull();

  fireEvent.click(within(blocks).getByRole("button", { name: "Cancel card..." }));
  const cancel = screen.getByRole("dialog", { name: "Cancel beta-7?" });
  const reason = within(cancel).getByRole("textbox") as HTMLTextAreaElement;
  expect(reason.value).toBe("deleting project beta");
  fireEvent.change(reason, { target: { value: "deleting project beta, superseded" } });
  fireEvent.click(within(cancel).getByRole("button", { name: "Cancel card" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe("/api/v1/cards/beta-7/cancel");
  expect(posted[0]?.body).toMatchObject({ reason: "deleting project beta, superseded" });

  const stop = await within(blocks).findByRole("button", { name: "Stop agent..." });
  expect(blocks.textContent).toContain("Card cancelled: deleting project beta, superseded");
  expect(blocks.textContent).toContain("Cancelled");
  fireEvent.click(stop);
  const asking = screen.getByRole("dialog", { name: "Stop orchestrator?" });
  fireEvent.click(within(asking).getByRole("button", { name: "Stop agent" }));
  await waitFor(() => expect(posted).toHaveLength(2));
  expect(posted[1]?.path).toBe(`/api/v1/agents/${ORCHESTRATOR}/stop`);

  await within(dialog).findByLabelText("Type beta to confirm");
  const rows = within(blocks)
    .getAllByRole("listitem")
    .map((row) => row.textContent);
  expect(rows).toEqual([
    "Builder run on beta-7 is liveCard cancelled: deleting project beta, supersededCancelled",
    `Orchestrator session ${ORCHESTRATOR} is liveIts pane is closed and its session ended.Stopped`,
  ]);
  expect(within(dialog).queryByRole("alert")).toBeNull();
  expect(within(dialog).getByRole("button", { name: "Delete project" })).toHaveProperty(
    "disabled",
    true,
  );
});

test("an agent on another host is shown as stopped there, with no button and no command", async () => {
  const posted = serve("agent", false);
  const dialog = renderDelete();
  const blocks = await within(dialog).findByRole("region", { name: "Blocks the delete" });
  expect(blocks.textContent).toContain("On its host");
  expect(blocks.textContent).toContain("Only its host can stop it.");
  expect(within(blocks).queryByRole("button")).toBeNull();
  expect(dialog.textContent).not.toContain("tiller ");
  expect(posted).toHaveLength(0);
});

test("a stop that leaves the session live shows its blocker again, with its action", async () => {
  const posted = serve("agent", true, "the pane did not close");
  const dialog = renderDelete();
  const blocks = await within(dialog).findByRole("region", { name: "Blocks the delete" });
  fireEvent.click(within(blocks).getByRole("button", { name: "Stop agent..." }));
  const asking = screen.getByRole("dialog", { name: "Stop orchestrator?" });
  fireEvent.click(within(asking).getByRole("button", { name: "Stop agent" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  await waitFor(() =>
    expect(within(blocks).queryByRole("button", { name: "Stop agent..." })).not.toBeNull(),
  );
  expect(blocks.textContent).not.toContain("Stopped");
  expect(within(dialog).queryByLabelText("Type beta to confirm")).toBeNull();
});
