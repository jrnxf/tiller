// The live mirror of an agent's herdr pane (protocol.md 7.4). The daemon
// streams `herdr terminal session observe` as NDJSON: a `size` line, then
// frames, the first of each run a full screen and the rest diffs against the
// one before. A diff only makes sense on top of the frame before it, so a gap
// in `seq`, or a diff with no full screen under it, drops the stream and opens
// it again for a fresh full screen. Keys go back one request at a time.
//
// With a Fit the open asks herdr to size the pane to the board's columns and
// rows while the stream is open; herdr gets its own size back when it ends.
// Herdr can refuse, and then the stream says so and goes on unfitted. A new
// size for an open Fit goes in place, so the pane never drops back between
// two sizes; the stream answers it with a `size` line and a full screen.

export const terminalPath = (sessionId: string) => `/api/v1/agents/${sessionId}/terminal`;
const terminalInputPath = (sessionId: string) => `${terminalPath(sessionId)}/input`;
const terminalFitPath = (sessionId: string) => `${terminalPath(sessionId)}/fit`;
const terminalHistoryPath = (sessionId: string) => `${terminalPath(sessionId)}/history`;

// What the mirror draws on: xterm in the board, a recorder in tests.
export interface Screen {
  // A new size, or a new run of frames: start again from a blank screen.
  reset(cols: number, rows: number): void;
  write(bytes: Uint8Array): void;
}

export type MirrorStatus =
  | { kind: "connecting" }
  | { kind: "live" }
  | { kind: "reconnecting"; attempt: number }
  | { kind: "unreachable"; title: string; detail: string }
  // The pane closed; the screen keeps its last frame.
  | { kind: "ended"; at: Date }
  | { kind: "forbidden" }
  | { kind: "remote"; detail: string }
  | { kind: "failed"; detail: string };

export interface TerminalSize {
  cols: number;
  rows: number;
}

// The Fit: the size to ask for at each open, or null for none, and what to do
// when herdr will not resize the pane.
export interface Fitting {
  size: () => TerminalSize | null;
  onUnfit: (message: string) => void;
}

const sameSize = (one: TerminalSize | null, other: TerminalSize | null) =>
  one === other || (!!one && !!other && one.cols === other.cols && one.rows === other.rows);

type Line =
  | { type: "size"; cols: number; rows: number }
  | { type: "frame"; seq: number; full: boolean; bytes: string }
  | { type: "closed"; reason: string }
  | { type: "unavailable"; message: string }
  | { type: "unfit"; message: string };

const UNREACHABLE_TITLE = "Can't reach herdr on this machine";
const UNREACHABLE_DETAIL = "The agent may still be running. Tiller tries again on its own, or now.";
// Tries in a row with no live screen before the mirror says herdr is gone
// rather than that the stream dropped.
const QUIET_TRIES = 4;
const MAX_DELAY = 8000;
const SLOW_DELAY = 10_000;

