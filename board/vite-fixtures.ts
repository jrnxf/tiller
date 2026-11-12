import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { resolve } from "node:path";

import type { Connect, Plugin } from "vite";

import type { BoardRow } from "./src/lib/api-types.ts";
import { fixtureMutation, withoutDeleted } from "./src/lib/fixture-mutations.ts";
import {
  DIALOG_SESSIONS,
  FixtureRefusal,
  SCENARIOS,
  bootScript,
  fixtureResource,
  fixtureSnapshot,
} from "./src/lib/fixtures.ts";
import type { Scenario } from "./src/lib/fixtures.ts";
import {
  FIXTURE_COLS,
  FIXTURE_ROWS,
  fullFrame,
  historyText,
  menuText,
  promptFrame,
  screenText,
  tickFrame,
  typeInto,
} from "./src/lib/fixtures-terminal.ts";
import type { PaneSize } from "./src/lib/fixtures-terminal.ts";

// Stands in for the daemon until T2.8 lands: serves the board API from
// fixtures and inlines the boot snapshot into index.html the way the daemon
// will. `?fixture=<scenario>` on the page URL picks the scenario.
function scenarioOf(url: string | undefined, referer: string | undefined): Scenario {
  for (const candidate of [url, referer]) {
    if (!candidate) continue;
    const name = new URL(candidate, "http://localhost").searchParams.get("fixture");
    if (name && (SCENARIOS as readonly string[]).includes(name)) return name as Scenario;
  }
  return "default";
}

// The boot snapshot as the stand-in server has it now: no deleted project.
function snapshotOf(scenario: Scenario) {
  const snapshot = fixtureSnapshot(scenario);
  const queries = Object.entries(snapshot.queries).map(([path, body]) => [
    path,
    withoutDeleted(path, body, scenario),
  ]);
  return { ...snapshot, queries: Object.fromEntries(queries) as typeof snapshot.queries };
}

function sendJson(res: ServerResponse, body: unknown) {
  res.setHeader("content-type", "application/json");
  res.setHeader("cache-control", "no-store");
  res.end(JSON.stringify(body));
}

// Open event streams, so a fixture mutation can push its later receipt stages.
const streams = new Set<ServerResponse>();
let seq = 100_000;

function broadcast(event: object) {
  seq += 1;
  const data = JSON.stringify({ seq, at: new Date().toISOString(), ...event });
  for (const stream of streams) stream.write(`id: ${seq}\nevent: change\ndata: ${data}\n\n`);
}

function readBody(req: IncomingMessage): Promise<Record<string, unknown>> {
  return new Promise((done, fail) => {
    let text = "";
    req.on("data", (chunk: Buffer) => (text += chunk.toString()));
    req.on("error", fail);
    req.on("aborted", () => fail(new Error("request aborted")));
    req.on("end", () => {
      try {
        done(JSON.parse(text) as Record<string, unknown>);
      } catch {
        done({});
      }
    });
  });
}

// The stand-in panes: what was typed at each one's prompt, and the streams
// watching it, so a key shows on every open mirror.
interface FixturePane {
  typed: string;
  started: number;
  // Each open mirror, drawing what was typed at its own size.
  viewers: Set<(typed: string) => void>;
  // The open mirror holding a Fit, which takes a new size in place.
  refit: ((size: PaneSize) => void) | null;
}
const panes = new Map<string, FixturePane>();

function paneOf(session: string): FixturePane {
  let pane = panes.get(session);
  if (!pane) {
    pane = { typed: "", started: Date.now(), viewers: new Set(), refit: null };
    panes.set(session, pane);
  }
  return pane;
}

// `?term=<mode>` on the page URL picks how the mirror's pane behaves: live (the
// default), ended, reconnect, unreachable, holder-down (the terminal holder is
// away), forbidden, or unfit (herdr refuses a Fit). `peer-down` refuses a peer
// host's pane as an unreachable link; a pane here still streams.
function terminalMode(referer: string | undefined): string {
  if (!referer) return "live";
  return new URL(referer, "http://localhost").searchParams.get("term") ?? "live";
}

// Tries to open the stream after a `reconnect` pane's first, per session: they
// hang, so the mirror stays on Reconnecting.
const dropped = new Set<string>();

