import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Project, ProjectsResponse } from "../../lib/api-types";
import { fixtureMutation, withoutDeleted } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureResponses } from "../../lib/fixtures";
import { grantMutation, grantsFor, resetGrantsFixture } from "../../lib/fixtures-grants";
import { projectsPath } from "../../lib/queries";
import { receiptsStore } from "../../lib/receipts";
import { ProjectDetail } from "./project-stage";
import { createTestQueryClient } from "../../test-app";

// Wed 11 Nov 2026, 12:00 in Lisbon (winter time is UTC).
const NOW = Date.parse("2026-11-11T12:00:00Z");

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
// A refusal the stand-in daemon gives the next create instead of the fixture.
let refuseNext: { status: number; code: string; message: string } | null = null;

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

// The stand-in daemon: reads and writes go to the fixture the dev server
// uses, so a made or revoked grant shows on the next read.
function stubDaemon() {
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data = withoutDeleted(path, fixtureResource(path, "default", NOW), "default");
      return data === undefined ? json({}, 404) : json(data);
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    calls.push({ method, path, body });
    if (refuseNext) {
      const { status, ...error } = refuseNext;
      refuseNext = null;
      return json({ ok: false, error }, status);
    }
    const { reply } = fixtureMutation(method, path, body);
    return json(reply.body, reply.status);
  });
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
  receiptsStore.set({ byKey: {}, progress: {} });
  resetGrantsFixture(NOW);
  calls = [];
  refuseNext = null;
  stubDaemon();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const alpha = (fixtureResponses()[projectsPath] as ProjectsResponse).projects.find(
  (row: Project) => row.id === "alpha",
) as Project;

function renderDetail(project: Project = alpha) {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ProjectDetail project={project} id="detail" />
    </QueryClientProvider>,
  );
}

async function openGrants() {
  fireEvent.click(await screen.findByRole("button", { name: "Grants and settings" }));
  const dialog = await screen.findByRole("dialog", { name: "alpha" });
  await within(dialog).findByRole("list", { name: "Active grants" });
  return within(dialog);
}

test("the panel counts the live grants, and the dialog shows each one in plain words", async () => {
  renderDetail();
  const fact = (await screen.findByText("Grants")).closest(".stage-fact") as HTMLElement;
  await within(fact).findByText("2 active");
  expect(within(fact).getByText("1 ends Thu 12 Nov")).toBeTruthy();

  const dialog = await openGrants();
  // The dialog opens on the grants, and New grant takes focus.
  expect(dialog.getByRole("button", { name: "Grants" }).getAttribute("aria-pressed")).toBe("true");
  expect(document.activeElement).toBe(dialog.getByRole("button", { name: "New grant" }));

  const live = within(dialog.getByRole("list", { name: "Active grants" }));
  const rows = live
    .getAllByRole("listitem")
    .filter((item) => item.parentElement?.className === "gr-list");
  expect(rows).toHaveLength(2);
  const [first, second] = rows as [HTMLElement, HTMLElement];
  expect(within(first).getByText("Squash into main")).toBeTruthy();
  const lines = within(first)
    .getAllByRole("listitem")
    .map((item) => item.textContent);
  expect(lines).toEqual([
    "Base main, head alpha-*",
    "Only PRs a builder of alpha opened",
    "Every check on the PR must pass",
    "code-review must pass on the current PR head, with no high finding",
    "Never touches migrations/** or the source-map paths",
  ]);
  expect(first.textContent).toContain(
    "Ends Thu 12 Nov, 18:00 · used 3 times · made by you, Mon 9 Nov",
  );
  expect(within(second).getByText("Merge or squash into main")).toBeTruthy();
  expect(second.textContent).toContain(
    "code-review or spec-review must pass on the current PR head",
  );
  expect(second.textContent).toContain("No end date · used 11 times · made by you, Tue 3 Nov");

  // B11: a review counts only on the PR's current head.
  expect(
    dialog.getByText(
      "A new push to the PR makes an old review stale, so the grant waits for a fresh one.",
    ),
  ).toBeTruthy();

  // The revoked grant is listed under Ended, with no Revoke of its own.
  const ended = dialog.getByRole("list", { name: "Ended grants" });
  expect(ended.textContent).toContain("Merge into main");
  expect(ended.textContent).toContain("Revoked Sat 7 Nov · used 4 times");
  expect(within(ended).queryByRole("button")).toBeNull();
});

