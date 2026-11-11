import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Machine } from "../../lib/api-types";
import { fixtureMutation, withoutDeleted } from "../../lib/fixture-mutations";
import type { Scenario } from "../../lib/fixtures";
import { forgetPeerChanges } from "../../lib/fixtures-peers";
import { createQueryClient } from "../../lib/query-client";
import { BoardsReach, PeerLink } from "./peer-link";

function serve(scenario: Scenario, refuse?: () => Response) {
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method;
    if (method && method !== "GET") {
      if (refuse) return refuse();
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      const reply = fixtureMutation(method, path, body, scenario).reply;
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    }
    return new Response(JSON.stringify(withoutDeleted(path, undefined, scenario)));
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

const deletes = (fetch: ReturnType<typeof serve>) =>
  fetch.mock.calls
    .filter(([, init]) => init?.method === "DELETE")
    .map(([input, init]) => ({
      path: String(input),
      body: JSON.parse(String(init?.body)) as Record<string, unknown>,
    }));

const hostD: Machine = {
  id: "01JPEERD0000000000000000D",
  label: "host-d",
  kind: "remote",
  view_only: false,
  health: "ok",
  running_agents: 2,
  capacity: 4,
  free_slots: 2,
  last_seen_at: "2026-11-11T16:04:00.000Z",
  stale_reason: null,
  link: "live",
  peer_scope: "act",
  rev: 3,
};

function renderBoth() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PeerLink machine={hostD} board="host-a" />
      <BoardsReach board="host-a" />
    </QueryClientProvider>,
  );
}

beforeEach(forgetPeerChanges);

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("a paired host's card shows its link, with Revoke key left of Unpair", async () => {
  serve("federated");
  renderBoth();
  const link = await screen.findByRole("region", { name: "Peer link" });
  expect(link.textContent).toContain("Open · last hello Wed 16:04");
  expect(within(link).getByText("SHA256:Qm4x…r9Tk")).toBeTruthy();
  expect(link.textContent).toContain("Actthe board may start and stop agents there");
  expect(link.textContent).toContain("Reaches host-a too");
  expect(link.textContent).toContain("2 agents run on host-d now.");
  const buttons = within(link)
    .getAllByRole("button")
    .map((button) => button.textContent);
  expect(buttons).toEqual(["Revoke key...", "Unpair..."]);
});

test("Unpair arms only once the label is typed, then the host leaves", async () => {
  const fetch = serve("federated");
  renderBoth();
  const link = await screen.findByRole("region", { name: "Peer link" });
  fireEvent.click(within(link).getByRole("button", { name: "Unpair..." }));
  const dialog = await screen.findByRole("dialog", { name: "Unpair host-d?" });
  expect(dialog.textContent).toContain(
    "host-d leaves this board and its key file is deleted. Its projects, cards and agents stay on host-d. Pair it again with Add a host.",
  );
  const confirm = within(dialog).getByRole<HTMLButtonElement>("button", { name: "Unpair host" });
  expect(confirm.disabled).toBe(true);
  fireEvent.change(within(dialog).getByLabelText("Type host-d to confirm"), {
    target: { value: "host-" },
  });
  expect(confirm.disabled).toBe(true);
  fireEvent.change(within(dialog).getByLabelText("Type host-d to confirm"), {
    target: { value: "host-d" },
  });
  expect(confirm.disabled).toBe(false);
  fireEvent.click(confirm);
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  const [sent] = deletes(fetch);
  expect(sent?.path).toBe("/api/v1/peers/host-d");
  expect(typeof sent?.body.idempotency_key).toBe("string");
  await waitFor(() => expect(screen.queryByRole("region", { name: "Peer link" })).toBeNull());
});

