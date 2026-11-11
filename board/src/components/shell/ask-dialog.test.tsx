import { QueryClientProvider } from "@tanstack/react-query";
import type { Editor } from "@tiptap/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AgentRow, AskRecorded, BoardResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureSnapshot } from "../../lib/fixtures";
import { boardPath, projectsPath, queryKeyFor } from "../../lib/queries";
import { AskDialog } from "./ask-dialog";
import { createTestQueryClient, loadAskEditor } from "../../test-app";

await loadAskEditor();

interface Call {
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
// Paths whose next call fails once, as a daemon refusing it.
let failing = new Set<string>();

function ok(data: unknown): Response {
  return new Response(
    JSON.stringify({
      ok: true,
      replayed: false,
      events: [],
      receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
      data,
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

function stub() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const body = JSON.parse(String(init?.body ?? "{}"));
      calls.push({ path, body });
      const target = `${body.host ?? ""}${path}`;
      if (failing.has(target)) {
        failing.delete(target);
        return new Response(
          JSON.stringify({
            ok: false,
            error: { code: "peer_unreachable", message: "host-d did not answer." },
          }),
          { status: 502 },
        );
      }
      if (path.endsWith("/orchestrator")) return ok({});
      const project = /projects\/([^/]+)\/asks/.exec(path)?.[1] ?? "alpha";
      const data: AskRecorded = {
        ask: {
          id: `01JASK${String(calls.length).padStart(20, "0")}`,
          project_id: project,
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

// Which orchestrators run: alpha and beta here, fleet on host-d. gamma and the
// peers' other projects have none.
const RUNNING = [
  { project_id: "alpha" },
  { project_id: "beta" },
  { project_id: "fleet", host: "host-d" },
] as AgentRow[];

function renderDialog(onClose = vi.fn(), running: AgentRow[] = RUNNING) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("federated"));
  const board: BoardResponse = { rows: [], asked: [] };
  queryClient.setQueryData(queryKeyFor(boardPath()), board);
  queryClient.setQueryData(queryKeyFor("/api/v1/agents"), { orchestrators: running });
  render(
    <QueryClientProvider client={queryClient}>
      <AskDialog open onClose={onClose} />
    </QueryClientProvider>,
  );
  return { queryClient, onClose };
}

beforeEach(() => {
  calls = [];
  failing = new Set();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

type Part = string | { key: string; name: string };
const chip = (name: string, key = name): Part => ({ key, name });

const prose = () => document.querySelector(".ProseMirror") as HTMLElement & { editor: Editor };

// Puts these words and chips in the editor, as typing and picking would.
function write(...parts: Part[]) {
  act(() => {
    prose().editor.commands.setContent(
      {
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: parts.map((part) =>
              typeof part === "string"
                ? { type: "text", text: part }
                : { type: "mention", attrs: { id: part.key, label: part.name } },
            ),
          },
        ],
      },
      { emitUpdate: true },
    );
  });
}

const sendButton = () =>
  screen.getByRole("button", { name: /^(send|retry)/i, hidden: false }) as HTMLButtonElement;
const rows = () => within(screen.getByRole("status")).queryAllByRole("listitem");
const row = (name: string) => screen.getByTestId(`ask-row-${name}`);
const asksTo = (project: string) =>
  calls.filter((call) => call.path === `/api/v1/projects/${project}/asks`);

test("the editor loads on demand and its box is there from the first paint", async () => {
  renderDialog();
  // Before the editor chunk arrives, the box holds its place at the same class,
  // so the dialog does not move when the editor replaces it.
  expect(document.querySelector(".ask-ed")).toBeTruthy();
  expect(prose()).toBeNull();
  await waitFor(() => expect(prose()).toBeTruthy());
  expect(document.querySelectorAll(".ask-ed")).toHaveLength(1);
});

test("it opens with nothing picked and says how to pick", async () => {
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  expect(screen.getByRole("status").textContent).toContain("Type @ to pick the projects");
  expect(sendButton().disabled).toBe(true);
  expect(prose().getAttribute("aria-labelledby")).toBeTruthy();
});

test("typing @ lists the projects, filters as you type and Enter picks one", async () => {
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  act(() => {
    prose().editor.commands.insertContent("@be");
  });
  const list = await screen.findByRole("listbox", { name: "Projects" });
  expect(within(list).getAllByRole("option")).toHaveLength(1);
  expect(within(list).getByRole("option").textContent).toContain("beta");
  fireEvent.keyDown(prose(), { key: "Enter" });
  expect(screen.queryByRole("listbox")).toBeNull();
  expect(calls).toHaveLength(0);
  // The chip reads as the name, with no @.
  expect(document.querySelector(".ask-chip")?.textContent).toBe("beta");
  expect(row("beta")).toBeTruthy();
});

test("Escape closes the list first and the dialog next", async () => {
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  act(() => {
    prose().editor.commands.insertContent("@");
  });
  await screen.findByRole("listbox");
  fireEvent.keyDown(prose(), { key: "Escape" });
  expect(onClose).not.toHaveBeenCalled();
  fireEvent.keyDown(prose(), { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});

test("a project another host owns is listed with why it cannot take an ask", async () => {
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  act(() => {
    prose().editor.commands.insertContent("@ledger");
  });
  const option = await screen.findByRole("option", { name: /ledger/ });
  expect(option.getAttribute("aria-disabled")).toBe("true");
  expect(option.textContent).toMatch(/host-e/);
  fireEvent.click(option);
  expect(rows()).toHaveLength(0);
});

test("one ask goes to each mentioned project, without the leading chips", async () => {
  stub();
  const { onClose, queryClient } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " ", chip("beta"), " optimize fonts");
  expect(rows().map((item) => item.textContent)).toEqual([
    expect.stringContaining("alpha"),
    expect.stringContaining("beta"),
  ]);
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());

  expect(calls.map((call) => call.path)).toEqual([
    "/api/v1/projects/alpha/asks",
    "/api/v1/projects/beta/asks",
  ]);
  expect(asksTo("alpha")[0]?.body.text).toBe("optimize fonts\n\nAlso asked of: beta");
  expect(asksTo("beta")[0]?.body.text).toBe("optimize fonts\n\nAlso asked of: alpha");
  expect(calls[0]?.body.idempotency_key).not.toBe(calls[1]?.body.idempotency_key);
  expect(calls[0]?.body).not.toHaveProperty("host");
  const all = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  expect(all?.asked).toHaveLength(2);
});

test("a chip in the middle of the sentence stays as the plain project name", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " match what ", chip("beta"), " ships");
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(asksTo("alpha")[0]?.body.text).toBe("match what beta ships\n\nAlso asked of: beta");
  expect(asksTo("beta")[0]?.body.text).toBe("match what beta ships\n\nAlso asked of: alpha");
});

test("a single project gets the plain ask with no footer", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), "  ship the footer ");
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(asksTo("alpha")[0]?.body.text).toBe("ship the footer");
});

