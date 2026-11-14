import type { EventSourceLike } from "./sse";
import { ownerFetch } from "./owner";

// The change stream over `fetch`, in place of EventSource: EventSource cannot
// send a header, and every request to the daemon carries the owner token
// (owner.ts). The URL is no place for the token: it would reach the request
// line, and so any log of it.
//
// It reads `text/event-stream` as the HTML standard does: `event`, `data` and
// `id` fields, comments, and a blank line ending each message. Unlike
// EventSource it never reconnects by itself: an end, a refusal or a cut moves
// it to CLOSED with an `error`, and the caller opens a new one after the last
// seq it applied (sse.ts).

const CONNECTING = 0;
const OPEN = 1;
const CLOSED = 2;

type Listener = (event: MessageEvent<string>) => void;

export class FetchEventSource implements EventSourceLike {
  readyState = CONNECTING;
  private readonly listeners = new Map<string, Set<Listener>>();
  private readonly abort = new AbortController();

  constructor(url: string) {
    void this.run(url);
  }

  addEventListener(type: string, listener: Listener | (() => void)): void {
    const set = this.listeners.get(type) ?? new Set<Listener>();
    set.add(listener as Listener);
    this.listeners.set(type, set);
  }

  close(): void {
    this.readyState = CLOSED;
    this.abort.abort();
  }

  private emit(type: string, event: MessageEvent<string> = new MessageEvent(type)) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }

  private async run(url: string) {
    try {
      const response = await ownerFetch(url, {
        headers: { accept: "text/event-stream" },
        cache: "no-store",
        signal: this.abort.signal,
      });
      if (!response.ok || !response.body || this.readyState === CLOSED) {
        throw new Error(`the stream answered ${response.status}`);
      }
      this.readyState = OPEN;
      this.emit("open");
      await this.read(response.body);
    } catch {
      // Reported below, as EventSource reports it: an `error`, no reason.
    }
    if (this.abort.signal.aborted) return;
    this.readyState = CLOSED;
    this.emit("error");
  }

  private async read(body: ReadableStream<Uint8Array>) {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let type = "";
    let data: string[] = [];
    let id = "";
    const field = (line: string) => {
      if (line === "") {
        if (data.length > 0) {
          const event = new MessageEvent(type || "message", {
            data: data.join("\n"),
            lastEventId: id,
          });
          this.emit(event.type, event);
        }
        type = "";
        data = [];
        return;
      }
      if (line.startsWith(":")) return;
      const colon = line.indexOf(":");
      const name = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (name === "event") type = value;
      else if (name === "data") data.push(value);
      else if (name === "id" && !value.includes("\0")) id = value;
    };
    for (;;) {
      const { done, value } = await reader.read();
      if (done || this.readyState === CLOSED) return;
      buffer += decoder.decode(value, { stream: true });
      // A lone CR may be the first half of a CRLF still to come: kept.
      const lines = buffer.split(/\r\n|\n|\r(?!$)/);
      buffer = lines.pop() ?? "";
      for (const line of lines) field(line);
    }
  }
}