test("New grant sends the conditions tiller grant create takes, and the grant joins the list", async () => {
  renderDetail();
  const dialog = await openGrants();
  fireEvent.click(dialog.getByRole("button", { name: "New grant" }));
  const form = within(await screen.findByRole("dialog", { name: "New grant for alpha" }));
  // The base is the project's default branch once its read is back.
  await waitFor(() =>
    expect((form.getByLabelText(/^Base branch/) as HTMLInputElement).value).toBe("main"),
  );
  // The first field takes focus once the form replaces the read's Cancel.
  expect(document.activeElement).toBe(form.getByLabelText("merge"));

  fireEvent.click(form.getByLabelText("merge"));
  fireEvent.click(form.getByLabelText("rebase"));
  fireEvent.change(form.getByLabelText(/^Head/), { target: { value: "alpha-*" } });
  // No kind chosen is any kind, so the field is left out until one is.
  fireEvent.click(form.getByLabelText("ship"));
  fireEvent.click(form.getByLabelText("ops"));
  fireEvent.click(form.getByLabelText("spec-review"));
  // A review appears with the high findings choice, None by default.
  expect(form.getByRole("button", { name: "None" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.change(form.getByLabelText(/^Never touches/), {
    target: { value: "migrations/**, infra/*" },
  });
  fireEvent.change(form.getByLabelText(/^Ends/), { target: { value: "2026-11-20T18:00" } });
  fireEvent.click(form.getByRole("button", { name: "Create grant" }));

  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "New grant for alpha" })).toBeNull(),
  );
  expect(calls).toEqual([
    {
      method: "POST",
      path: "/api/v1/projects/alpha/grants",
      body: {
        kind: "merge",
        conditions: {
          v: 1,
          actions: ["rebase"],
          base_ref: "main",
          head_ref_pattern: "alpha-*",
          opened_by_fleet_run: true,
          checks: "success",
          // alpha ships through no-mistakes, so its pipeline must pass too.
          pipeline: "passed",
          card_kinds: ["ship", "ops"],
          reviews: { require: ["spec-review"], max_high: 0 },
          exclude_paths: ["migrations/**", "infra/*"],
        },
        expires: "2026-11-20T18:00",
        idempotency_key: expect.any(String),
      },
    },
  ]);
  const live = await dialog.findByText("Rebase into main");
  expect(live.closest("li")?.textContent).toContain("Ends Fri 20 Nov, 18:00 · not used yet");
  expect(live.closest("li")?.textContent).toContain("Only ship or ops cards");
  expect(await dialog.findByText(/Grant made/)).toBeTruthy();
  expect(grantsFor("alpha").grants[0]?.granted_by).toBe("board");
});

test("New grant keeps Create off with no merge action or a past end, and shows the daemon's refusal", async () => {
  renderDetail();
  const dialog = await openGrants();
  fireEvent.click(dialog.getByRole("button", { name: "New grant" }));
  const form = within(await screen.findByRole("dialog", { name: "New grant for alpha" }));
  await waitFor(() =>
    expect((form.getByLabelText(/^Base branch/) as HTMLInputElement).value).toBe("main"),
  );
  const create = form.getByRole("button", { name: "Create grant" }) as HTMLButtonElement;

  fireEvent.click(form.getByLabelText("merge"));
  expect(create.disabled).toBe(true);
  expect(form.getByRole("alert").textContent).toBe("Choose at least one way the grant may merge.");
  fireEvent.click(form.getByLabelText("squash"));
  expect(create.disabled).toBe(false);

  fireEvent.change(form.getByLabelText(/^Ends/), { target: { value: "2026-11-10T09:00" } });
  expect(create.disabled).toBe(true);
  expect(form.getByRole("alert").textContent).toBe(
    "That end time has passed. Choose a later one, or leave it empty.",
  );
  expect(form.getByLabelText(/^Ends/).getAttribute("aria-invalid")).toBe("true");
  fireEvent.change(form.getByLabelText(/^Ends/), { target: { value: "" } });
  expect(create.disabled).toBe(false);
  expect(calls).toEqual([]);

  // A condition core refuses comes back in the daemon's own words.
  const shape = 'grant conditions: unknown key "exclude_path"; version 1 takes v, actions';
  refuseNext = { status: 400, code: "usage", message: shape };
  fireEvent.click(create);
  expect((await form.findByRole("alert")).textContent).toBe(shape);
  expect(calls).toHaveLength(1);

  refuseNext = { status: 403, code: "forbidden", message: "only the owner makes a grant" };
  fireEvent.click(create);
  expect(await form.findByText("only the owner makes a grant")).toBeTruthy();
  // The refused grant is not made, and the dialog stays open for a retry.
  expect(grantsFor("alpha").grants).toHaveLength(3);
  expect(screen.getByRole("dialog", { name: "New grant for alpha" })).toBeTruthy();
});