// The Fit a stream open asks for, as the daemon takes it.
function fitOf(body: Record<string, unknown>): PaneSize | null {
  const fit = body.fit as { cols?: unknown; rows?: unknown } | undefined;
  if (!fit || typeof fit.cols !== "number" || typeof fit.rows !== "number") return null;
  return { cols: fit.cols, rows: fit.rows };
}

function terminalStream(
  req: IncomingMessage,
  res: ServerResponse,
  session: string,
  body: Record<string, unknown>,
) {
  const mode = terminalMode(req.headers.referer);
  if (mode === "forbidden") {
    res.statusCode = 403;
    return sendJson(res, {
      ok: false,
      error: { code: "forbidden", message: "only the owner sees an agent's terminal" },
    });
  }
  if (mode === "unreachable") {
    res.statusCode = 503;
    return sendJson(res, {
      ok: false,
      error: { code: "unavailable", message: "herdr is unreachable: no socket" },
    });
  }
  if (mode === "holder-down") {
    res.statusCode = 503;
    return sendJson(res, {
      ok: false,
      error: {
        code: "unavailable",
        message: "the terminal holder is unreachable: no socket",
      },
    });
  }
  if (mode === "peer-down" && typeof body.host === "string") {
    res.statusCode = 503;
    return sendJson(res, {
      ok: false,
      error: {
        code: "peer_unreachable",
        message: `${body.host} is unreachable; nothing was sent or queued`,
        host: body.host,
      },
    });
  }
  if (mode !== "reconnect") dropped.delete(session);
  else if (dropped.has(session)) return;
  const pane = paneOf(session);
  const seconds = () => Math.round((Date.now() - pane.started) / 1000);
  res.writeHead(200, { "content-type": "application/x-ndjson", "cache-control": "no-store" });
  let frame = 1;
  const send = (line: object) => res.write(`${JSON.stringify(line)}\n`);
  const diff = (bytes: string) => {
    frame += 1;
    send({ type: "frame", seq: frame, full: false, bytes });
  };
  const own = { cols: FIXTURE_COLS, rows: FIXTURE_ROWS };
  const fit = fitOf(body);
  let size = fit && mode !== "unfit" ? fit : own;
  if (fit && mode === "unfit") {
    send({ type: "size", ...fit });
    send({
      type: "unfit",
      message: `herdr: terminal ${session} already has an attached client; retry with --takeover`,
    });
  }
  send({ type: "size", ...size });
  send({ type: "frame", seq: frame, full: true, bytes: fullFrame(pane.typed, seconds(), size) });
  if (mode === "ended") {
    send({ type: "closed", reason: "pane closed" });
    return res.end();
  }
  if (mode === "reconnect") {
    dropped.add(session);
    return res.end();
  }
  const typed = (text: string) => diff(promptFrame(text, size));
  pane.viewers.add(typed);
  const refit = (next: PaneSize) => {
    size = next;
    send({ type: "size", ...size });
    frame += 1;
    send({ type: "frame", seq: frame, full: true, bytes: fullFrame(pane.typed, seconds(), size) });
  };
  if (size === fit) pane.refit = refit;
  const ticker = setInterval(() => diff(tickFrame(pane.typed, seconds(), size)), 1000);
  // The response, not the request: the request is done once its body is read.
  res.on("close", () => {
    clearInterval(ticker);
    pane.viewers.delete(typed);
    if (pane.refit === refit) pane.refit = null;
  });
}

function terminalFit(res: ServerResponse, session: string, body: Record<string, unknown>) {
  const pane = paneOf(session);
  const size = fitOf({ fit: body });
  if (!pane.refit || !size) {
    res.statusCode = 422;
    return sendJson(res, {
      ok: false,
      error: { code: "refused", message: "no open terminal stream holds a Fit" },
    });
  }
  pane.refit(size);
  sendJson(res, { ok: true, data: { session_id: session, fit: size } });
}

function terminalInput(res: ServerResponse, session: string, body: Record<string, unknown>) {
  const pane = paneOf(session);
  pane.typed = typeInto(pane.typed, body);
  for (const viewer of pane.viewers) viewer(pane.typed);
  const sent = Array.isArray(body.keys) ? body.keys.length : 1;
  sendJson(res, { ok: true, data: { session_id: session, sent } });
}