function decode(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

async function errorOf(response: Response): Promise<{ code: string; message: string }> {
  try {
    const parsed = (await response.json()) as { error?: { code?: unknown; message?: unknown } };
    return {
      code: String(parsed.error?.code ?? ""),
      message: String(parsed.error?.message ?? `The daemon answered ${response.status}.`),
    };
  } catch {
    return { code: "", message: `The daemon answered ${response.status}.` };
  }
}

const NOT_OWNER =
  "This browser is not signed in as the owner. Run `tiller board --login` for a login link.";

// The pane's scrollback (protocol.md 7.4, History): its last rows as herdr
// renders them, oldest first and the screen itself last. The stream carries
// the screen only, so this is the one way to read back.
export type History = { ok: true; text: string } | { ok: false; detail: string };

export async function readHistory(sessionId: string, signal?: AbortSignal): Promise<History> {
  try {
    const response = await fetch(terminalHistoryPath(sessionId), {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: "{}",
      signal,
    });
    // Not a mutation, so the body is the read itself, not wrapped in `data`.
    if (response.ok) return { ok: true, text: ((await response.json()) as { text: string }).text };
    if (response.status === 403) return { ok: false, detail: NOT_OWNER };
    return { ok: false, detail: (await errorOf(response)).message };
  } catch {
    return { ok: false, detail: "The daemon did not answer." };
  }
}

// Why a stream ended, and what the mirror does next.
type Outcome =
  | { next: "retry"; title?: string; detail?: string }
  | { next: "stop"; status: MirrorStatus };

export class TerminalStream {
  private readonly path: string;
  private readonly fitPath: string;
  private readonly screen: Screen;
  private readonly onStatus: (status: MirrorStatus) => void;
  private readonly onSize: (size: TerminalSize) => void;
  private readonly fitting: Fitting | null;
  // The Fit the open stream holds.
  private fitted: TerminalSize | null = null;
  private abort: AbortController | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  // Opens in a row that ended before a live screen.
  private failures = 0;
  private status: MirrorStatus = { kind: "connecting" };

  constructor(
    sessionId: string,
    screen: Screen,
    onStatus: (status: MirrorStatus) => void,
    onSize: (size: TerminalSize) => void,
    fitting: Fitting | null = null,
  ) {
    this.path = terminalPath(sessionId);
    this.fitPath = terminalFitPath(sessionId);
    this.screen = screen;
    this.onStatus = onStatus;
    this.onSize = onSize;
    this.fitting = fitting;
  }

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    this.failures = 0;
    this.set({ kind: "connecting" });
    void this.open();
  }

  // The viewer left: dropping the request ends the daemon's observe process.
  stop() {
    this.stopped = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.abort?.abort();
    this.abort = null;
  }

  // The Retry button: open now instead of at the next try.
  retry() {
    this.stop();
    this.start();
  }

  // The Fit wanted changed. A live Fit takes a new size in place; turning
  // the Fit on or off opens again, and the screen keeps the last frame until
  // the new open's first full screen replaces it. A retry waiting to open
  // takes the new Fit when it does, and a stream that ended stays ended.
  refit() {
    const want = this.fitting?.size() ?? null;
    if (this.stopped || sameSize(want, this.fitted)) return;
    if (this.timer !== null || !this.abort) return;
    if (want && this.fitted && this.status.kind === "live") {
      void this.resize(this.abort, want);
      return;
    }
    this.reopen();
  }

  private reopen() {
    this.abort?.abort();
    this.abort = null;
    void this.open();
  }

  private async resize(stream: AbortController, size: TerminalSize) {
    this.fitted = size;
    let sent = false;
    try {
      const response = await fetch(this.fitPath, {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(size),
      });
      sent = response.ok;
    } catch {
      // The daemon did not answer: a new open asks again.
    }
    // The stream that held the Fit is gone, or herdr let it go: open again.
    if (!sent && !this.stopped && this.abort === stream) this.reopen();
  }

  private set(status: MirrorStatus) {
    this.status = status;
    this.onStatus(status);
  }

  private async open() {
    const abort = new AbortController();
    this.abort = abort;
    let outcome: Outcome;
    try {
      outcome = await this.read(abort.signal);
    } catch {
      outcome = { next: "retry" };
    }
    if (this.stopped || this.abort !== abort) return;
    abort.abort();
    this.abort = null;
    if (outcome.next === "stop") {
      this.set(outcome.status);
      return;
    }
    this.failures += 1;
    if (this.failures >= QUIET_TRIES || outcome.title) {
      this.set({
        kind: "unreachable",
        title: outcome.title ?? UNREACHABLE_TITLE,
        detail: outcome.detail ?? UNREACHABLE_DETAIL,
      });
    } else {
      this.set({ kind: "reconnecting", attempt: this.failures });
    }
    const delay =
      this.failures >= QUIET_TRIES
        ? SLOW_DELAY
        : Math.min(250 * 2 ** (this.failures - 1), MAX_DELAY);
    this.timer = setTimeout(() => {
      this.timer = null;
      if (!this.stopped) void this.open();
    }, delay);
  }

  private async read(signal: AbortSignal): Promise<Outcome> {
    const fit = this.fitting?.size() ?? null;
    this.fitted = fit;
    const response = await fetch(this.path, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/x-ndjson" },
      body: fit ? JSON.stringify({ fit }) : "{}",
      signal,
    });
    if (!response.ok) return this.refused(response);
    if (!response.body) return { next: "retry" };
    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
    let pending = "";
    // The size of the next run, applied with its first full screen so the
    // last screen stays up until a new one can replace it.
    let size: TerminalSize | null = null;
    let last: number | null = null;
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return { next: "retry" };
      pending += value;
      let newline = pending.indexOf("\n");
      while (newline !== -1) {
        const text = pending.slice(0, newline);
        pending = pending.slice(newline + 1);
        newline = pending.indexOf("\n");
        if (!text.trim()) continue;
        const line = JSON.parse(text) as Line;
        if (line.type === "size") {
          size = { cols: line.cols, rows: line.rows };
          last = null;
        } else if (line.type === "frame") {
          if (!line.full && (last === null || line.seq !== last + 1)) return { next: "retry" };
          if (line.full && size) {
            this.screen.reset(size.cols, size.rows);
            this.onSize(size);
            size = null;
          }
          this.screen.write(decode(line.bytes));
          last = line.seq;
          if (this.status.kind !== "live") {
            this.failures = 0;
            this.set({ kind: "live" });
          }
        } else if (line.type === "closed") {
          return { next: "stop", status: { kind: "ended", at: new Date() } };
        } else if (line.type === "unavailable") {
          return { next: "retry" };
        } else if (line.type === "unfit") {
          this.fitted = null;
          this.fitting?.onUnfit(line.message);
        }
      }
    }
  }

  private async refused(response: Response): Promise<Outcome> {
    const error = await errorOf(response);
    if (response.status === 403) return { next: "stop", status: { kind: "forbidden" } };
    if (response.status === 422) {
      return { next: "stop", status: { kind: "remote", detail: error.message } };
    }
    if (response.status === 404) {
      return { next: "stop", status: { kind: "ended", at: new Date() } };
    }
    if (response.status === 503) {
      return error.message.startsWith("herdr is unreachable")
        ? { next: "retry", title: UNREACHABLE_TITLE, detail: UNREACHABLE_DETAIL }
        : { next: "retry", title: "The terminal is not available", detail: error.message };
    }
    return { next: "stop", status: { kind: "failed", detail: error.message } };
  }
}

