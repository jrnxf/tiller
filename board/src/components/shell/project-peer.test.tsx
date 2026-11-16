import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Machine, Project, ProjectsResponse } from "../../lib/api-types";
import {
  fixtureMutation,
  resetPeerProjectsFixture,
  withoutDeleted,
} from "../../lib/fixture-mutations";
import { FixtureRefusal, fixtureResource, fixtureResponses } from "../../lib/fixtures";
import { machinesPath, projectsPath, queryKeyFor } from "../../lib/queries";
import { grantsFor, resetGrantsFixture } from "../../lib/fixtures-grants";
import { receiptsStore } from "../../lib/receipts";
import { onProjectGone } from "../../lib/sse";
import { choose } from "../../test-select";
import { createTestQueryClient } from "../../test-app";
import { standingOf } from "../../lib/peers";
import { ArchiveProject } from "./project-archive";
import { AddProject } from "./project-add";
import { ConfirmDelete } from "./project-delete";
import { ProjectDetail } from "./project-stage";
import { ArchivedProjects } from "./projects-archived";

// Fri 13 Nov 2026, 12:00 in Lisbon (winter time is UTC).
const NOW = Date.parse("2026-11-13T12:00:00Z");

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
let reads: string[] = [];
// A write the stand-in daemon holds until the test lets it go.
let hold: Promise<void> | null = null;

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

// The stand-in daemon of the dev server's federated fixture: a forwarded
// project read or write runs on the peer it names.
function stubDaemon() {
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method ?? "GET";
    if (method === "GET") {
      reads.push(path);
      const data = withoutDeleted(path, fixtureResource(path, "federated", NOW), "federated");
      if (data instanceof FixtureRefusal) return json(data.body, data.status);
      return data === undefined ? json({}, 404) : json(data);
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    calls.push({ method, path, body });
    if (hold) await hold;
    const { reply } = fixtureMutation(method, path, body, "federated");
    return json(reply.body, reply.status);
  });
}

