import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AskRecorded, BoardResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureSnapshot } from "../../lib/fixtures";
import { boardPath, queryKeyFor } from "../../lib/queries";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { AskDialog } from "./ask-dialog";

interface Call {
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
let release: (() => void) | null = null;

function reply(project: string, warning: string | null): Response {
  const data: AskRecorded = {
    ask: {
      id: "01JASK0000000000000000001",
      project_id: project,
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
      const path = String(input);
      calls.push({ path, body: JSON.parse(String(init?.body ?? "{}")) });
      const project = /projects\/([^/]+)\/asks/.exec(path)?.[1] ?? "alpha";
      if (release !== null) {
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      }
      return reply(project, warning);
    }),
  );
}

function renderDialog(onClose = vi.fn()) {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("federated"));
  const board: BoardResponse = { rows: [], asked: [] };
  queryClient.setQueryData(queryKeyFor(boardPath()), board);
  render(
    <QueryClientProvider client={queryClient}>
      <AskDialog open onClose={onClose} />
    </QueryClientProvider>,
  );
  return { queryClient, onClose };
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

// The pickable row for a project; a peer's same-named project is not one.
const row = (name: RegExp) => {
  const found = screen
    .getAllByRole("option", { name })
    .filter((option) => option.ariaDisabled !== "true");
  if (found.length !== 1) throw new Error(`${found.length} rows match ${name}`);
  return found[0] as HTMLElement;
};
const search = () => document.querySelector(".ask-combo-in") as HTMLInputElement;
const note = () => document.querySelector(".ask-note")?.textContent;
const trigger = () => document.querySelector(".ask-combo-btn") as HTMLElement;
const pick = (name: RegExp) => fireEvent.click(row(name));
const input = () => screen.getByLabelText("Ask", { selector: "textarea" }) as HTMLTextAreaElement;
const sendButton = () => screen.getByRole("button", { name: /send/i }) as HTMLButtonElement;

test("the list opens with the search box focused and nothing is picked", async () => {
  renderDialog();
  await waitFor(() => expect(document.activeElement).toBe(search()));
  expect(trigger().textContent).toContain("Pick an orchestrator");
  expect(screen.getAllByRole("option").every((option) => option.ariaSelected !== "true")).toBe(
    true,
  );
});

test("Send stays off until an orchestrator is picked and there is text", () => {
  stub(null);
  renderDialog();
  expect(sendButton().disabled).toBe(true);
  fireEvent.change(input(), { target: { value: "hello" } });
  expect(sendButton().disabled).toBe(true);
  expect(note()).toBe("Pick an orchestrator first.");
  pick(/beta/i);
  expect(sendButton().disabled).toBe(false);
  expect(note()).toBe("Goes to the beta orchestrator.");
  expect(trigger().textContent).toContain("beta");
});

test("typing filters the list", () => {
  renderDialog();
  fireEvent.change(search(), { target: { value: "gam" } });
  expect(row(/gamma/i)).toBeTruthy();
  expect(screen.queryAllByRole("option", { name: /alpha/i })).toHaveLength(0);
});

test("a number key picks that row and the text box takes focus", async () => {
  renderDialog();
  fireEvent.keyDown(search(), { key: "2" });
  expect(trigger().textContent).toContain("beta");
  await waitFor(() => expect(document.activeElement).toBe(input()));
});

test("each row shows its number", () => {
  renderDialog();
  expect(row(/alpha/i).textContent).toMatch(/1$/);
  expect(row(/gamma/i).textContent).toMatch(/3$/);
});

test("a click picks and Tab order runs from the picker to the text", async () => {
  renderDialog();
  pick(/alpha/i);
  await waitFor(() => expect(document.activeElement).toBe(input()));
  const stops = [
    ...screen.getByRole("dialog").querySelectorAll<HTMLElement>("button, textarea"),
  ].filter((element) => element.tabIndex >= 0 && !(element as HTMLButtonElement).disabled);
  expect(stops.slice(0, 2)).toEqual([trigger(), input()]);
});

test("a project another host owns is shown but cannot be picked", () => {
  renderDialog();
  const off = screen.getAllByRole("option").filter((option) => option.ariaDisabled === "true");
  expect(off.length).toBeGreaterThan(0);
  fireEvent.click(off[0] as HTMLElement);
  expect(trigger().textContent).toContain("Pick an orchestrator");
});

test("sending posts to the picked orchestrator, shows the tile and closes", async () => {
  stub(null);
  const { queryClient, onClose } = renderDialog();
  pick(/gamma/i);
  fireEvent.change(input(), { target: { value: "  ship the footer\nthen the header  " } });
  fireEvent.click(sendButton());

  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe("/api/v1/projects/gamma/asks");
  expect(calls[0]?.body.text).toBe("ship the footer\nthen the header");
  expect(typeof calls[0]?.body.idempotency_key).toBe("string");
  expect(calls[0]?.body).not.toHaveProperty("host");
  const all = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  expect(all?.asked.map((ask) => ask.id)).toEqual(["01JASK0000000000000000001"]);
});

test("Enter sends, Shift+Enter adds a line", async () => {
  stub(null);
  renderDialog();
  pick(/alpha/i);
  fireEvent.change(input(), { target: { value: "one" } });
  fireEvent.keyDown(input(), { key: "Enter", shiftKey: true });
  expect(calls).toHaveLength(0);
  fireEvent.keyDown(input(), { key: "Enter" });
  await waitFor(() => expect(calls).toHaveLength(1));
});

test("Escape closes the list first and the dialog next", () => {
  const { onClose } = renderDialog();
  fireEvent.keyDown(search(), { key: "Escape" });
  expect(onClose).not.toHaveBeenCalled();
  fireEvent.keyDown(trigger(), { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});

test("the dialog is locked and says Sending while the ask is in flight, and sends once", async () => {
  release = () => {};
  stub(null);
  const { onClose } = renderDialog();
  pick(/alpha/i);
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() => expect(sendButton().textContent).toContain("Sending..."));
  expect(sendButton().disabled).toBe(true);
  expect(input().readOnly).toBe(true);
  fireEvent.keyDown(input(), { key: "Enter" });
  expect(calls).toHaveLength(1);
  await act(async () => release?.());
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(calls).toHaveLength(1);
});

test("with no orchestrator running it says so before and after sending", async () => {
  stub("no_orchestrator");
  const queryClient = createQueryClient();
  const snapshot = fixtureSnapshot("federated");
  hydrateFromSnapshot(queryClient, snapshot);
  queryClient.setQueryData(queryKeyFor("/api/v1/agents"), { orchestrators: [] });
  render(
    <QueryClientProvider client={queryClient}>
      <AskDialog open onClose={() => {}} />
    </QueryClientProvider>,
  );
  pick(/alpha/i);
  expect(note()).toBe(
    "No orchestrator is running for alpha: the ask is recorded and picked up when it starts.",
  );
});

test("a failed send keeps the dialog open with the text and says why", async () => {
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
  const { onClose } = renderDialog();
  pick(/alpha/i);
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() =>
    expect(screen.getByRole("alert").textContent).toBe("The store is read only."),
  );
  expect(input().value).toBe("hello");
  expect(onClose).not.toHaveBeenCalled();
});
