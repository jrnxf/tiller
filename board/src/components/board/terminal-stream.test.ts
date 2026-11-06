import { afterEach, expect, test, vi } from "vitest";

import type { MirrorStatus, Screen } from "./terminal-stream";
import {
  TerminalInput,
  TerminalStream,
  TerminalTail,
  pasteChunks,
  readHistory,
  terminalPath,
} from "./terminal-stream";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const encoder = new TextEncoder();

function frame(seq: number, full: boolean, text: string) {
  return { type: "frame", seq, full, bytes: btoa(text) };
}

// One open of the stream: the lines it answers with, then held open unless
// `end` says the daemon closed it.
function ndjson(lines: unknown[], end = false): Response {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const line of lines) controller.enqueue(encoder.encode(`${JSON.stringify(line)}\n`));
      if (end) controller.close();
    },
  });
  return new Response(body, { status: 200, headers: { "content-type": "application/x-ndjson" } });
}

function refusal(status: number, code: string, message: string): Response {
  return new Response(JSON.stringify({ ok: false, error: { code, message } }), { status });
}

// Answers each open with the next response, and keeps the signals so a test
// can see the request was dropped.
function opens(...responses: Response[]) {
  const signals: AbortSignal[] = [];
  const request = vi.fn(async (_path: string, init: RequestInit) => {
    if (init.signal) signals.push(init.signal);
    const next = responses.shift();
    if (!next) return new Promise<Response>(() => {});
    return next;
  });
  vi.stubGlobal("fetch", request);
  return { request, signals };
}

function recorder() {
  const events: string[] = [];
  const statuses: MirrorStatus["kind"][] = [];
  const screen: Screen = {
    reset: (cols, rows) => events.push(`reset ${cols}x${rows}`),
    write: (bytes) => events.push(new TextDecoder().decode(bytes)),
  };
  return {
    events,
    statuses,
    screen,
    onStatus: (status: MirrorStatus) => statuses.push(status.kind),
  };
}

async function settle() {
  for (let index = 0; index < 20; index += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("a full screen and the diffs after it are drawn at the pane's size", async () => {
  const { request } = opens(
    ndjson([{ type: "size", cols: 120, rows: 36 }, frame(1, true, "full"), frame(2, false, "d2")]),
  );
  const { events, statuses, screen, onStatus } = recorder();
  const sizes: string[] = [];
  const stream = new TerminalStream("s1", screen, onStatus, (size) =>
    sizes.push(`${size.cols}x${size.rows}`),
  );
  stream.start();
  await settle();
  expect(request.mock.calls[0]?.[0]).toBe(terminalPath("s1"));
  expect(events).toEqual(["reset 120x36", "full", "d2"]);
  expect(sizes).toEqual(["120x36"]);
  expect(statuses).toEqual(["connecting", "live"]);
  stream.stop();
});

test.each([
  ["a gap in seq", [frame(1, true, "full"), frame(3, false, "d3")]],
  ["a diff with no full screen under it", [frame(4, false, "d4")]],
])("%s drops the stream and opens it again for a fresh full screen", async (_, first) => {
  const { request, signals } = opens(
    ndjson([{ type: "size", cols: 80, rows: 24 }, ...first]),
    ndjson([{ type: "size", cols: 80, rows: 24 }, frame(1, true, "again")]),
  );
  const { events, statuses, screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 300));
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(signals[0]?.aborted).toBe(true);
  expect(events).not.toContain("d3");
  expect(events).not.toContain("d4");
  expect(events.at(-1)).toBe("again");
  expect(statuses).toContain("reconnecting");
  expect(statuses.at(-1)).toBe("live");
  stream.stop();
});

test("a stream that ends is opened again", async () => {
  const { request } = opens(
    ndjson([{ type: "size", cols: 80, rows: 24 }, frame(1, true, "one")], true),
    ndjson([{ type: "size", cols: 80, rows: 24 }, frame(1, true, "two")]),
  );
  const { events, screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 300));
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(events.at(-1)).toBe("two");
  stream.stop();
});

test("a closed pane keeps its last screen and is not opened again", async () => {
  const { request } = opens(
    ndjson([
      { type: "size", cols: 80, rows: 24 },
      frame(1, true, "last"),
      { type: "closed", reason: "pane closed" },
    ]),
  );
  const { events, statuses, screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 600));
  expect(request).toHaveBeenCalledTimes(1);
  expect(events).toEqual(["reset 80x24", "last"]);
  expect(statuses.at(-1)).toBe("ended");
});

test.each([
  [403, "forbidden", "forbidden"],
  [422, "refused", "remote"],
  [404, "not_found", "ended"],
])("a %i answer to the open is final, not retried", async (status, code, kind) => {
  const { request } = opens(refusal(status, code, "no"));
  const { statuses, screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 600));
  expect(request).toHaveBeenCalledTimes(1);
  expect(statuses.at(-1)).toBe(kind);
});

