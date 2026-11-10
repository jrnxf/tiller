import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { BoardResponse, Project, ProjectsResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureResource, fixtureSnapshot, serverResponses } from "../../lib/fixtures";
import { LIMITS, charCount, isTooLong } from "../../lib/limits";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { boardPath } from "../../lib/queries";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { NewCardDialog } from "../board/new-card";
import { answeredStore, withdrawnStore } from "./answered";
import { EditFields } from "./card-lifecycle";

// The daemon refuses text over these lengths with 400 `usage` (protocol.md
// 7.4). Each board input that sends one of these fields says so before Send,
// and sends nothing.

const RETRY = "01JATTN0000000000000000000";
const flush = () => act(async () => {});

let posted: { method: string; path: string }[] = [];

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  answeredStore.set({});
  withdrawnStore.set({});
  posted = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method !== "GET") {
      posted.push({ method, path });
      return new Response(JSON.stringify({ ok: true, replayed: false, events: [1], data: {} }));
    }
    const data =
      (serverResponses("default") as Record<string, unknown>)[path] ??
      fixtureResource(path, "default");
    return new Response(JSON.stringify(data), { status: 200 });
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const long = (limit: number) => "a".repeat(limit + 1);

test("the limits count characters the way the daemon does", () => {
  expect(LIMITS).toEqual({ cardTitle: 1000, cardBody: 100_000, comment: 20_000, answer: 20_000 });
  // One emoji is two UTF-16 units and one character.
  expect(charCount("\u{1F600}")).toBe(1);
  expect(isTooLong("\u{1F600}".repeat(1000), 1000)).toBe(false);
  expect(isTooLong("\u{1F600}".repeat(1001), 1000)).toBe(true);
  expect(isTooLong("a".repeat(1000), 1000)).toBe(false);
});

async function renderInbox(item: string) {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", Date.parse("2026-11-02T12:00:00Z")));
  const router = createAppRouter(
    queryClient,
    createMemoryHistory({ initialEntries: [`/inbox?item=${item}`] }),
  );
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

test("a comment over 20,000 characters says so and cannot be sent", async () => {
  await renderInbox(RETRY);
  const actions = screen.getByTestId("card-actions");
  const input = within(actions).getByLabelText("Comment to the orchestrator");
  const send = within(actions).getByRole("button", { name: /Send/ });

  fireEvent.change(input, { target: { value: "a".repeat(LIMITS.comment) } });
  expect((send as HTMLButtonElement).disabled).toBe(false);
  expect(within(actions).queryByText(/The limit is/)).toBeNull();

  fireEvent.change(input, { target: { value: long(LIMITS.comment) } });
  expect(
    within(actions).getByText("The comment is 20,001 characters. The limit is 20,000."),
  ).toBeTruthy();
  expect((send as HTMLButtonElement).disabled).toBe(true);
  fireEvent.keyDown(input, { key: "Enter" });
  await flush();
  expect(posted).toEqual([]);
});

const options = () => within(screen.getByRole("group", { name: "Answer" })).getAllByRole("button");

test("an answer note over 20,000 characters turns every option off", async () => {
  await renderInbox(RETRY);
  const note = screen.getByRole("textbox", { name: "Note" });

  fireEvent.change(note, { target: { value: "a".repeat(LIMITS.answer - 50) } });
  expect(options().every((button) => !(button as HTMLButtonElement).disabled)).toBe(true);

  fireEvent.change(note, { target: { value: long(LIMITS.answer) } });
  expect(
    screen.getByText(/The answer is 20,\d{3} characters\. The limit is 20,000\./),
  ).toBeTruthy();
  expect(options().every((button) => (button as HTMLButtonElement).disabled)).toBe(true);
  fireEvent.keyDown(window, { key: "1" });
  await flush();
  expect(posted).toEqual([]);
});

test("a follow-up card title over 1,000 characters cannot be added", async () => {
  await renderInbox(RETRY);
  const actions = screen.getByTestId("card-actions");
  fireEvent.click(within(actions).getByRole("button", { name: "Add card" }));
  fireEvent.change(within(actions).getByPlaceholderText("Title"), {
    target: { value: long(LIMITS.cardTitle) },
  });
  expect(
    within(actions).getByText("The title is 1,001 characters. The limit is 1,000."),
  ).toBeTruthy();
  const add = within(actions).getAllByRole("button", { name: "Add card" }).at(-1);
  expect((add as HTMLButtonElement).disabled).toBe(true);
});

function aProject(): Project {
  const projects = fixtureResource("/api/v1/projects", "default") as ProjectsResponse;
  const first = projects.projects[0];
  if (!first) throw new Error("no fixture project");
  return first;
}

test("New card refuses a title over 1,000 or a body over 100,000 characters", async () => {
  render(<NewCardDialog project={aProject()} onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog");
  const file = within(dialog).getByRole("button", { name: "File card" }) as HTMLButtonElement;
  const title = within(dialog).getByLabelText("Title");
  const body = within(dialog).getByLabelText("Body");

  fireEvent.change(title, { target: { value: "a".repeat(LIMITS.cardTitle) } });
  expect(file.disabled).toBe(false);

  fireEvent.change(title, { target: { value: long(LIMITS.cardTitle) } });
  expect(
    within(dialog).getByText("The title is 1,001 characters. The limit is 1,000."),
  ).toBeTruthy();
  expect(file.disabled).toBe(true);

  fireEvent.change(title, { target: { value: "Fix it" } });
  expect(file.disabled).toBe(false);
  fireEvent.change(body, { target: { value: long(LIMITS.cardBody) } });
  expect(
    within(dialog).getByText("The body is 100,001 characters. The limit is 100,000."),
  ).toBeTruthy();
  expect(file.disabled).toBe(true);
  fireEvent.submit(title.closest("form") as HTMLFormElement);
  await flush();
  expect(posted).toEqual([]);
});

test("Edit refuses a title over 1,000 or a body over 100,000 characters", async () => {
  const board = fixtureResource(boardPath(), "default") as BoardResponse;
  const card = board.rows[0];
  if (!card) throw new Error("no fixture card");
  render(
    <EditFields card={card} rev={card.rev} title="Old title" body="Old body" onDone={() => {}} />,
  );
  const save = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

  fireEvent.change(screen.getByLabelText("Title"), { target: { value: long(LIMITS.cardTitle) } });
  expect(screen.getByText("The title is 1,001 characters. The limit is 1,000.")).toBeTruthy();
  expect(save.disabled).toBe(true);

  fireEvent.change(screen.getByLabelText("Title"), { target: { value: "New title" } });
  expect(save.disabled).toBe(false);
  fireEvent.change(screen.getByLabelText("Body"), { target: { value: long(LIMITS.cardBody) } });
  expect(screen.getByText("The body is 100,001 characters. The limit is 100,000.")).toBeTruthy();
  expect(save.disabled).toBe(true);
});
