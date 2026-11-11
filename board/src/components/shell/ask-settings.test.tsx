import { QueryClientProvider } from "@tanstack/react-query";
import type { Editor } from "@tiptap/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AgentRow, AskRecorded, BoardResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureSnapshot } from "../../lib/fixtures";
import { boardPath, queryKeyFor } from "../../lib/queries";
import { AskDialog } from "./ask-dialog";
import { createTestQueryClient, loadAskEditor } from "../../test-app";

await loadAskEditor();

interface Call {
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
// The settings a start of gamma still lacks; empty once the owner has saved.
let lacking: string[] = [];
// The host a refusal came from, for a peer's start.
let refusedBy: string | undefined;

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function ok(data: unknown): Response {
  return json({
    ok: true,
    replayed: false,
    events: [],
    receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
    data,
  });
}

const problem = (key: string) => ({
  key,
  kind: "missing",
  problem: `${key} is not set; tiller orchestrator start needs it`,
});

function stub() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const body = JSON.parse(String(init?.body ?? "{}"));
      if (init?.method !== "POST") {
        if (path === "/api/v1/session") return json({ owner: true });
        if (path === "/api/v1/start-settings") {
          return json({
            pool_root: null,
            checkouts_root: lacking.includes("spawn.checkouts_root") ? null : "/home/user/src",
            orchestrator_home: null,
            scratch_root: null,
            problems: [],
            config_error: null,
          });
        }
        return json({}, 404);
      }
      calls.push({ path, body });
      if (path === "/api/v1/config") {
        if (body.value === "relative") {
          return json(
            { ok: false, error: { code: "usage", message: "spawn.pool_root must be absolute" } },
            400,
          );
        }
        lacking = lacking.filter((key) => key !== body.key);
        return ok({ key: body.key, value: body.value, changed: true });
      }
      if (path.endsWith("/orchestrator")) {
        if (lacking.length > 0) {
          return json(
            {
              ok: false,
              error: {
                code: "usage",
                message: `config.toml: ${lacking[0]} is not set`,
                current: { settings: lacking.map(problem) },
                host: refusedBy,
              },
            },
            400,
          );
        }
        return ok({});
      }
      const data: AskRecorded = {
        ask: {
          id: "01JASK00000000000000000001",
          project_id: "gamma",
          session_id: null,
          source: "board",
          text: String(body.text),
          received_at: "2026-11-05T12:00:00Z",
        },
        message: null,
        warning: null,
      };
      return ok(data);
    }),
  );
}

const RUNNING = [{ project_id: "alpha" }] as AgentRow[];

function renderDialog(onClose = vi.fn()) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("federated"));
  const board: BoardResponse = { rows: [], asked: [] };
  queryClient.setQueryData(queryKeyFor(boardPath()), board);
  queryClient.setQueryData(queryKeyFor("/api/v1/agents"), { orchestrators: RUNNING });
  render(
    <QueryClientProvider client={queryClient}>
      <AskDialog open onClose={onClose} />
    </QueryClientProvider>,
  );
  return { onClose };
}

beforeEach(() => {
  calls = [];
  refusedBy = undefined;
  lacking = ["checkouts.gamma", "spawn.pool_root", "spawn.checkouts_root"];
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const prose = () => document.querySelector(".ProseMirror") as HTMLElement & { editor: Editor };

function ask(...parts: string[]) {
  act(() => {
    prose().editor.commands.setContent(
      {
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: [
              { type: "mention", attrs: { id: parts[0], label: parts[0] } },
              { type: "text", text: " go" },
            ],
          },
        ],
      },
      { emitUpdate: true },
    );
  });
}

const sendButton = () =>
  screen.getByRole("button", { name: /^(send|retry)/i }) as HTMLButtonElement;

test("a start refused for settings names every one, sets them here and retries", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  ask("gamma");
  fireEvent.click(sendButton());

  const fix = await screen.findByTestId("start-settings-fix");
  expect(fix.textContent).toContain("3 settings are missing or wrong");
  const folder = within(screen.getByTestId("setting-checkouts.gamma"));
  const pool = within(screen.getByTestId("setting-spawn.pool_root"));
  const root = within(screen.getByTestId("setting-spawn.checkouts_root"));
  // Only the checkout has a default, and only once the folder it sits in is known.
  expect((folder.getByRole("textbox") as HTMLInputElement).value).toBe("");
  expect((pool.getByRole("textbox") as HTMLInputElement).value).toBe("");

  fireEvent.change(root.getByRole("textbox"), { target: { value: "/home/user/src" } });
  fireEvent.click(root.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(root.getByRole("button", { name: "Saved" })).toBeTruthy());
  await waitFor(() =>
    expect((folder.getByRole("textbox") as HTMLInputElement).value).toBe("/home/user/src/gamma"),
  );
  fireEvent.click(folder.getByRole("button", { name: "Save" }));
  fireEvent.change(pool.getByRole("textbox"), { target: { value: "/home/user/pool" } });
  fireEvent.click(pool.getByRole("button", { name: "Save" }));
  await screen.findByText("Saved. Press Retry to send.");
  expect(
    calls.filter((call) => call.path === "/api/v1/config").map((call) => call.body.key),
  ).toEqual(["spawn.checkouts_root", "checkouts.gamma", "spawn.pool_root"]);

  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
});

test("a refused value is shown in the daemon's words and nothing is marked saved", async () => {
  stub();
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  ask("gamma");
  fireEvent.click(sendButton());
  const pool = within(await screen.findByTestId("setting-spawn.pool_root"));
  fireEvent.change(pool.getByRole("textbox"), { target: { value: "relative" } });
  fireEvent.click(pool.getByRole("button", { name: "Save" }));
  expect((await pool.findByRole("alert")).textContent).toBe("spawn.pool_root must be absolute");
  expect(screen.queryByText("Saved. Press Retry to send.")).toBeNull();
});

test("a peer's missing settings are set on that host, with no input here", async () => {
  stub();
  refusedBy = "host-d";
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  ask("gamma");
  fireEvent.click(sendButton());
  const fix = await screen.findByTestId("start-settings-fix");
  expect(fix.textContent).toContain("3 settings are missing or wrong on host-d");
  expect(within(fix).queryAllByRole("textbox")).toHaveLength(0);
  expect(fix.textContent).toContain("Set on host-d");
  expect(fix.textContent).toContain("tiller config set spawn.pool_root <path>");
});
