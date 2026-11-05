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

// The pickable pill for a project; a peer's same-named project is not one.
const pill = (name: RegExp) => {
  const found = screen
    .getAllByRole("radio", { name })
    .filter((radio) => radio.ariaDisabled !== "true");
  if (found.length !== 1) throw new Error(`${found.length} pills match ${name}`);
  return found[0] as HTMLElement;
};
const input = () => screen.getByLabelText("Ask", { selector: "textarea" }) as HTMLTextAreaElement;
const sendButton = () => screen.getByRole("button", { name: /send/i }) as HTMLButtonElement;

test("focus starts on the picker and nothing is picked", () => {
  renderDialog();
  expect(document.activeElement).toBe(pill(/alpha/i));
  expect(screen.getAllByRole("radio").every((radio) => radio.ariaChecked === "false")).toBe(true);
});

test("Send stays off until an orchestrator is picked and there is text", () => {
  stub(null);
  renderDialog();
  expect(sendButton().disabled).toBe(true);
  fireEvent.change(input(), { target: { value: "hello" } });
  expect(sendButton().disabled).toBe(true);
  expect(screen.getByRole("status").textContent).toBe("Pick an orchestrator first.");
  fireEvent.click(pill(/beta/i));
  expect(sendButton().disabled).toBe(false);
  expect(screen.getByRole("status").textContent).toBe("Goes to the beta orchestrator.");
});

test("arrows move between the pills, Enter picks, and Tab goes from the picker to the text", () => {
  renderDialog();
  fireEvent.keyDown(pill(/alpha/i), { key: "ArrowRight" });
  expect(document.activeElement).toBe(pill(/beta/i));
  expect(pill(/beta/i).tabIndex).toBe(0);
  expect(pill(/alpha/i).tabIndex).toBe(-1);
  // Enter on a focused button is a click.
  fireEvent.click(document.activeElement as HTMLElement);
  expect(pill(/beta/i).ariaChecked).toBe("true");

  // The picker is one tab stop, and the text is the next one in the dialog.
  const stops = [
    ...screen.getByRole("dialog").querySelectorAll<HTMLElement>("button, textarea"),
  ].filter((element) => element.tabIndex >= 0 && !(element as HTMLButtonElement).disabled);
  expect(stops.slice(0, 2)).toEqual([pill(/beta/i), input()]);
});

test("a pill's number picks it", () => {
  renderDialog();
  fireEvent.keyDown(pill(/alpha/i), { key: "2" });
  expect(pill(/alpha/i).ariaChecked).toBe("false");
  fireEvent.keyDown(pill(/beta/i), { key: "2" });
  expect(pill(/beta/i).ariaChecked).toBe("true");
});

test("a project another host owns is shown but cannot be picked", () => {
  renderDialog();
  const off = screen.getAllByRole("radio").filter((radio) => radio.ariaDisabled === "true");
  expect(off.length).toBeGreaterThan(0);
  fireEvent.click(off[0] as HTMLElement);
  expect(off[0]?.ariaChecked).toBe("false");
  expect(off[0]?.tabIndex).toBe(-1);
});

test("sending posts to the picked orchestrator, shows the tile and closes", async () => {
  stub(null);
  const { queryClient, onClose } = renderDialog();
  fireEvent.click(pill(/gamma/i));
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
  fireEvent.click(pill(/alpha/i));
  fireEvent.change(input(), { target: { value: "one" } });
  fireEvent.keyDown(input(), { key: "Enter", shiftKey: true });
  expect(calls).toHaveLength(0);
  fireEvent.keyDown(input(), { key: "Enter" });
  await waitFor(() => expect(calls).toHaveLength(1));
});

test("Escape closes the dialog", () => {
  const { onClose } = renderDialog();
  fireEvent.keyDown(pill(/alpha/i), { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});

test("the dialog is locked and says Sending while the ask is in flight, and sends once", async () => {
  release = () => {};
  stub(null);
  const { onClose } = renderDialog();
  fireEvent.click(pill(/alpha/i));
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
  fireEvent.click(pill(/alpha/i));
  expect(screen.getByRole("status").textContent).toBe(
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
  fireEvent.click(pill(/alpha/i));
  fireEvent.change(input(), { target: { value: "hello" } });
  fireEvent.click(sendButton());
  await waitFor(() =>
    expect(screen.getByRole("alert").textContent).toBe("The store is read only."),
  );
  expect(input().value).toBe("hello");
  expect(onClose).not.toHaveBeenCalled();
});
