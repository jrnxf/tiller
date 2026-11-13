import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { PromptDialog } from "../../lib/api-types";
import { AnswerStrip } from "./answer-strip";

const PATH = "/api/v1/agents/s1/prompt";

function dialog(command: string): PromptDialog {
  return {
    layout: "permission",
    tool: "Bash",
    title: "Bash command",
    command,
    folder: null,
    question: "Do you want to proceed?",
    options: [
      { n: 1, text: "Yes" },
      { n: 2, text: "No" },
    ],
    screen: `hash of ${command}`,
  };
}

// The daemon's two prompt endpoints: each read answers the next screen, and
// an answer is taken when it names the screen on top.
function stubPrompt(screens: PromptDialog[]) {
  const calls: { path: string; body: unknown }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const body = JSON.parse(String(init?.body ?? "{}")) as { screen?: string };
      calls.push({ path, body });
      if (path === PATH) {
        const prompt = screens.length > 1 ? screens.shift() : screens[0];
        const read = { session_id: "s1", read_at: new Date().toISOString(), reason: null };
        return new Response(JSON.stringify({ ...read, prompt }), { status: 200 });
      }
      if (body.screen !== screens[0]?.screen) {
        const error = { code: "screen_changed", message: "the prompt changed" };
        return new Response(JSON.stringify({ ok: false, error }), { status: 409 });
      }
      return new Response(JSON.stringify({ ok: true, data: {} }), { status: 200 });
    }),
  );
  return calls;
}

function renderStrip(waiting: boolean) {
  const client = new QueryClient();
  const strip = (observedAt: string) => (
    <QueryClientProvider client={client}>
      <AnswerStrip sessionId="s1" waiting={waiting} observedAt={observedAt} />
    </QueryClientProvider>
  );
  const { rerender } = render(strip("2026-11-06T10:00:00Z"));
  // The agent's activity observed again, still waiting.
  return (observedAt: string) => rerender(strip(observedAt));
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("an agent that is not waiting has its screen left unread", () => {
  const calls = stubPrompt([dialog("ls")]);
  renderStrip(false);
  expect(screen.queryByRole("button")).toBeNull();
  expect(calls).toEqual([]);
});

test("a tap sends its digit with the dialog it was drawn from", async () => {
  const calls = stubPrompt([dialog("rm -f old.txt")]);
  renderStrip(true);
  expect(await screen.findByText("rm -f old.txt")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Answer 1: Yes" }));
  const sent = await screen.findByRole("status");
  expect(sent.textContent).toBe("Sent 1 · Yes.");
  expect(calls.map(({ path }) => path)).toEqual([PATH, `${PATH}/answer`]);
  expect(calls[1]?.body).toEqual({ option: 1, screen: "hash of rm -f old.txt" });
});

test("a tap on a changed prompt sends nothing and draws the new one", async () => {
  const calls = stubPrompt([dialog("rm -f old.txt"), dialog("rm -rf /home/user/project")]);
  renderStrip(true);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 2: No" }));
  const refused = await screen.findByRole("alert");
  expect(refused.textContent).toMatch(/^Not sent\. The prompt changed after these buttons/);
  expect(screen.getByText("rm -rf /home/user/project")).toBeTruthy();
  expect(screen.queryByRole("status")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Answer 2: No" }));
  await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Sent 2 · No."));
  expect(calls.map(({ path }) => path)).toEqual([PATH, `${PATH}/answer`, PATH, `${PATH}/answer`]);
});

test("the same wait observed again keeps its buttons, and the next wait reads again", async () => {
  const screens = [dialog("rm -f old.txt")];
  const calls = stubPrompt(screens);
  const observe = renderStrip(true);
  expect(await screen.findByText("rm -f old.txt")).toBeTruthy();
  observe("2026-11-06T10:00:01Z");
  expect(screen.getByText("rm -f old.txt")).toBeTruthy();
  expect(calls).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Answer 1: Yes" }));
  await screen.findByRole("status");
  screens[0] = dialog("rm -f new.txt");
  observe("2026-11-06T10:00:05Z");
  expect(await screen.findByText("rm -f new.txt")).toBeTruthy();
  expect(calls.map(({ path }) => path)).toEqual([PATH, `${PATH}/answer`, PATH]);
});

test("a peer's read_scope refusal is read once and shows the fallback", async () => {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      calls.push(String(input));
      const error = {
        code: "peer_refused",
        message: "bravo refused the call: read_scope",
        detail: { host: "bravo", code: "read_scope" },
      };
      return new Response(JSON.stringify({ ok: false, error }), { status: 502 });
    }),
  );
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AnswerStrip
        sessionId="s1"
        waiting
        observedAt="2026-11-06T10:00:00Z"
        host="bravo"
        fallback={<p>No buttons</p>}
      />
    </QueryClientProvider>,
  );
  expect(await screen.findByText("No buttons")).toBeTruthy();
  expect(calls).toEqual([PATH]);
});