// A history read: refused like the stream for a forbidden pane, and slow
// enough under `?term=slowhistory` to see the board wait for it.
function terminalHistory(req: IncomingMessage, res: ServerResponse, session: string) {
  const mode = terminalMode(req.headers.referer);
  if (mode === "forbidden") {
    res.statusCode = 403;
    return sendJson(res, {
      ok: false,
      error: { code: "forbidden", message: "only the owner reads an agent's terminal" },
    });
  }
  const pane = paneOf(session);
  const seconds = Math.round((Date.now() - pane.started) / 1000);
  const text = historyText(pane.typed, seconds, { cols: FIXTURE_COLS, rows: FIXTURE_ROWS });
  setTimeout(
    () => sendJson(res, { session_id: session, text }),
    mode === "slowhistory" ? 60_000 : 150,
  );
}

// A tail read of a pane on a saved machine: refused like the stream for a
// forbidden pane, gone for an ended one, and herdr away for an unreachable
// one.
function terminalTail(req: IncomingMessage, res: ServerResponse, session: string) {
  const mode = terminalMode(req.headers.referer);
  const refusals: Record<string, [number, string, string]> = {
    forbidden: [403, "forbidden", "only the owner reads an agent's terminal"],
    ended: [404, "not_found", `no pane for ${session}`],
    unreachable: [503, "unavailable", "herdr is unreachable: no socket"],
  };
  const refused = refusals[mode];
  if (refused) {
    const [status, code, message] = refused;
    res.statusCode = status;
    return sendJson(res, { ok: false, error: { code, message } });
  }
  const pane = paneOf(session);
  const seconds = Math.round((Date.now() - pane.started) / 1000);
  // The `dialog` scenario's beta-7 has a menu up the board cannot read.
  const text = session === DIALOG_SESSIONS.unknown ? menuText() : screenText(pane.typed, seconds);
  sendJson(res, { session_id: session, cols: FIXTURE_COLS, rows: FIXTURE_ROWS, text });
}

function startMover(res: ServerResponse) {
  const board = fixtureResource("/api/v1/board?filter=all", "live") as { rows: BoardRow[] };
  const row = board.rows.find((candidate) => candidate.display_id === "alpha-42");
  if (!row) return undefined;
  let moverSeq = 4181;
  let landing = false;
  return setInterval(() => {
    landing = !landing;
    moverSeq += 1;
    const card = {
      ...row,
      column: landing ? "landing" : "working",
      badge: landing ? "checks" : "working",
    };
    const change = {
      seq: moverSeq,
      at: new Date().toISOString(),
      kind: "card.changed",
      txn: "fixture",
      entity: "card",
      entity_id: row.id,
      rev_after: moverSeq,
      card_id: row.id,
      entity_state: null,
      card,
    };
    res.write(`id: ${moverSeq}\nevent: change\ndata: ${JSON.stringify(change)}\n\n`);
  }, 4000);
}

// The stand-in for an `--allow-host` name's sign-in: Settings' Sign out drops
// it, and the code below signs back in. Signed out, every read is refused and
// the page has no boot snapshot, so the board shows its signed-out screen.
let signedIn = true;
const FIXTURE_CODE = "12345678";

function refuse(res: ServerResponse, code: string, message: string) {
  res.statusCode = 403;
  sendJson(res, { ok: false, error: { code, message } });
}

// Answers the session endpoints; true when it did.
function sessionRoute(req: IncomingMessage, res: ServerResponse, path: string): boolean {
  if (path === "/api/v1/session") {
    sendJson(res, { owner: signedIn });
    return true;
  }
  if (req.method !== "POST" || (path !== "/api/v1/login" && path !== "/api/v1/logout")) {
    return false;
  }
  readBody(req)
    .then((body) => {
      if (path === "/api/v1/logout") {
        signedIn = false;
        for (const stream of streams) stream.end();
        streams.clear();
      } else if (body.code === FIXTURE_CODE) {
        signedIn = true;
      } else {
        return refuse(res, "wrong_code", "that code is wrong or has expired");
      }
      sendJson(res, { owner: signedIn });
    })
    .catch(() => res.destroy());
  return true;
}

