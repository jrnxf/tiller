import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { Machine, Project, ProjectFields, ProjectsResponse } from "../../lib/api-types";
import { fixtureResponses } from "../../lib/fixtures";
import { machinesPath, projectsPath, queryKeyFor } from "../../lib/queries";
import { ProjectDetail } from "./project-stage";
import { createTestQueryClient } from "../../test-app";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

const alpha = (fixtureResponses()[projectsPath] as ProjectsResponse).projects.find(
  (row: Project) => row.id === "alpha",
) as Project;

const FIELDS: ProjectFields = {
  id: "alpha",
  name: "alpha",
  repo: "example/alpha",
  remote: "git@example.com:example/alpha.git",
  default_branch: "main",
  delivery: "direct-pr",
  checks: { full: ["cargo test"], no_logic: [] },
  capacity: 2,
  deploy_workflow: "deploy.yml",
  deploy_environment: "production",
  deploy_timeout_minutes: 20,
  rev: alpha.rev,
};

// The project the daemon stands in for, and every PATCH it is sent. `other`
// is an edit made elsewhere before the first save. A read of the peer
// host-b's copy (`?host=host-b`) is answered from the same project, and
// lands in `reads`.
function serve(other?: Partial<ProjectFields>, reads: string[] = []) {
  let stored = { ...FIELDS };
  let otherPending = other;
  const patches: Record<string, unknown>[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      reads.push(String(input));
      const path = String(input).replace("?host=host-b", "");
      if (path === "/api/v1/session") return json({ owner: true });
      if (path.endsWith("/setup")) {
        return json({
          project: "alpha",
          repo: stored.repo,
          merge_gate: "detect",
          merge_gate_changed_at: null,
          checkout: "/home/user/src/alpha",
          config_error: null,
        });
      }
      if (path === "/api/v1/projects/alpha" && init?.method === "PATCH") {
        const body = JSON.parse(String(init.body)) as Record<string, unknown>;
        patches.push(body);
        if (otherPending) {
          stored = { ...stored, ...otherPending, rev: stored.rev + 1 };
          otherPending = undefined;
        }
        if (body.rev !== stored.rev) {
          return json(
            {
              ok: false,
              error: {
                code: "stale",
                message: `conflict: project alpha is at rev ${stored.rev}, you sent ${String(body.rev)}`,
                current: { rev: stored.rev, edited_at: "2026-11-03T14:00:00Z" },
              },
            },
            409,
          );
        }
        const { rev: _rev, idempotency_key: _key, host: _host, ...changes } = body;
        stored = { ...stored, ...(changes as Partial<ProjectFields>), rev: stored.rev + 1 };
        return json({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: "2026-11-11T10:00:00Z", outbox_id: null, approval_id: null },
          data: { ...alpha, rev: stored.rev },
        });
      }
      if (path === "/api/v1/projects/alpha/grants") return json({ project: "alpha", grants: [] });
      if (path === "/api/v1/projects/alpha") return json(stored);
      return json({}, 404);
    }),
  );
  return patches;
}

// host-b as the Machines read lists it: live with the `act` scope unless
// `machine` says otherwise.
function peerB(machine: Partial<Machine> = {}): Machine {
  return {
    id: "host-b-id",
    label: "host-b",
    kind: "remote",
    view_only: false,
    running_agents: null,
    capacity: null,
    free_slots: null,
    last_seen_at: "2026-11-13T10:00:00Z",
    stale_reason: null,
    link: "live",
    peer_scope: "act",
    ...machine,
  } as Machine;
}

function renderDetail(project: Project = alpha, machines: Machine[] = []) {
  const client = createTestQueryClient();
  client.setQueryData(queryKeyFor(machinesPath), { machines });
  render(
    <QueryClientProvider client={client}>
      <ProjectDetail project={project} id="detail" />
    </QueryClientProvider>,
  );
}

async function openEdit() {
  fireEvent.click(await screen.findByRole("button", { name: "Grants and settings" }));
  const dialog = await screen.findByRole("dialog", { name: "alpha" });
  // The dialog opens on the grants; the fields are under Edit.
  fireEvent.click(within(dialog).getByRole("button", { name: "Edit" }));
  // The form shows once this open's own read is back.
  await within(dialog).findByLabelText("Name");
  return within(dialog);
}