test("a peer answer left unconfirmed in one wait is not replayed in the next wait with the same screen", async () => {
  const answers: { idempotency_key?: string }[] = [];
  const replies = [504, 504];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === PATH) {
        const read = { session_id: "s1", read_at: new Date().toISOString(), reason: null };
        return new Response(JSON.stringify({ ...read, prompt: dialog("ls") }), { status: 200 });
      }
      answers.push(JSON.parse(String(init?.body ?? "{}")) as { idempotency_key?: string });
      const status = replies.shift() ?? 200;
      if (status === 200) {
        return new Response(JSON.stringify({ ok: true, data: {} }), { status });
      }
      const error = { code: "forward_uncertain", message: "charlie did not answer" };
      return new Response(JSON.stringify({ ok: false, error }), { status });
    }),
  );
  const client = new QueryClient();
  const strip = (waiting: boolean, observedAt: string) => (
    <QueryClientProvider client={client}>
      <AnswerStrip sessionId="s1" waiting={waiting} observedAt={observedAt} host="charlie" />
    </QueryClientProvider>
  );
  const { rerender } = render(strip(true, "2026-11-06T10:00:00Z"));
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  expect(await screen.findByText(/Sent, not confirmed\./)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Answer 1: Yes" }));
  await waitFor(() => expect(answers).toHaveLength(2));
  expect(answers[1]?.idempotency_key).toBe(answers[0]?.idempotency_key);
  await screen.findByText(/Sent, not confirmed\./);
  rerender(strip(false, "2026-11-06T10:01:00Z"));
  rerender(strip(true, "2026-11-06T10:02:00Z"));
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  expect((await screen.findByRole("status")).textContent).toBe("Sent 1 · Yes.");
  expect(answers).toHaveLength(3);
  expect(typeof answers[2]?.idempotency_key).toBe("string");
  expect(answers[2]?.idempotency_key).not.toBe(answers[0]?.idempotency_key);
});

// A strip on delta's agent s1, mounted fresh with its own query cache.
function deltaStrip(observedAt: string) {
  return (
    <QueryClientProvider client={new QueryClient()}>
      <AnswerStrip sessionId="s1" waiting observedAt={observedAt} host="delta" />
    </QueryClientProvider>
  );
}

test("a peer answer left unconfirmed keeps its key when the strip mounts again in the same wait", async () => {
  const answers: { idempotency_key?: string }[] = [];
  const replies = [504];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === PATH) {
        const read = { session_id: "s1", read_at: new Date().toISOString(), reason: null };
        return new Response(JSON.stringify({ ...read, prompt: dialog("ls") }), { status: 200 });
      }
      answers.push(JSON.parse(String(init?.body ?? "{}")) as { idempotency_key?: string });
      const status = replies.shift() ?? 200;
      if (status === 200) {
        return new Response(JSON.stringify({ ok: true, data: {} }), { status });
      }
      const error = { code: "forward_uncertain", message: "delta did not answer" };
      return new Response(JSON.stringify({ ok: false, error }), { status });
    }),
  );
  const first = render(deltaStrip("2026-11-06T10:00:00Z"));
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  expect(await screen.findByText(/Sent, not confirmed\./)).toBeTruthy();
  first.unmount();
  render(deltaStrip("2026-11-06T10:00:07Z"));
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  expect((await screen.findByRole("status")).textContent).toBe("Sent 1 · Yes.");
  expect(answers).toHaveLength(2);
  expect(answers[1]?.idempotency_key).toBe(answers[0]?.idempotency_key);
});