test("Enter sends and Shift+Enter adds a line", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " one");
  fireEvent.keyDown(prose(), { key: "Enter", shiftKey: true });
  expect(calls).toHaveLength(0);
  expect(prose().querySelectorAll("p")).toHaveLength(2);
  fireEvent.keyDown(prose(), { key: "Enter" });
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(asksTo("alpha")).toHaveLength(1);
});

test("text with no project, or a project with no text, cannot be sent", async () => {
  stub();
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write("optimize fonts");
  expect(screen.getByRole("status").textContent).toContain("Add a project with @ first");
  expect(sendButton().disabled).toBe(true);
  write(chip("alpha"));
  expect(sendButton().disabled).toBe(true);
  fireEvent.keyDown(prose(), { key: "Enter" });
  expect(calls).toHaveLength(0);
});

test("a project can be removed from the summary", async () => {
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " ", chip("beta"), " go");
  fireEvent.click(screen.getByRole("button", { name: "Remove alpha" }));
  expect(rows()).toHaveLength(1);
  expect(prose().textContent).not.toContain("alpha");
});

test("a project with no orchestrator running says so, and the send starts one first", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("gamma"), " ", chip("alpha"), " go");
  expect(row("gamma").textContent).toContain("will start orchestrator");
  expect(row("alpha").textContent).toContain("orchestrator running");
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  const gamma = calls.filter((call) => call.path.startsWith("/api/v1/projects/gamma/"));
  expect(gamma.map((call) => call.path)).toEqual([
    "/api/v1/projects/gamma/orchestrator",
    "/api/v1/projects/gamma/asks",
  ]);
  expect(calls.some((call) => call.path === "/api/v1/projects/alpha/orchestrator")).toBe(false);
});

test("a peer's project is asked through the board host with its host label", async () => {
  stub();
  const { onClose, queryClient } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " ", chip("fleet", "host-d/fleet"), " go");
  expect(row("host / host-d").textContent).toContain("on host-d");
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(asksTo("alpha")[0]?.body).not.toHaveProperty("host");
  expect(asksTo("fleet")[0]?.body.host).toBe("host-d");
  expect(asksTo("fleet")[0]?.body.text).toBe("go\n\nAlso asked of: alpha");
  // Only this host's ask joins the Asked column now; the peer's comes by event.
  const all = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  expect(all?.asked.map((ask) => ask.project_id)).toEqual(["alpha"]);
});