test("Edit opens on the stored fields and saves only what changed, against the rev it read", async () => {
  const patches = serve();
  renderDetail();
  const dialog = await openEdit();
  expect((dialog.getByLabelText("Name") as HTMLInputElement).value).toBe("alpha");
  expect((dialog.getByLabelText(/^Repo/) as HTMLInputElement).value).toBe("example/alpha");
  expect((dialog.getByLabelText("Checks that must pass") as HTMLTextAreaElement).value).toBe(
    "cargo test",
  );
  expect(dialog.getByRole("button", { name: "No checks" }).getAttribute("aria-pressed")).toBe(
    "true",
  );
  const save = dialog.getByRole("button", { name: "Save" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);

  fireEvent.click(dialog.getByRole("button", { name: "One slot more" }));
  fireEvent.change(dialog.getByLabelText(/^Remote/), { target: { value: "" } });
  fireEvent.click(dialog.getByRole("button", { name: "Same checks" }));
  fireEvent.change(dialog.getByLabelText("Checks that must pass"), {
    target: { value: "cargo test\n\n  cargo clippy  \n" },
  });
  expect(save.disabled).toBe(false);
  fireEvent.click(save);

  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(patches).toEqual([
    {
      capacity: 3,
      remote: "",
      checks: ["cargo test", "cargo clippy"],
      rev: alpha.rev,
      idempotency_key: expect.any(String),
    },
  ]);
  expect(await screen.findByText(/Project saved/)).toBeTruthy();
});

test("a field left empty or a bad timeout keeps Save off", async () => {
  serve();
  renderDetail();
  const dialog = await openEdit();
  const save = dialog.getByRole("button", { name: "Save" }) as HTMLButtonElement;
  fireEvent.change(dialog.getByLabelText("Base branch"), { target: { value: " " } });
  expect(save.disabled).toBe(true);
  expect(dialog.getByLabelText("Base branch").getAttribute("aria-invalid")).toBe("true");
  fireEvent.change(dialog.getByLabelText("Base branch"), { target: { value: "trunk" } });
  expect(save.disabled).toBe(false);
  fireEvent.change(dialog.getByLabelText("Timeout"), { target: { value: "0" } });
  expect(save.disabled).toBe(true);
});

test("a project changed elsewhere is not overwritten: the dialog says what moved and loads it under the owner's change", async () => {
  const patches = serve({ capacity: 5 });
  renderDetail();
  const dialog = await openEdit();
  fireEvent.change(dialog.getByLabelText(/^Deploy environment/), {
    target: { value: "staging" },
  });
  fireEvent.click(dialog.getByRole("button", { name: "Save" }));

  const alert = await dialog.findByText(/Builder slots is now 5\./);
  expect(alert.textContent).toBe(
    "Not saved: alpha changed on Tue 3 Nov, 14:00, after you opened it. Builder slots is now 5. Load the new values, then save again. Your changes to other fields stay.",
  );
  fireEvent.click(dialog.getByRole("button", { name: "Load new values" }));
  expect(dialog.getByRole("group", { name: "Builder slots" }).textContent).toContain("5");
  expect((dialog.getByLabelText(/^Deploy environment/) as HTMLInputElement).value).toBe("staging");
  expect(dialog.queryByRole("alert")).toBeNull();

  fireEvent.click(dialog.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(patches.at(-1)).toEqual({
    deploy_environment: "staging",
    rev: alpha.rev + 1,
    idempotency_key: expect.any(String),
  });
});

const onB: Project = { ...alpha, host: "host-b", link: "live" };

test("a peer's project is edited from here: the read and the save go to that peer", async () => {
  const reads: string[] = [];
  const patches = serve(undefined, reads);
  renderDetail(onB, [peerB()]);
  // Grants stay on their own host, so the button opens the fields alone.
  expect(screen.queryByRole("button", { name: "Grants and settings" })).toBeNull();
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  const dialog = within(await screen.findByRole("dialog", { name: "alpha on host-b" }));
  expect(dialog.getByText(/Changes this project on host-b/)).toBeTruthy();
  expect(dialog.queryByRole("button", { name: "Grants" })).toBeNull();
  await dialog.findByLabelText("Name");
  expect(reads).toContain("/api/v1/projects/alpha?host=host-b");
  expect(reads.some((path) => path.includes("/grants"))).toBe(false);

  fireEvent.click(dialog.getByRole("button", { name: "One slot more" }));
  fireEvent.click(dialog.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(patches).toEqual([
    { capacity: 3, rev: alpha.rev, host: "host-b", idempotency_key: expect.any(String) },
  ]);
});

test("a peer's refusal of an edit names the peer", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === "/api/v1/session") return json({ owner: true });
      if (path === "/api/v1/projects/alpha" && init?.method === "PATCH") {
        return json(
          {
            ok: false,
            error: { code: "refused", message: "project alpha is retired", host: "host-b" },
          },
          422,
        );
      }
      if (path === "/api/v1/projects/alpha?host=host-b") return json(FIELDS);
      return json({}, 404);
    }),
  );
  renderDetail(onB, [peerB()]);
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  const dialog = within(await screen.findByRole("dialog", { name: "alpha on host-b" }));
  await dialog.findByLabelText("Name");
  fireEvent.click(dialog.getByRole("button", { name: "One slot more" }));
  fireEvent.click(dialog.getByRole("button", { name: "Save" }));
  expect((await dialog.findByRole("alert")).textContent).toBe(
    "host-b refused it: project alpha is retired",
  );
});

test("a peer the board cannot act on shows Edit, Archive and Delete off, with the reason", async () => {
  serve();
  const cases: [Machine, string, string][] = [
    [peerB({ peer_scope: "read" }), "view only pairing", "host-b: view only pairing"],
    [
      peerB({ link: "down" }),
      "host-b is unreachable since Fri 10:00",
      "host-b is unreachable since Fri 10:00",
    ],
  ];
  for (const [machine, reason, shown] of cases) {
    renderDetail({ ...onB, link: machine.link ?? null }, [machine]);
    // A tooltip does not show on a touch screen, so the panel says it too.
    expect(await screen.findByText(shown, { selector: ".pdetail-off" })).toBeTruthy();
    for (const name of ["Edit", "Archive", "Delete"]) {
      const button = (await screen.findByRole("button", { name })) as HTMLButtonElement;
      expect(button.disabled).toBe(true);
      expect(button.title).toBe(reason);
    }
    cleanup();
  }
});

test("the fleet project has an Edit button that opens the fields alone, with no grants", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === "/api/v1/session") return json({ owner: true });
      if (path === "/api/v1/projects/fleet") {
        return json({ ...FIELDS, id: "fleet", name: "fleet", repo: null, remote: null });
      }
      return json({}, 404);
    }),
  );
  renderDetail({ ...alpha, id: "fleet", name: "fleet" });
  expect(screen.queryByRole("button", { name: "Grants and settings" })).toBeNull();
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  const dialog = within(await screen.findByRole("dialog", { name: "fleet" }));
  await dialog.findByLabelText("Name");
  expect(dialog.queryByRole("button", { name: "Grants" })).toBeNull();
});