test("herdr out of reach is shown and tried again", async () => {
  const { request } = opens(
    refusal(503, "unavailable", "herdr is unreachable: no socket"),
    ndjson([{ type: "size", cols: 80, rows: 24 }, frame(1, true, "back")]),
  );
  const { statuses, screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  expect(statuses.at(-1)).toBe("unreachable");
  await new Promise((resolve) => setTimeout(resolve, 300));
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(statuses.at(-1)).toBe("live");
  stream.stop();
});

test("leaving the mirror drops the request, which ends the observe process", async () => {
  const { signals } = opens(ndjson([{ type: "size", cols: 80, rows: 24 }]));
  const { screen, onStatus } = recorder();
  const stream = new TerminalStream("s1", screen, onStatus, () => {});
  stream.start();
  await settle();
  expect(signals[0]?.aborted).toBe(false);
  stream.stop();
  expect(signals[0]?.aborted).toBe(true);
});

function inputs(...answers: Response[]) {
  const bodies: unknown[] = [];
  let release: (() => void) | null = null;
  const request = vi.fn(async (_path: string, init: RequestInit) => {
    bodies.push(JSON.parse(String(init.body)));
    if (bodies.length === 1) await new Promise<void>((resolve) => (release = resolve));
    return answers.shift() ?? new Response(JSON.stringify({ ok: true }), { status: 200 });
  });
  vi.stubGlobal("fetch", request);
  return { bodies, release: () => release?.() };
}

test("a live Fit takes a new size in place, and opens again when it cannot", async () => {
  const opened: string[] = [];
  const signals: AbortSignal[] = [];
  const refits: string[] = [];
  const streams = [
    ndjson([{ type: "size", cols: 100, rows: 30 }, frame(1, true, "fitted")]),
    ndjson([{ type: "size", cols: 130, rows: 44 }, frame(1, true, "again")]),
    ndjson([{ type: "size", cols: 80, rows: 24 }, frame(1, true, "own")]),
  ];
  const answers = [new Response("{}", { status: 200 }), refusal(422, "refused", "no Fit")];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (path: string, init: RequestInit) => {
      if (path.endsWith("/fit")) {
        refits.push(String(init.body));
        return answers.shift();
      }
      opened.push(String(init.body));
      if (init.signal) signals.push(init.signal);
      return streams.shift() ?? new Promise<Response>(() => {});
    }),
  );
  const { events, screen, onStatus } = recorder();
  let fit: { cols: number; rows: number } | null = { cols: 100, rows: 30 };
  const stream = new TerminalStream("s1", screen, onStatus, () => {}, {
    size: () => fit,
    onUnfit: () => {},
  });
  stream.start();
  await settle();
  expect(opened).toEqual(['{"fit":{"cols":100,"rows":30}}']);
  // The same Fit again changes nothing.
  stream.refit();
  await settle();
  expect(refits).toEqual([]);
  // A new size goes to the open Fit, and the stream stays open.
  fit = { cols: 120, rows: 40 };
  stream.refit();
  await settle();
  expect(refits).toEqual(['{"cols":120,"rows":40}']);
  expect(opened).toHaveLength(1);
  expect(signals[0]?.aborted).toBe(false);
  // The daemon no longer holds the Fit: the stream opens again with it.
  fit = { cols: 130, rows: 44 };
  stream.refit();
  await settle();
  expect(refits).toHaveLength(2);
  expect(signals[0]?.aborted).toBe(true);
  expect(opened.at(-1)).toBe('{"fit":{"cols":130,"rows":44}}');
  // The Fit turned off: open again without it, so herdr gets its size back.
  fit = null;
  stream.refit();
  await settle();
  expect(signals[1]?.aborted).toBe(true);
  expect(opened.at(-1)).toBe("{}");
  expect(events).toEqual(["reset 100x30", "fitted", "reset 130x44", "again", "reset 80x24", "own"]);
  stream.stop();
});

test("a new Fit waits for a pending retry, and an ended stream stays ended", async () => {
  vi.useFakeTimers();
  const { request } = opens(
    ndjson([{ type: "unavailable", message: "herdr went away" }], true),
    ndjson([{ type: "closed", reason: "pane_closed" }], true),
  );
  const { statuses, screen, onStatus } = recorder();
  let fit = { cols: 100, rows: 30 };
  const stream = new TerminalStream("s1", screen, onStatus, () => {}, {
    size: () => fit,
    onUnfit: () => {},
  });
  stream.start();
  await vi.advanceTimersByTimeAsync(0);
  expect(statuses.at(-1)).toBe("reconnecting");
  fit = { cols: 120, rows: 40 };
  stream.refit();
  await vi.advanceTimersByTimeAsync(0);
  expect(request).toHaveBeenCalledTimes(1);
  // The retry opens on its own time, with the new Fit.
  await vi.advanceTimersByTimeAsync(250);
  expect(request).toHaveBeenCalledTimes(2);
  expect(request.mock.calls[1]?.[1].body).toBe('{"fit":{"cols":120,"rows":40}}');
  expect(statuses.at(-1)).toBe("ended");
  fit = { cols: 90, rows: 30 };
  stream.refit();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(request).toHaveBeenCalledTimes(2);
  stream.stop();
});