const TRUST: PromptDialog = {
  layout: "trust",
  tool: "",
  title: "Accessing workspace:",
  command: null,
  folder: "/home/user/project",
  question: "Is this a project you created or one you trust?",
  options: [
    { n: 1, text: "No, exit" },
    { n: 2, text: "Yes, I trust this folder" },
  ],
  screen: "hash of trust",
};

// The trust dialog's endpoints, plus the run "No, exit" names.
function stubTrust() {
  const calls: { path: string; body: unknown }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      calls.push({ path, body: JSON.parse(String(init?.body ?? "{}")) });
      if (path === PATH) {
        const read = { session_id: "s1", read_at: new Date().toISOString(), reason: null };
        return new Response(JSON.stringify({ ...read, prompt: TRUST }), { status: 200 });
      }
      if (path === "/api/v1/runs/r1") {
        const run = { id: "r1", started_at: "2026-11-06T15:11:00Z" };
        return new Response(JSON.stringify(run), { status: 200 });
      }
      return new Response(JSON.stringify({ ok: true, data: {} }), { status: 200 });
    }),
  );
  return calls;
}

function renderTrust() {
  const client = new QueryClient();
  const agent = { name: "alpha-15-build", card: "alpha-15", runId: "r1" };
  render(
    <QueryClientProvider client={client}>
      <AnswerStrip sessionId="s1" waiting observedAt="2026-11-06T10:00:00Z" agent={agent} />
    </QueryClientProvider>,
  );
}

test("the trust dialog shows its folder and two buttons with no digits, Yes filled on the right", async () => {
  stubTrust();
  renderTrust();
  expect(await screen.findByText("/home/user/project")).toBeTruthy();
  expect(screen.getByText("Claude Code asks to trust")).toBeTruthy();
  expect(screen.getByText(/known layout: trust/)).toBeTruthy();
  const buttons = screen.getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual([
    "No, exit...",
    "Yes, I trust this folder",
  ]);
  expect(document.querySelector(".av-opts kbd")).toBeNull();
  expect(buttons[1]?.classList.contains("first")).toBe(true);
  expect(buttons[0]?.classList.contains("first")).toBe(false);
});

test("Yes sends the trust row, with no digit in the receipt", async () => {
  const calls = stubTrust();
  renderTrust();
  fireEvent.click(await screen.findByRole("button", { name: "Yes, I trust this folder" }));
  const sent = await screen.findByRole("status");
  expect(sent.textContent).toBe("Sent Yes, I trust this folder.");
  expect(calls[1]).toEqual({
    path: `${PATH}/answer`,
    body: { option: 2, screen: "hash of trust" },
  });
});

test("No, exit asks first: Keep waiting sends nothing, Exit the session sends the exit row", async () => {
  const calls = stubTrust();
  renderTrust();
  fireEvent.click(await screen.findByRole("button", { name: "No, exit..." }));
  const ask = screen.getByRole("dialog", { name: "Exit alpha-15-build's session?" });
  expect(ask.textContent).toContain(
    "Claude Code exits, so alpha-15-build stops and its run ends. The card stays where it is.",
  );
  expect(ask.textContent).toContain("/home/user/project");
  await waitFor(() => expect(ask.textContent).toContain("alpha-15, started "));
  fireEvent.click(screen.getByRole("button", { name: "Keep waiting" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(calls.some(({ path }) => path.endsWith("/answer"))).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "No, exit..." }));
  fireEvent.click(screen.getByRole("button", { name: "Exit the session" }));
  expect((await screen.findByRole("status")).textContent).toBe("Sent No, exit.");
  expect(calls.find(({ path }) => path.endsWith("/answer"))?.body).toEqual({
    option: 1,
    screen: "hash of trust",
  });
});