test("Revoke asks first, Keep sends nothing, and the revoked grant moves to Ended", async () => {
  renderDetail();
  const dialog = await openGrants();
  const revokes = dialog.getAllByRole("button", { name: "Revoke..." });
  expect(revokes).toHaveLength(2);
  // Revoke is secondary: never the primary or the danger look in the list.
  expect(revokes[0]?.className).toBe("btn sm");

  fireEvent.click(revokes[0] as HTMLElement);
  let confirm = within(await screen.findByRole("dialog", { name: /^Revoke grant / }));
  expect(document.activeElement).toBe(confirm.getByRole("button", { name: "Keep grant" }));
  expect(confirm.getByText(/go back to you for approval/).textContent).toBe(
    'PRs that wait on "Squash into main" go back to you for approval. A merge that already started is not stopped.',
  );
  fireEvent.click(confirm.getByRole("button", { name: "Keep grant" }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: /^Revoke grant / })).toBeNull());
  expect(calls).toEqual([]);

  fireEvent.click(dialog.getAllByRole("button", { name: "Revoke..." })[0] as HTMLElement);
  confirm = within(await screen.findByRole("dialog", { name: /^Revoke grant / }));
  const button = confirm.getByRole("button", { name: "Revoke grant" });
  expect(button.className).toBe("btn danger");
  fireEvent.click(button);
  await waitFor(() => expect(screen.queryByRole("dialog", { name: /^Revoke grant / })).toBeNull());
  const id = grantsFor("alpha").grants.find((grant) => grant.revoked_at && grant.uses === 3)?.id;
  expect(calls).toEqual([
    {
      method: "DELETE",
      path: `/api/v1/grants/${id}`,
      body: { idempotency_key: expect.any(String) },
    },
  ]);
  await waitFor(() => expect(dialog.getAllByRole("button", { name: "Revoke..." })).toHaveLength(1));
  const ended = dialog.getByRole("list", { name: "Ended grants" });
  expect(ended.textContent).toContain("Squash into main");
  expect(ended.textContent).toContain("Revoked Wed 11 Nov");
  expect(await dialog.findByText(/Grant revoked/)).toBeTruthy();
});

test("a peer's project shows no grants: its own host keeps them", async () => {
  renderDetail({ ...alpha, host: "host-b", link: "live" });
  const button = (await screen.findByRole("button", {
    name: "Grants and settings",
  })) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  expect(screen.queryByText("Grants")).toBeNull();
});

// The stand-in daemon refuses what `tiller grant create` and
// `tiller grant revoke` refuse, so fixture mode cannot show a grant the
// daemon would never make.
const knownHere = (project: string) => project === "alpha";

const create = (body: Record<string, unknown>, project = "alpha") =>
  grantMutation(
    "POST",
    `/api/v1/projects/${project}/grants`,
    { idempotency_key: "k", ...body },
    knownHere,
  );

const revoke = (id: string, body: Record<string, unknown> = {}) =>
  grantMutation("DELETE", `/api/v1/grants/${id}`, { idempotency_key: "k", ...body }, knownHere);

test("the fixture refuses a grant the daemon refuses", () => {
  const merge = { v: 1, actions: ["merge"] };
  expect(create({ kind: "merge", conditions: merge, reason: "x" })).toMatchObject({
    ok: false,
    status: 400,
    message: "unknown field `reason`",
  });
  expect(create({ kind: "deploy", conditions: merge })).toMatchObject({
    status: 400,
    message: "unknown grant kind: deploy",
  });
  expect(create({ kind: "merge", conditions: ["merge"] })).toMatchObject({
    status: 400,
    message: "conditions is a JSON object",
  });
  expect(create({ kind: "merge", conditions: merge, expires: "soon" })).toMatchObject({
    status: 400,
    code: "usage",
  });
  expect(create({ kind: "merge", conditions: merge }, "zeta")).toMatchObject({
    status: 404,
    message: "no project zeta",
  });
  expect(create({ kind: "merge", conditions: { actions: ["merge"] } })).toMatchObject({
    status: 400,
    message: "grant conditions need v: 1",
  });
  expect(create({ kind: "land_direct", conditions: merge })).toMatchObject({
    status: 400,
    message: "a land_direct grant's actions must be some of push",
  });
  expect(create({ kind: "merge", conditions: { v: 1, actions: [] } })).toMatchObject({
    status: 400,
    message: "a merge grant's actions must be some of merge, squash, rebase",
  });
  expect(grantsFor("alpha").grants).toHaveLength(3);

  expect(revoke("nothere")).toMatchObject({ status: 404, message: "no grant nothere" });
  const [live, , ended] = grantsFor("alpha").grants;
  expect(revoke(live?.short ?? "", { reason: "  " })).toMatchObject({
    status: 400,
    message: "reason is required",
  });
  // Revoke is a no-op on a grant already revoked: its first revoke stands.
  expect(revoke(ended?.id ?? "")).toEqual({ ok: true, data: ended });
  expect(revoke(live?.short ?? "")).toMatchObject({
    ok: true,
    data: { revoke_reason: "revoked by the owner" },
  });
});

test("the fixture's board host does not forward a grant change to a peer", () => {
  const { reply } = fixtureMutation("DELETE", "/api/v1/grants/01JGR00000000001ABCDEFGHJK", {
    idempotency_key: "k",
    host: "host-b",
  });
  expect(reply.status).toBe(502);
});
