import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { HostConfig } from "../../lib/api-types";
import { hostConfigFor, setConfigFixture } from "../../lib/fixture-settings";
import { fixtureResource } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { signedOutStore } from "../../lib/session";
import { SettingsPage } from "./settings-page";
import { createTestQueryClient } from "../../test-app";

// This host's other keys, served beside the rest. A POST is answered by the
// stand-in server's rules, which mirror the daemon's.
function serve(owner: boolean, config: Partial<HostConfig> = {}) {
  const posts: Record<string, unknown>[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === "/api/v1/session") return new Response(JSON.stringify({ owner }));
    if (path === "/api/v1/host-config") {
      return new Response(JSON.stringify({ ...hostConfigFor(), ...config }));
    }
    if (path === "/api/v1/config" && init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      posts.push(body);
      const saved = setConfigFixture(body, "default");
      if (!saved.ok) {
        const error = { code: saved.code, message: saved.message, current: saved.current };
        return new Response(JSON.stringify({ ok: false, error }), { status: saved.status });
      }
      return new Response(
        JSON.stringify({
          ok: true,
          replayed: false,
          events: [],
          receipt: { recorded_at: "2026-11-05T12:00:00Z", outbox_id: null, approval_id: null },
          data: saved.data,
        }),
      );
    }
    if (path === "/api/v1/health" || path === "/api/v1/machines") {
      return new Response(JSON.stringify(fixtureResource(path, "federated")));
    }
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetch);
  return posts;
}

function renderSettings() {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <SettingsPage />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  signedOutStore.set(false);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const section = async (name: string) => within(await screen.findByRole("region", { name }));

test("the host keys sit in three sections under Starting agents, one row per key with Save on the right", async () => {
  serve(true);
  renderSettings();
  const agents = await section("Agents and hooks");
  const peers = await section("Peers and memory");
  const learning = await section("Learning");
  for (const [rows, labels] of [
    [agents, ["herdr terminal", "Stop hook mode"]],
    [peers, ["Accept peers", "Memory remote"]],
    [
      learning,
      ["Retro sample", "Measure window", "Curator model", "Curator limit", "Evals folder"],
    ],
  ] as const) {
    for (const label of labels) {
      const form = rows.getByRole("form", { name: label });
      // The control first, Save last: the primary action is rightmost.
      const control = form.querySelector(".setting-control") as HTMLElement;
      const save = within(control).getByRole("button", { name: "Save" });
      expect(control.lastElementChild).toBe(save);
      expect(save.className).toContain("primary");
    }
  }
  // The secret is not a row.
  expect(screen.queryByText(/decider/i)).toBeNull();
  expect(screen.queryByLabelText(/decider/i)).toBeNull();
  const titles = [...document.querySelectorAll(".settings-page > .set-sec > .set-sec-h h2")].map(
    (title) => title.textContent,
  );
  // After the hosts, before this browser's own settings.
  expect(titles.slice(titles.indexOf("Agents and hooks"), titles.indexOf("This browser"))).toEqual([
    "Agents and hooks",
    "Peers and memory",
    "Learning",
  ]);
});

test("a row shows what config.toml holds and Save arms only on a change", async () => {
  serve(true);
  renderSettings();
  const learning = await section("Learning");
  const sample = learning.getByRole("form", { name: "Retro sample" });
  const input = within(sample).getByRole<HTMLInputElement>("textbox");
  expect(input.value).toBe("5");
  expect(within(sample).getByText("cards")).toBeTruthy();
  const save = within(sample).getByRole<HTMLButtonElement>("button", { name: "Save" });
  expect(save.disabled).toBe(true);
  fireEvent.change(input, { target: { value: "7" } });
  expect(save.disabled).toBe(false);
  fireEvent.change(input, { target: { value: "5" } });
  expect(save.disabled).toBe(true);
});

test("a count is saved as its text and a bad one is refused in plain words", async () => {
  const posts = serve(true);
  renderSettings();
  const learning = await section("Learning");
  const form = learning.getByRole("form", { name: "Measure window" });
  const input = within(form).getByRole("textbox");

  fireEvent.change(input, { target: { value: "0" } });
  fireEvent.click(within(form).getByRole("button", { name: "Save" }));
  expect((await within(form).findByRole("alert")).textContent).toContain("a whole number from 1");

  fireEvent.change(input, { target: { value: "12" } });
  fireEvent.click(within(form).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(posts.map((post) => post.value)).toEqual(["0", "12"]));
  expect(posts[1]).toMatchObject({ key: "learning.window_cards", value: "12" });
});

test("Off and Observe save", async () => {
  const posts = serve(true);
  renderSettings();
  const agents = await section("Agents and hooks");
  const form = agents.getByRole("form", { name: "Stop hook mode" });
  const modes = within(form).getByRole("group", { name: "Stop hook mode" });
  expect(within(modes).getByRole("button", { name: "Observe" }).getAttribute("aria-pressed")).toBe(
    "true",
  );
  fireEvent.click(within(modes).getByRole("button", { name: "Off" }));
  fireEvent.click(within(form).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0]).toMatchObject({ key: "stop_hook_mode", value: "off" });
  expect(within(form).queryByRole("alert")).toBeNull();
  // Put it back for the other tests.
  setConfigFixture({ key: "stop_hook_mode", value: "observe" }, "default");
});