test("a Fit herdr refuses is reported and the stream goes on at the pane's size", async () => {
  const { request } = opens(
    ndjson([
      { type: "size", cols: 100, rows: 30 },
      { type: "unfit", message: "terminal w1:p1 already has an attached client" },
      { type: "size", cols: 80, rows: 24 },
      frame(1, true, "own size"),
    ]),
  );
  const { events, statuses, screen, onStatus } = recorder();
  const unfit: string[] = [];
  let fit: { cols: number; rows: number } | null = { cols: 100, rows: 30 };
  const stream = new TerminalStream("s1", screen, onStatus, () => {}, {
    size: () => fit,
    onUnfit: (message) => {
      unfit.push(message);
      fit = null;
    },
  });
  stream.start();
  await settle();
  expect(unfit).toEqual(["terminal w1:p1 already has an attached client"]);
  expect(events).toEqual(["reset 80x24", "own size"]);
  expect(statuses.at(-1)).toBe("live");
  // Turning the Fit off after a refusal keeps the stream it has.
  stream.refit();
  await settle();
  expect(request).toHaveBeenCalledTimes(1);
  stream.stop();
});

test("keys typed while a request is out go together in the next one", async () => {
  const { bodies, release } = inputs();
  const input = new TerminalInput("s1", () => {});
  input.keys(["l"]);
  input.keys(["s"]);
  input.keys(["enter"]);
  await settle();
  release();
  await settle();
  expect(bodies).toEqual([{ keys: ["l"] }, { keys: ["s", "enter"] }]);
});

test("keys sent too fast are sent again after a pause, in order", async () => {
  const { bodies, release } = inputs(
    refusal(503, "unavailable", "input came faster than 20 a second; nothing was sent"),
  );
  const refused = vi.fn();
  const input = new TerminalInput("s1", refused);
  input.keys(["a"]);
  input.keys(["b"]);
  await settle();
  release();
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 350));
  await settle();
  expect(bodies).toEqual([{ keys: ["a"] }, { keys: ["a"] }, { keys: ["b"] }]);
  expect(refused).not.toHaveBeenCalled();
});

test("a refused key is dropped and reported, and the keys after it still go", async () => {
  const { bodies, release } = inputs(refusal(400, "usage", "herdr refused a key"));
  const refused = vi.fn();
  const input = new TerminalInput("s1", refused);
  input.keys(["bad"]);
  input.text("ok");
  await settle();
  release();
  await settle();
  expect(bodies).toEqual([{ keys: ["bad"] }, { text: "ok" }]);
  expect(refused).toHaveBeenCalledTimes(1);
});

test("a paste is split into pieces the daemon takes, never inside a character", () => {
  const text = `${"a".repeat(4095)}€${"b".repeat(10)}`;
  const chunks = pasteChunks(text);
  expect(chunks.join("")).toBe(text);
  expect(chunks.map((chunk) => encoder.encode(chunk).length)).toEqual([4095, 13]);
});

test("a history read takes the daemon's plain body, and a refusal its reason", async () => {
  const answers = [
    new Response(JSON.stringify({ session_id: "s1", text: "one\ntwo\n" }), { status: 200 }),
    refusal(403, "forbidden", "owner only"),
    refusal(404, "not_found", "pane w2:p1 is gone"),
  ];
  const paths: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (path: string) => {
      paths.push(path);
      return answers.shift();
    }),
  );
  expect(await readHistory("s1")).toEqual({ ok: true, text: "one\ntwo\n" });
  expect(await readHistory("s1")).toEqual({ ok: false, detail: expect.stringContaining("owner") });
  expect(await readHistory("s1")).toEqual({ ok: false, detail: "pane w2:p1 is gone" });
  expect(paths).toEqual(Array(3).fill("/api/v1/agents/s1/terminal/history"));
});

function tail(cols: number, rows: number, text: string): Response {
  return new Response(JSON.stringify({ session_id: "s1", cols, rows, text }), { status: 200 });
}

const TAIL_HEAD = "\x1b[0m\x1b[H\x1b[2J\x1b[?25l";

