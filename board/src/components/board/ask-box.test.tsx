import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AskRecorded, BoardResponse, Project } from "../../lib/api-types";
import { boardPath, queryKeyFor } from "../../lib/queries";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { AskBox } from "./ask-box";

const PROJECT = { id: "alpha", name: "Alpha" } as Project;
const PEER_PROJECT = { id: "alpha", name: "Alpha", host: "host-b", link: "live" } as Project;

interface Call {
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
let release: (() => void) | null = null;

function reply(warning: string | null): Response {
  const data: AskRecorded = {
    ask: {
      id: "01JASK0000000000000000001",
      project_id: "alpha",
      session_id: null,
      source: "board",
      text: "ship the footer",
      received_at: "2026-11-05T12:00:00Z",
    },
    message: warning ? null : { id: "01JOUTB", target_session_id: "01JSESS" },
    warning,
  };
  return new Response(
    JSON.stringify({
      ok: true,
      replayed: false,
      events: [],
      receipt: {
        recorded_at: "2026-11-05T12:00:00Z",
        outbox_id: data.message?.id ?? null,
        approval_id: null,
      },
      data,
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

function stub(warning: string | null) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ path: String(input), body: JSON.parse(String(init?.body ?? "{}")) });
      if (release === null) return reply(warning);
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return reply(warning);
    }),
  );
}

function renderBox(project: Project) {
  const queryClient = createQueryClient();
  const board: BoardResponse = { rows: [], asked: [] };
  queryClient.setQueryData(queryKeyFor(boardPath()), board);
  queryClient.setQueryData(queryKeyFor(boardPath({ project: "beta" })), board);
  render(
    <QueryClientProvider client={queryClient}>
      <AskBox project={project} />
    </QueryClientProvider>,
  );
  return queryClient;
}

beforeEach(() => {
  calls = [];
  release = null;
  receiptsStore.set({ byKey: {}, progress: {} });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const input = () => screen.getByLabelText("Ask the Alpha orchestrator", { selector: "textarea" });
const sendButton = () => screen.getByRole("button", { name: /send/i });

test("Send stays off until there is text, then sends it and clears the box", async () => {
  stub(null);
  const queryClient = renderBox(PROJECT);
  expect((sendButton() as HTMLButtonElement).disabled).toBe(true);

  fireEvent.change(input(), { target: { value: "  ship the footer\nthen the header  " } });
  expect((sendButton() as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(sendButton());

  await waitFor(() => expect((input() as HTMLTextAreaElement).value).toBe(""));
  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe("/api/v1/projects/alpha/asks");
  expect(calls[0]?.body.text).toBe("ship the footer\nthen the header");
  expect(typeof calls[0]?.body.idempotency_key).toBe("string");
  expect(calls[0]?.body).not.toHaveProperty("host");
  expect(screen.getByRole("status").textContent).toBe("Sent to the Alpha orchestrator.");

  // The ask tile is on the board's reads for this project, before any event.
  const all = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  expect(all?.asked.map((ask) => ask.id)).toEqual(["01JASK0000000000000000001"]);
  const other = queryClient.getQueryData<BoardResponse>(
    queryKeyFor(boardPath({ project: "beta" })),
  );
  expect(other?.asked).toEqual([]);
});

test("Enter sends, Shift+Enter adds a line", async () => {
  stub(null);
  renderBox(PROJECT);
  fireEvent.change(input(), { target: { value: "one" } });
  fireEvent.keyDown(input(), { key: "Enter", shiftKey: true });
  expect(calls).toHaveLength(0);
  fireEvent.keyDown(input(), { key: "Enter" });
  await waitFor(() => expect(calls).toHaveLength(1));
});

test("the box is read-only and Send says Sending while the ask is in flight, and sends once", async () => {
  release = () => {};
  stub(null);
  renderBox(PROJECT);
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() => expect(sendButton().textContent).toContain("Sending..."));
  expect((sendButton() as HTMLButtonElement).disabled).toBe(true);
  expect((input() as HTMLTextAreaElement).readOnly).toBe(true);
  fireEvent.keyDown(input(), { key: "Enter" });
  expect(calls).toHaveLength(1);
  await act(async () => release?.());
  await waitFor(() => expect((input() as HTMLTextAreaElement).value).toBe(""));
  expect(calls).toHaveLength(1);
});

test("with no orchestrator running it says the ask is recorded and will be picked up", async () => {
  stub("no_orchestrator");
  renderBox(PROJECT);
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() =>
    expect(screen.getByRole("status").textContent).toBe(
      "Recorded. No orchestrator is running for Alpha: it will pick this up when it starts.",
    ),
  );
});

test("a failed send keeps the text and says why", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(
          JSON.stringify({
            ok: false,
            error: { code: "read_only", message: "The store is read only." },
          }),
          { status: 409 },
        ),
    ),
  );
  renderBox(PROJECT);
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() =>
    expect(screen.getByRole("alert").textContent).toBe("The store is read only."),
  );
  expect((input() as HTMLTextAreaElement).value).toBe("hello");
});

test("a peer's project cannot take an ask here and says where to ask", () => {
  stub(null);
  renderBox(PEER_PROJECT);
  expect((input() as HTMLTextAreaElement).disabled).toBe(true);
  expect((sendButton() as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByRole("status").textContent).toBe(
    "host-b owns this project: ask its orchestrator there",
  );
});