beforeEach(() => {
  receiptsStore.set({ byKey: {}, progress: {} });
  resetPeerProjectsFixture();
  resetGrantsFixture(NOW);
  calls = [];
  reads = [];
  hold = null;
  stubDaemon();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const responses = fixtureResponses("federated", NOW);
const machines = (responses[machinesPath] as { machines: Machine[] }).machines;
const projects = (responses[projectsPath] as ProjectsResponse).projects;
const peerGamma = projects.find((row) => row.id === "gamma" && row.host === "host-d") as Project;

function seeded(): QueryClient {
  const client = createTestQueryClient();
  client.setQueryData(queryKeyFor(machinesPath), { machines });
  client.setQueryData(queryKeyFor(projectsPath), { projects });
  return client;
}

function renderWith(node: React.ReactNode, client = seeded()) {
  render(<QueryClientProvider client={client}>{node}</QueryClientProvider>);
  return client;
}

async function openAdd() {
  renderWith(<AddProject />);
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  return screen.findByRole("dialog");
}

test("Add project picks the host above Id: this host first, then each peer, with why one is off", async () => {
  const dialog = within(await openAdd());
  const trigger = dialog.getByRole("combobox", { name: "Host" });
  expect(trigger.textContent).toMatch(/this host/);
  fireEvent.click(trigger);
  const list = within(await screen.findByRole("listbox"));
  const names = list.getAllByRole("option").map((option) => option.textContent);
  expect(names[0]).toMatch(/this host/);
  expect(names).toEqual(expect.arrayContaining(["host-d", "host-f: view only pairing"]));
  const off = list.getByRole("option", { name: "host-f: view only pairing" });
  expect(off.getAttribute("aria-disabled")).toBe("true");
  expect(list.getByRole("option", { name: /^host-e/ }).getAttribute("aria-disabled")).toBe("true");
});

test("a project added on a peer is checked under that peer's root and sent with its host", async () => {
  const dialog = within(await openAdd());
  await choose(dialog.getByRole("combobox", { name: "Host" }), "host-d");
  expect(dialog.getByText(/Registers a project on host-d/)).toBeTruthy();
  const path = dialog.getByLabelText("Local checkout") as HTMLInputElement;
  expect(path.placeholder).toBe("checkouts_root/my-app on host-d");
  fireEvent.change(dialog.getByLabelText("Id"), { target: { value: "orchard" } });
  fireEvent.change(dialog.getByLabelText(/^Repo/), { target: { value: "example/orchard" } });
  fireEvent.click(dialog.getByRole("button", { name: "Clone" }));
  await waitFor(() => expect(path.placeholder).toBe("/home/user/work/orchard"));
  expect(reads.some((read) => read.includes("/checkout?") && read.includes("host=host-d"))).toBe(
    true,
  );
  fireEvent.click(dialog.getByRole("button", { name: "Add project" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(calls).toEqual([
    expect.objectContaining({
      method: "POST",
      path: "/api/v1/projects",
      body: expect.objectContaining({
        id: "orchard",
        host: "host-d",
        checkout: { path: "", clone: true },
      }),
    }),
  ]);
});

test("a peer's refusal of an add names the peer", async () => {
  const dialog = within(await openAdd());
  await choose(dialog.getByRole("combobox", { name: "Host" }), "host-d");
  fireEvent.change(dialog.getByLabelText("Id"), { target: { value: "gamma" } });
  const add = dialog.getByRole("button", { name: "Add project" }) as HTMLButtonElement;
  await waitFor(() => expect(add.disabled).toBe(false));
  fireEvent.click(add);
  expect(
    await dialog.findByText("host-d refused it: project gamma: project id is taken"),
  ).toBeTruthy();
});

test("a clone counts its time, so the dialog never looks stuck", async () => {
  const gate: { resolve?: () => void } = {};
  hold = new Promise((resolve) => {
    gate.resolve = resolve;
  });
  const dialog = within(await openAdd());
  await choose(dialog.getByRole("combobox", { name: "Host" }), "host-d");
  fireEvent.change(dialog.getByLabelText("Id"), { target: { value: "orchard" } });
  fireEvent.change(dialog.getByLabelText(/^Repo/), { target: { value: "example/orchard" } });
  fireEvent.click(dialog.getByRole("button", { name: "Clone" }));
  const add = dialog.getByRole("button", { name: "Add project" }) as HTMLButtonElement;
  await waitFor(() => expect(add.disabled).toBe(false));
  fireEvent.click(add);
  const status = await waitFor(() => {
    const found = dialog.getAllByRole("status").find((one) => one.matches(".pa-progress"));
    if (!found) throw new Error("no clone progress yet");
    return found;
  });
  expect(status.textContent).toMatch(
    /^Cloning on host-d: 0:0\d so far\. A clone can take up to 10 min\.$/,
  );
  const first = status.textContent;
  await waitFor(() => expect(status.textContent).not.toBe(first), { timeout: 2500 });
  expect(dialog.getByRole("button", { name: "Cloning..." })).toBeTruthy();
  gate.resolve?.();
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("a peer's project is deleted on the peer, with its id typed and its host sent", async () => {
  const client = renderWith(<ConfirmDelete project={peerGamma} onClose={() => {}} />);
  const dialog = within(await screen.findByRole("dialog"));
  expect(dialog.getByRole("heading", { name: "Delete gamma on host-d?" })).toBeTruthy();
  expect(reads).toContain("/api/v1/projects/gamma/delete?host=host-d");
  const button = dialog.getByRole("button", { name: /^Delete/ }) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  fireEvent.change(await dialog.findByRole("textbox"), { target: { value: "gamma" } });
  fireEvent.click(button);
  await waitFor(() => expect(calls).toHaveLength(1));
  expect(calls[0]).toEqual({
    method: "POST",
    path: "/api/v1/projects/gamma/delete",
    body: expect.objectContaining({ confirm: "gamma", host: "host-d" }),
  });
  // This host's gamma stays: only the peer's row is gone.
  await waitFor(() => {
    const left = client.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath))?.projects ?? [];
    expect(left.some((row) => row.id === "gamma" && row.host === "host-d")).toBe(false);
    expect(left.some((row) => row.id === "gamma" && !row.host)).toBe(true);
  });
});

test("a peer's project is archived on the peer, and its archived list shows it with its host", async () => {
  // gamma on host-d has an open card, so archive a project with none.
  const made = fixtureMutation(
    "POST",
    projectsPath,
    { id: "orchard", delivery: "direct-pr", capacity: 2, host: "host-d" },
    "federated",
  ).reply.body as { data: Project };
  const orchard = made.data;
  renderWith(<ArchiveProject project={orchard} standing={standingOf(orchard, machines)} />);
  fireEvent.click(screen.getByRole("button", { name: "Archive" }));
  const dialog = within(await screen.findByRole("dialog"));
  expect(dialog.getByRole("heading", { name: "Archive orchard on host-d?" })).toBeTruthy();
  expect(reads).toContain("/api/v1/projects/orchard/retire?host=host-d");
  const archive = dialog.getByRole("button", { name: /^Archive/ }) as HTMLButtonElement;
  await waitFor(() => expect(archive.disabled).toBe(false));
  fireEvent.click(archive);
  await waitFor(() => expect(calls).toHaveLength(1));
  expect(calls[0]?.body).toEqual(expect.objectContaining({ host: "host-d" }));
  cleanup();
  renderWith(<ArchivedProjects />);
  fireEvent.click(await screen.findByRole("button", { name: /Archived/ }));
  const row = await screen.findByText("orchard");
  expect(reads).toContain("/api/v1/projects?retired=true&host=host-d");
  expect(within(row.closest(".prow") as HTMLElement).getByText("host-d")).toBeTruthy();
});

test("a grant made and revoked on a peer's project is the peer's, sent with its host", async () => {
  renderWith(<ProjectDetail project={peerGamma} id="detail" />);
  fireEvent.click(await screen.findByRole("button", { name: "Grants and settings" }));
  const dialog = within(await screen.findByRole("dialog", { name: "gamma on host-d" }));
  expect(await dialog.findByText(/No grant is active/)).toBeTruthy();

  fireEvent.click(dialog.getByRole("button", { name: "New grant" }));
  const form = within(await screen.findByRole("dialog", { name: "New grant for gamma" }));
  // The base branch is the peer's own default branch.
  await waitFor(() =>
    expect((form.getByLabelText(/^Base branch/) as HTMLInputElement).value).toBe("main"),
  );
  fireEvent.click(form.getByRole("button", { name: "Create grant" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "New grant for gamma" })).toBeNull(),
  );
  expect(calls).toEqual([
    expect.objectContaining({
      method: "POST",
      path: "/api/v1/projects/gamma/grants",
      body: expect.objectContaining({ kind: "merge", host: "host-d" }),
    }),
  ]);
  const made = grantsFor("gamma", "host-d").grants[0];
  expect(made).toBeTruthy();
  // This host's gamma has a grant of its own, and it is left as it was.
  expect(grantsFor("gamma").grants.some((grant) => grant.id === made?.id)).toBe(false);
  expect(reads).toContain("/api/v1/projects/gamma/grants?host=host-d");

  fireEvent.click(await dialog.findByRole("button", { name: "Revoke..." }));
  const confirm = within(await screen.findByRole("dialog", { name: /^Revoke grant / }));
  fireEvent.click(confirm.getByRole("button", { name: "Revoke grant" }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: /^Revoke grant / })).toBeNull());
  expect(calls[1]).toEqual({
    method: "DELETE",
    path: `/api/v1/grants/${made?.id}`,
    body: { host: "host-d", idempotency_key: expect.any(String) },
  });
  expect(grantsFor("gamma", "host-d").grants[0]?.revoked_at).toBeTruthy();
});

// This host's own gamma, beside host-d's.
const localGamma = (row: Project) => row.id === "gamma" && !row.host;

test("a peer's stage switch is sent with its host and moves that peer's project", async () => {
  renderWith(<ProjectDetail project={peerGamma} id="detail" />);
  fireEvent.click(await screen.findByRole("button", { name: "Switch to pre-launch" }));
  const dialog = within(await screen.findByRole("dialog"));
  fireEvent.click(dialog.getByRole("button", { name: "Switch to pre-launch" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(calls).toEqual([
    {
      method: "POST",
      path: "/api/v1/projects/gamma/stage",
      body: {
        rev: peerGamma.rev,
        stage: "pre-launch",
        host: "host-d",
        idempotency_key: expect.any(String),
      },
    },
  ]);
  const now = withoutDeleted(
    projectsPath,
    fixtureResource(projectsPath, "federated", NOW),
    "federated",
  ) as ProjectsResponse;
  const moved = now.projects.find((row) => row.id === "gamma" && row.host === "host-d");
  expect(moved?.stage).toBe("pre-launch");
  // This host's gamma stays as it was.
  expect(now.projects.find(localGamma)?.stage).toBe(projects.find(localGamma)?.stage);
});

test("a peer's project read is refused while its link is down or its pairing only reads", () => {
  const down = fixtureResource("/api/v1/projects/ledger/delete?host=host-e", "federated", NOW);
  expect(down).toBeInstanceOf(FixtureRefusal);
  expect((down as FixtureRefusal).status).toBe(503);
  const read = fixtureResource("/api/v1/projects/atlas/retire?host=host-f", "federated", NOW);
  expect((read as FixtureRefusal).status).toBe(502);
  expect(
    fixtureMutation("POST", "/api/v1/projects/ledger/delete", { host: "host-e", confirm: "ledger" })
      .reply.status,
  ).toBe(503);
});

test("a project gone on a peer leaves this host's project of the same id", () => {
  const client = seeded();
  onProjectGone(client, "gamma", "host-d");
  const left = client.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath))?.projects ?? [];
  expect(left.filter((row) => row.id === "gamma").map((row) => row.host ?? null)).toEqual([null]);
  onProjectGone(client, "gamma");
  const after = client.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath))?.projects ?? [];
  expect(after.some((row) => row.id === "gamma")).toBe(false);
});