test("Block once is refused by the switching rule, with the numbers behind it", async () => {
  const posts = serve(true);
  renderSettings();
  const agents = await section("Agents and hooks");
  const form = agents.getByRole("form", { name: "Stop hook mode" });
  fireEvent.click(within(form).getByRole("button", { name: "Block once" }));
  fireEvent.click(within(form).getByRole("button", { name: "Save" }));

  const alert = await within(form).findByRole("alert");
  expect(alert.textContent).toContain(
    "Not saved: block once needs 200 orchestrator stops in the last 7 days with a false-block rate under 2%.",
  );
  const facts = within(alert).getByTestId("switching-rule");
  const rows = [...facts.querySelectorAll("dt")].map((term) => [
    term.textContent,
    term.nextElementSibling?.textContent,
  ]);
  expect(rows.slice(0, 3)).toEqual([
    ["Stops", "146need 200"],
    ["Would block", "29"],
    ["False blocks", "13.4% of thoseneed under 2%"],
  ]);
  expect(rows[3]?.[0]).toBe("Window");
  expect(rows[3]?.[1]).toMatch(/^\w{3} \d{1,2} \w{3} to \w{3} \d{1,2} \w{3}$/);
  // The refused value is not what the host holds.
  expect(posts).toHaveLength(1);
  const hold = await screen.findByTestId("host-config-agents");
  expect(within(hold).getByRole("button", { name: "Observe" }).getAttribute("aria-pressed")).toBe(
    "false",
  );
  expect(
    within(hold).getByRole("button", { name: "Block once" }).getAttribute("aria-pressed"),
  ).toBe("true");
});

test("Accept peers is On or Off and Memory remote may be cleared", async () => {
  const posts = serve(true);
  renderSettings();
  const peers = await section("Peers and memory");
  const accept = peers.getByRole("form", { name: "Accept peers" });
  fireEvent.click(within(accept).getByRole("button", { name: "Off" }));
  fireEvent.click(within(accept).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0]).toMatchObject({ key: "federation.accept", value: "false" });

  const remote = peers.getByRole("form", { name: "Memory remote" });
  fireEvent.change(within(remote).getByRole("textbox"), { target: { value: "" } });
  fireEvent.click(within(remote).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(posts).toHaveLength(2));
  expect(posts[1]).toMatchObject({ key: "memory.remote", value: "" });
  setConfigFixture({ key: "federation.accept", value: "true" }, "default");
  setConfigFixture({ key: "memory.remote", value: "git@example.com:team/memory.git" }, "default");
});

test("a browser that is not the owner reads the values and cannot change them", async () => {
  serve(false);
  renderSettings();
  const learning = await section("Learning");
  expect(learning.getByText("5 cards")).toBeTruthy();
  expect(learning.getByText("sonnet")).toBeTruthy();
  expect(learning.queryAllByRole("textbox")).toHaveLength(0);
  expect(learning.queryAllByRole("button", { name: "Save" })).toHaveLength(0);
  const agents = await section("Agents and hooks");
  expect(agents.getByText("Observe")).toBeTruthy();
  expect(agents.queryAllByRole("button")).toHaveLength(0);
});

test("another host's rows say where to set them, with the command and no input", async () => {
  serve(true);
  renderSettings();
  const agents = await section("Agents and hooks");
  fireEvent.click(await agents.findByRole("combobox", { name: "Host" }));
  const option = await screen.findByRole("option", { name: "host-b" });
  fireEvent.pointerDown(option);
  fireEvent.click(option);

  await waitFor(() =>
    expect(screen.getByRole("region", { name: "Agents and hooks" }).textContent).toContain(
      "Read by every new agent on host-b.",
    ),
  );
  for (const name of ["Agents and hooks", "Peers and memory", "Learning"]) {
    const part = within(screen.getByRole("region", { name }));
    expect(part.queryAllByRole("textbox")).toHaveLength(0);
    expect(part.queryAllByRole("button", { name: "Save" })).toHaveLength(0);
  }
  const stop = screen.getByTestId("setting-stop_hook_mode");
  expect(stop.textContent).toContain("Set on host-b.");
  expect(stop.textContent).toContain("tiller config set stop_hook_mode <off|observe|block-once>");
  // Nothing was sent to the other host.
  const sent = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls.filter(
    ([, init]) => (init as RequestInit | undefined)?.method === "POST",
  );
  expect(sent).toHaveLength(0);
});

test("a config.toml that cannot be read is said instead of the rows", async () => {
  serve(true, { config_error: "config.toml does not parse: expected `=`" });
  renderSettings();
  const part = await section("Host config");
  expect(part.getByText("config.toml does not parse: expected =")).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Learning" })).toBeNull();
});