// The daemon's limits on one input request (protocol.md 7.4).
const MAX_KEYS = 64;
const MAX_TEXT = 4096;
const RATE_PAUSE = 300;
const RATE_TRIES = 20;

type Input = { keys: string[] } | { text: string };

// Splits a paste into pieces the daemon takes, never inside a character.
export function pasteChunks(text: string): string[] {
  const encoder = new TextEncoder();
  const chunks: string[] = [];
  let chunk = "";
  let size = 0;
  for (const char of text) {
    const bytes = encoder.encode(char).length;
    if (size + bytes > MAX_TEXT) {
      chunks.push(chunk);
      chunk = "";
      size = 0;
    }
    chunk += char;
    size += bytes;
  }
  if (chunk) chunks.push(chunk);
  return chunks;
}

// What the owner types, sent in order with one request in flight. Keys typed
// while a request is out are sent together in the next one.
export class TerminalInput {
  private readonly path: string;
  private readonly onRefused: (title: string, detail: string) => void;
  private readonly queue: Input[] = [];
  private busy = false;
  private closed = false;

  constructor(sessionId: string, onRefused: (title: string, detail: string) => void) {
    this.path = terminalInputPath(sessionId);
    this.onRefused = onRefused;
  }

  keys(keys: string[]) {
    if (keys.length === 0) return;
    const tail = this.queue.at(-1);
    let rest = keys;
    if (tail && "keys" in tail && tail.keys.length < MAX_KEYS) {
      const room = MAX_KEYS - tail.keys.length;
      tail.keys.push(...rest.slice(0, room));
      rest = rest.slice(room);
    }
    for (let index = 0; index < rest.length; index += MAX_KEYS) {
      this.queue.push({ keys: rest.slice(index, index + MAX_KEYS) });
    }
    void this.pump();
  }

  text(text: string) {
    for (const chunk of pasteChunks(text)) this.queue.push({ text: chunk });
    void this.pump();
  }

  close() {
    this.closed = true;
    this.queue.length = 0;
  }

  private async pump() {
    if (this.busy) return;
    this.busy = true;
    try {
      // The request in flight is off the queue, so new keys never join it.
      for (let input = this.queue.shift(); input && !this.closed; input = this.queue.shift()) {
        if ((await this.send(input)) === "drop-all") {
          this.queue.length = 0;
          break;
        }
      }
    } finally {
      this.busy = false;
    }
  }

  private async send(input: Input): Promise<"sent" | "drop" | "drop-all"> {
    for (let tries = 0; ; tries += 1) {
      let response: Response;
      try {
        response = await fetch(this.path, {
          method: "POST",
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify(input),
        });
      } catch {
        this.onRefused("Keys not sent", "The daemon did not answer.");
        return "drop-all";
      }
      if (response.ok) return "sent";
      const error = await errorOf(response);
      if (
        response.status === 503 &&
        error.message.startsWith("input came faster") &&
        tries < RATE_TRIES
      ) {
        await new Promise((resolve) => setTimeout(resolve, RATE_PAUSE));
        if (this.closed) return "drop-all";
        continue;
      }
      if (response.status === 400) {
        this.onRefused("Key not sent", error.message);
        return "drop";
      }
      if (response.status === 403) {
        this.onRefused("Keys not sent", NOT_OWNER);
        return "drop-all";
      }
      this.onRefused("Keys not sent", error.message);
      return "drop-all";
    }
  }
}