test("a peer's project with no orchestrator is started on its own host", async () => {
  stub();
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("gamma", "host-d/gamma"), " go");
  expect(row("gamma").textContent).toContain("will start orchestrator");
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(calls.map((call) => [call.path, call.body.host])).toEqual([
    ["/api/v1/projects/gamma/orchestrator", "host-d"],
    ["/api/v1/projects/gamma/asks", "host-d"],
  ]);
});

test("a view-only peer cannot take an ask", async () => {
  stub();
  renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  act(() => {
    prose().editor.commands.insertContent("@atlas");
  });
  const option = await screen.findByRole("option", { name: /atlas/ });
  expect(option.getAttribute("aria-disabled")).toBe("true");
  expect(option.textContent).toContain("view only pairing");
  // A chip that got in anyway (the pairing changed) holds the send back.
  write(chip("atlas", "host-f/atlas"), " go");
  expect(row("atlas").textContent).toContain("view only pairing");
  expect(sendButton().disabled).toBe(true);
});

test("a failed project keeps the dialog open and a retry sends only that one", async () => {
  stub();
  failing.add("host-d/api/v1/projects/fleet/asks");
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("alpha"), " ", chip("fleet", "host-d/fleet"), " go");
  fireEvent.click(sendButton());

  await waitFor(() => expect(row("host / host-d").textContent).toContain("Could not send"));
  expect(row("alpha").textContent).toContain("Sent");
  expect(screen.getByRole("alert").textContent).toBe("host-d did not answer.");
  expect(onClose).not.toHaveBeenCalled();
  // The words are locked while some projects are sent.
  expect(prose().getAttribute("contenteditable")).toBe("false");
  expect(sendButton().textContent).toContain("Retry 1 failed");
  expect(asksTo("alpha")).toHaveLength(1);
  const first = asksTo("fleet")[0]?.body.idempotency_key;

  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(asksTo("alpha")).toHaveLength(1);
  expect(asksTo("fleet")).toHaveLength(2);
  expect(asksTo("fleet")[1]?.body.idempotency_key).toBe(first);
});

test("a project whose orchestrator started but whose ask failed retries without starting twice", async () => {
  stub();
  failing.add("/api/v1/projects/gamma/asks");
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  write(chip("gamma"), " go");
  fireEvent.click(sendButton());
  await waitFor(() => expect(row("gamma").textContent).toContain("Could not send"));
  const start = calls[0]?.body.idempotency_key;
  fireEvent.click(sendButton());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  // The start carries the same key, so the daemon replays it.
  const starts = calls.filter((call) => call.path.endsWith("/orchestrator"));
  expect(starts.every((call) => call.body.idempotency_key === start)).toBe(true);
});

test("with the projects not read, Ask still opens and says why Send waits", async () => {
  // Every read fails, as with a daemon that cannot open its store.
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(
          JSON.stringify({
            ok: false,
            error: { code: "unavailable", message: "unavailable: the store is locked" },
          }),
          { status: 503 },
        ),
    ),
  );
  const queryClient = createTestQueryClient();
  const snapshot = fixtureSnapshot("default");
  const queries = { ...snapshot.queries };
  delete queries[projectsPath];
  hydrateFromSnapshot(queryClient, { ...snapshot, queries });
  render(
    <QueryClientProvider client={queryClient}>
      <AskDialog open onClose={vi.fn()} />
    </QueryClientProvider>,
  );

  const dialog = screen.getByRole("dialog", { name: "Ask orchestrators" });
  expect(within(dialog).getByText(/Projects are still loading/)).toBeTruthy();
  expect(sendButton().disabled).toBe(true);

  const reason = await within(dialog).findByText(
    "Projects could not be read: the store is locked. Send waits until they load.",
  );
  expect(reason.getAttribute("data-warn")).toBe("true");
  expect(sendButton().disabled).toBe(true);
});

test("a stray Escape after the list closed closes the dialog", async () => {
  const { onClose } = renderDialog();
  await waitFor(() => expect(prose()).toBeTruthy());
  act(() => {
    prose().editor.commands.insertContent("@");
  });
  await screen.findByRole("listbox");
  fireEvent.keyDown(prose(), { key: "Escape" });
  expect(onClose).not.toHaveBeenCalled();
  // Focus falls to the page, as when Send disables the focused button.
  (document.activeElement as HTMLElement | null)?.blur();
  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});