test("Revoke key asks first, then the board's row reads Revoked", async () => {
  const fetch = serve("federated");
  renderBoth();
  const boards = await screen.findByRole("region", { name: "Boards that reach this host" });
  expect(boards.textContent).toContain(
    "Boards on other hosts that paired with host-a and may read or act here.",
  );
  const live = within(boards).getByTestId("board-key-host-d");
  expect(live.textContent).toContain("Act · key SHA256:Vb8n…2xC5 · accepted Thu 5 Nov, 21:40");
  const gone = within(boards).getByTestId("board-key-host-c");
  expect(gone.textContent).toContain("Read only · revoked Sat 7 Nov, 12:03");
  expect(within(gone).getByText("Revoked")).toBeTruthy();
  expect(within(gone).queryByRole("button")).toBeNull();

  fireEvent.click(within(live).getByRole("button", { name: "Revoke..." }));
  const dialog = await screen.findByRole("dialog", { name: "Revoke host-d's key?" });
  expect(dialog.textContent).toContain(
    "host-d's board stops reaching host-a at once. Agents here keep running. To let it back, add host-a again from host-d's board.",
  );
  expect(document.activeElement?.textContent).toBe("Cancel");
  fireEvent.click(within(dialog).getByRole("button", { name: "Revoke key" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(deletes(fetch).map((call) => call.path)).toEqual([
    "/api/v1/peer-keys/01JKEYD00000000000000000D1",
  ]);
  await waitFor(() =>
    expect(screen.getByTestId("board-key-host-d").getAttribute("data-revoked")).toBe("true"),
  );
  // Its board no longer reaches here, so the card loses Revoke key.
  const link = screen.getByRole("region", { name: "Peer link" });
  expect(within(link).queryByRole("button", { name: "Revoke key..." })).toBeNull();
});

test("a refusal shows in the dialog and leaves it open", async () => {
  serve(
    "federated",
    () =>
      new Response(
        JSON.stringify({
          ok: false,
          error: { code: "forbidden", message: "the owner unpairs a host" },
        }),
        { status: 403 },
      ),
  );
  renderBoth();
  const link = await screen.findByRole("region", { name: "Peer link" });
  fireEvent.click(within(link).getByRole("button", { name: "Unpair..." }));
  const dialog = await screen.findByRole("dialog", { name: "Unpair host-d?" });
  fireEvent.change(within(dialog).getByLabelText("Type host-d to confirm"), {
    target: { value: "host-d" },
  });
  fireEvent.click(within(dialog).getByRole("button", { name: "Unpair host" }));
  expect((await within(dialog).findByRole("alert")).textContent).toContain(
    "the owner unpairs a host",
  );
  expect(screen.getByRole("dialog", { name: "Unpair host-d?" })).toBeTruthy();
});

const unpair = (label: string) =>
  fixtureMutation("DELETE", `/api/v1/peers/${label}`, { idempotency_key: label }, "federated");

test("the fixture unpairs as the daemon does: a host not paired is a no-op", () => {
  const first = unpair("host-d");
  expect(first.reply.status).toBe(200);
  expect(first.followUps?.map((follow) => follow.event.txn)).toEqual(["peer.remove"]);
  for (const label of ["host-d", "host-b"]) {
    const noop = unpair(label);
    expect(noop.reply.status).toBe(200);
    expect(noop.followUps ?? []).toEqual([]);
  }
  expect(unpair("host-z").reply.status).toBe(404);
});

test("an open Unpair dialog stays when the link ends meanwhile", async () => {
  serve(
    "federated",
    () =>
      new Response(
        JSON.stringify({ ok: false, error: { code: "conflict", message: "host-d changed" } }),
        { status: 409 },
      ),
  );
  const queryClient = createQueryClient();
  render(
    <QueryClientProvider client={queryClient}>
      <PeerLink machine={hostD} board="host-a" />
    </QueryClientProvider>,
  );
  const link = await screen.findByRole("region", { name: "Peer link" });
  fireEvent.click(within(link).getByRole("button", { name: "Unpair..." }));
  await screen.findByRole("dialog", { name: "Unpair host-d?" });
  // A terminal unpairs host-d while the dialog is open.
  fixtureMutation("DELETE", "/api/v1/peers/host-d", { idempotency_key: "cli" }, "federated");
  await queryClient.invalidateQueries();
  await waitFor(() => expect(screen.queryByRole("region", { name: "Peer link" })).toBeNull());
  const dialog = screen.getByRole("dialog", { name: "Unpair host-d?" });
  fireEvent.change(within(dialog).getByLabelText("Type host-d to confirm"), {
    target: { value: "host-d" },
  });
  fireEvent.click(within(dialog).getByRole("button", { name: "Unpair host" }));
  expect((await within(dialog).findByRole("alert")).textContent).toContain("host-d changed");
});

test("a host with no links shows neither block", async () => {
  const fetch = serve("default");
  renderBoth();
  await waitFor(() =>
    expect(fetch.mock.calls.some(([input]) => String(input) === "/api/v1/peers")).toBe(true),
  );
  expect(screen.queryByRole("region", { name: "Peer link" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Boards that reach this host" })).toBeNull();
});
