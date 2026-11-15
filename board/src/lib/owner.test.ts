import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { FetchEventSource } from "./fetch-events";
import { OWNER_HEADER, ownerFetch, ownerToken, takeOwnerTokenFromFragment } from "./owner";

beforeEach(() => window.localStorage.clear());
afterEach(() => {
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

function stubFetch(answer: () => Response = () => new Response("{}")) {
  const fetch = vi.fn(async (_path: string, _init?: RequestInit) => answer());
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function sentToken(fetch: ReturnType<typeof stubFetch>, call = 0): string | null {
  return new Headers(fetch.mock.calls[call]?.[1]?.headers).get(OWNER_HEADER);
}

test("a login link's fragment is kept and taken out of the address bar", () => {
  window.history.replaceState({ key: "k" }, "", "/cards/T1?filter=all#owner=0a1b2c");
  expect(takeOwnerTokenFromFragment()).toBe(true);
  expect(ownerToken()).toBe("0a1b2c");
  expect(window.location.hash).toBe("");
  expect(`${window.location.pathname}${window.location.search}`).toBe("/cards/T1?filter=all");
  // The history entry is replaced, not added: Back never shows the token.
  expect(window.history.state).toEqual({ key: "k" });
});

test("another fragment is left alone, and a token that is not hex is not kept", () => {
  window.history.replaceState(null, "", "/#section");
  expect(takeOwnerTokenFromFragment()).toBe(false);
  expect(window.location.hash).toBe("#section");
  window.history.replaceState(null, "", "/#owner=<script>");
  expect(takeOwnerTokenFromFragment()).toBe(false);
  expect(ownerToken()).toBeNull();
  expect(window.location.hash).toBe("");
});

test("every request carries the kept token, and none is sent before one is kept", async () => {
  const fetch = stubFetch();
  await ownerFetch("/api/v1/board", { headers: { accept: "application/json" } });
  expect(sentToken(fetch)).toBeNull();
  window.localStorage.setItem("tiller-owner-token", "f1c7");
  await ownerFetch("/api/v1/board", { headers: { accept: "application/json" } });
  expect(sentToken(fetch, 1)).toBe("f1c7");
  expect(new Headers(fetch.mock.calls[1]?.[1]?.headers).get("accept")).toBe("application/json");
});

function streamOf(chunks: string[]): Response {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(new TextEncoder().encode(chunk));
      controller.close();
    },
  });
  return new Response(body, { headers: { "content-type": "text/event-stream" } });
}

test("the change stream is read over fetch, with the token, field by field", async () => {
  window.localStorage.setItem("tiller-owner-token", "f1c7");
  const fetch = stubFetch(() =>
    streamOf([
      ": hello\n\nid: 7\nevent: change\nda",
      'ta: {"seq":7}\r',
      "\n\ndata: a\ndata: b\n\n",
    ]),
  );
  const source = new FetchEventSource("/api/v1/events?since=6");
  const seen: string[] = [];
  source.addEventListener("open", () => seen.push(`open ${source.readyState}`));
  source.addEventListener("change", (event) =>
    seen.push(`change ${event.data} ${event.lastEventId}`),
  );
  source.addEventListener("message", (event) => seen.push(`message ${event.data}`));
  const ended = new Promise<void>((done) => source.addEventListener("error", () => done()));
  await ended;
  expect(fetch.mock.calls[0]?.[0]).toBe("/api/v1/events?since=6");
  expect(sentToken(fetch)).toBe("f1c7");
  expect(seen).toEqual(["open 1", 'change {"seq":7} 7', "message a\nb"]);
  // It never reconnects by itself: the caller opens a new one.
  expect(source.readyState).toBe(2);
  expect(fetch).toHaveBeenCalledTimes(1);
});

test("a refused stream never opens, and a closed one reports nothing", async () => {
  stubFetch(() => new Response("{}", { status: 403 }));
  const refused = new FetchEventSource("/api/v1/events");
  const seen: string[] = [];
  refused.addEventListener("open", () => seen.push("open"));
  await new Promise<void>((done) => refused.addEventListener("error", () => done()));
  expect(seen).toEqual([]);

  stubFetch(() => streamOf(["data: late\n\n"]));
  const closed = new FetchEventSource("/api/v1/events");
  closed.addEventListener("error", () => seen.push("error"));
  closed.addEventListener("message", () => seen.push("message"));
  closed.close();
  await new Promise((done) => setTimeout(done, 20));
  expect(seen).toEqual([]);
});