function apiMiddleware(): Connect.NextHandleFunction {
  return (req: IncomingMessage, res, next) => {
    const url = req.url ?? "";
    if (!url.startsWith("/api/v1/")) return next();
    const scenario = scenarioOf(undefined, req.headers.referer);
    if (sessionRoute(req, res, url.split("?")[0] ?? url)) return;
    if (!signedIn) return refuse(res, "signed_out", "this board is not signed in");

    const terminal = /^\/api\/v1\/agents\/([^/]+)\/terminal(\/input|\/fit|\/history|\/tail)?$/.exec(
      url,
    );
    if (terminal?.[1] && req.method === "POST") {
      const session = terminal[1];
      readBody(req)
        .then((body) => {
          if (terminal[2] === "/input") terminalInput(res, session, body);
          else if (terminal[2] === "/fit") terminalFit(res, session, body);
          else if (terminal[2] === "/history") terminalHistory(req, res, session);
          else if (terminal[2] === "/tail") terminalTail(req, res, session);
          else terminalStream(req, res, session, body);
        })
        .catch(() => res.destroy());
      return;
    }

    // Every mutation method the board sends (`sendMutation`), DELETE included:
    // ending away mode and removing a dependency are DELETEs.
    if (["POST", "PATCH", "PUT", "DELETE"].includes(req.method ?? "")) {
      readBody(req)
        .then((body) => {
          const { reply, followUps } = fixtureMutation(req.method ?? "POST", url, body, scenario);
          res.statusCode = reply.status;
          sendJson(res, reply.body);
          for (const { delayMs, event } of followUps) setTimeout(() => broadcast(event), delayMs);
        })
        .catch((error: unknown) => {
          if (res.writableEnded || res.destroyed) return;
          res.statusCode = 500;
          sendJson(res, { ok: false, error: { code: "internal", message: String(error) } });
        });
      return;
    }

    if (url.split("?")[0] === "/api/v1/events") {
      // `?fixture=degraded`: the daemon is unreachable, so the board keeps the
      // reconnecting banner up.
      if (scenario === "degraded") {
        res.statusCode = 503;
        return res.end();
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
      res.write("retry: 1000\n\n");
      streams.add(res);
      const keepalive = setInterval(() => res.write(": keepalive\n\n"), 15_000);
      // `?fixture=live`: one card moves between Working and Landing every few
      // seconds, so live updates can be watched without a daemon.
      const mover = scenario === "live" ? startMover(res) : undefined;
      req.on("close", () => {
        clearInterval(keepalive);
        clearInterval(mover);
        streams.delete(res);
      });
      return;
    }
    if (url === "/api/v1/snapshot") return sendJson(res, snapshotOf(scenario));

    const resource = withoutDeleted(url, fixtureResource(url, scenario), scenario);
    if (resource instanceof FixtureRefusal) {
      res.statusCode = resource.status;
      return sendJson(res, resource.body);
    }
    if (resource !== undefined) return sendJson(res, resource);
    res.statusCode = 404;
    sendJson(res, { ok: false, error: "not found" });
  };
}

export function tillerFixtures(): Plugin {
  return {
    name: "tiller-fixtures",
    configureServer(server) {
      server.middlewares.use(apiMiddleware());
    },
    transformIndexHtml: {
      order: "pre",
      handler(html, ctx) {
        if (!ctx.server) return html;
        if (!signedIn) return html;
        const scenario = scenarioOf(ctx.originalUrl, undefined);
        return html.replace("</head>", `${bootScript(snapshotOf(scenario))}</head>`);
      },
    },
    configurePreviewServer(server) {
      server.middlewares.use(apiMiddleware());
      server.middlewares.use((req, res, next) => {
        const path = (req.url ?? "").split("?")[0];
        if (path !== "/" && path !== "/index.html") return next();
        const dist = resolve(server.config.root, server.config.build.outDir, "index.html");
        readFile(dist, "utf8")
          .then((html) => {
            res.setHeader("content-type", "text/html");
            if (!signedIn) return res.end(html);
            const scenario = scenarioOf(req.url, undefined);
            res.end(html.replace("</head>", `${bootScript(snapshotOf(scenario))}</head>`));
          })
          .catch(next);
      });
    },
  };
}