test("a tail draws herdr's rows from the top, and resets only when the size changes", async () => {
  vi.useFakeTimers();
  const { request } = opens(
    tail(80, 24, "one\n\x1b[34mtwo\x1b[0m\n"),
    tail(80, 24, "one\nthree\n"),
    tail(100, 30, "wide\n"),
    tail(100, 2, "a\nb\nc\n"),
  );
  const { events, statuses, screen, onStatus } = recorder();
  const sizes: string[] = [];
  const reader = new TerminalTail("s1", screen, onStatus, (size) =>
    sizes.push(`${size.cols}x${size.rows}`),
  );
  reader.start();
  await vi.advanceTimersByTimeAsync(0);
  expect(request.mock.calls[0]?.[0]).toBe(`${terminalPath("s1")}/tail`);
  expect(request.mock.calls[0]?.[1].body).toBe("{}");
  await vi.advanceTimersByTimeAsync(15_000);
  expect(events).toEqual([
    "reset 80x24",
    `${TAIL_HEAD}one\r\n\x1b[34mtwo\x1b[0m`,
    `${TAIL_HEAD}one\r\nthree`,
    "reset 100x30",
    `${TAIL_HEAD}wide`,
    // Resized between herdr's two reads: the bottom rows fit, nothing scrolls.
    "reset 100x2",
    `${TAIL_HEAD}b\r\nc`,
  ]);
  expect(sizes).toEqual(["80x24", "100x30", "100x2"]);
  expect(statuses).toEqual(["connecting", "tail", "tail", "tail", "tail"]);
  reader.stop();
});

test("a tail stays up when herdr or the daemon fails, and a closed pane ends it", async () => {
  vi.useFakeTimers();
  const { request } = opens(
    tail(80, 24, "one\n"),
    refusal(503, "unavailable", "herdr is unreachable"),
    refusal(500, "internal", "the read panicked"),
    refusal(404, "not_found", "no pane for s1"),
  );
  const { statuses, screen, onStatus } = recorder();
  const reader = new TerminalTail("s1", screen, onStatus, () => {});
  reader.start();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(statuses).toEqual(["connecting", "tail"]);
  await vi.advanceTimersByTimeAsync(5_000);
  expect(statuses).toEqual(["connecting", "tail", "ended"]);
  // An ended tail reads no more.
  await vi.advanceTimersByTimeAsync(20_000);
  expect(request).toHaveBeenCalledTimes(4);
  reader.stop();
});

test("a tail that never drew says herdr is unreachable, and keeps trying", async () => {
  vi.useFakeTimers();
  const { request } = opens(refusal(503, "unavailable", "herdr is unreachable"), tail(80, 24, "x"));
  const { statuses, screen, onStatus } = recorder();
  const reader = new TerminalTail("s1", screen, onStatus, () => {});
  reader.start();
  await vi.advanceTimersByTimeAsync(0);
  expect(statuses).toEqual(["connecting", "unreachable"]);
  await vi.advanceTimersByTimeAsync(5_000);
  expect(request).toHaveBeenCalledTimes(2);
  expect(statuses.at(-1)).toBe("tail");
  reader.stop();
});

test("a tail whose daemon never answers says so, and keeps trying", async () => {
  vi.useFakeTimers();
  const request = vi.fn(async () => {
    throw new TypeError("Failed to fetch");
  });
  vi.stubGlobal("fetch", request);
  const statuses: MirrorStatus[] = [];
  const reader = new TerminalTail(
    "s1",
    recorder().screen,
    (status) => statuses.push(status),
    () => {},
  );
  reader.start();
  await vi.advanceTimersByTimeAsync(5_000);
  expect(statuses.at(-1)).toMatchObject({
    kind: "unreachable",
    detail: "The daemon did not answer.",
  });
  expect(request).toHaveBeenCalledTimes(2);
  reader.stop();
});

test("a retry during a failed read leaves one reader, not two", async () => {
  vi.useFakeTimers();
  let answer: (response: Response) => void = () => {};
  const request = vi.fn(
    (_path: string, init: RequestInit) =>
      new Promise<Response>((resolve, reject) => {
        answer = resolve;
        init.signal?.addEventListener("abort", () => reject(new DOMException("aborted")));
      }),
  );
  vi.stubGlobal("fetch", request);
  const reader = new TerminalTail(
    "s1",
    recorder().screen,
    () => {},
    () => {},
  );
  reader.start();
  await vi.advanceTimersByTimeAsync(0);
  const first = answer;
  reader.retry();
  await vi.advanceTimersByTimeAsync(0);
  // The read from before the retry answers late; only the new one goes on.
  first(refusal(503, "unavailable", "herdr is unreachable"));
  answer(tail(80, 24, "x"));
  await vi.advanceTimersByTimeAsync(5_000);
  expect(request).toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(0);
  answer(tail(80, 24, "y"));
  await vi.advanceTimersByTimeAsync(5_000);
  expect(request).toHaveBeenCalledTimes(4);
  reader.stop();
});
