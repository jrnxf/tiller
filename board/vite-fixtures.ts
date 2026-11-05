import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { resolve } from "node:path";

import type { Connect, Plugin } from "vite";

import type { BoardRow } from "./src/lib/api-types.ts";
import { fixtureMutation, withoutDeleted } from "./src/lib/fixture-mutations.ts";
import { SCENARIOS, bootScript, fixtureResource, fixtureSnapshot } from "./src/lib/fixtures.ts";
import type { Scenario } from "./src/lib/fixtures.ts";
import {
  FIXTURE_COLS,
  FIXTURE_ROWS,
  fullFrame,
  promptFrame,
  tickFrame,
  typeInto,
} from "./src/lib/fixtures-terminal.ts";

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
  viewers: Set<(bytes: string) => void>;
}
const panes = new Map<string, FixturePane>();

function paneOf(session: string): FixturePane {
  let pane = panes.get(session);
  if (!pane) {
    pane = { typed: "", started: Date.now(), viewers: new Set() };
    panes.set(session, pane);
  }
  return pane;
}

// `?term=<mode>` on the page URL picks how the mirror's pane behaves: live (the
// default), ended, reconnect, unreachable or forbidden.
function terminalMode(referer: string | undefined): string {
  if (!referer) return "live";
  return new URL(referer, "http://localhost").searchParams.get("term") ?? "live";
}

// Tries to open the stream after a `reconnect` pane's first, per session: they
// hang, so the mirror stays on Reconnecting.
const dropped = new Set<string>();

function terminalStream(req: IncomingMessage, res: ServerResponse, session: string) {
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
  send({ type: "size", cols: FIXTURE_COLS, rows: FIXTURE_ROWS });
  send({ type: "frame", seq: frame, full: true, bytes: fullFrame(pane.typed, seconds()) });
  if (mode === "ended") {
    send({ type: "closed", reason: "pane closed" });
    return res.end();
  }
  if (mode === "reconnect") {
    dropped.add(session);
    return res.end();
  }
  pane.viewers.add(diff);
  const ticker = setInterval(() => diff(tickFrame(pane.typed, seconds())), 1000);
  // The response, not the request: the request is done once its body is read.
  res.on("close", () => {
    clearInterval(ticker);
    pane.viewers.delete(diff);
  });
}

function terminalInput(res: ServerResponse, session: string, body: Record<string, unknown>) {
  const pane = paneOf(session);
  pane.typed = typeInto(pane.typed, body);
  for (const viewer of pane.viewers) viewer(promptFrame(pane.typed));
  const sent = Array.isArray(body.keys) ? body.keys.length : 1;
  sendJson(res, { ok: true, data: { session_id: session, sent } });
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

function apiMiddleware(): Connect.NextHandleFunction {
  return (req: IncomingMessage, res, next) => {
    const url = req.url ?? "";
    if (!url.startsWith("/api/v1/")) return next();
    const scenario = scenarioOf(undefined, req.headers.referer);

    const terminal = /^\/api\/v1\/agents\/([^/]+)\/terminal(\/input)?$/.exec(url);
    if (terminal?.[1] && req.method === "POST") {
      const session = terminal[1];
      readBody(req)
        .then((body) =>
          terminal[2] ? terminalInput(res, session, body) : terminalStream(req, res, session),
        )
        .catch(() => res.destroy());
      return;
    }

    if (req.method === "POST" || req.method === "PATCH") {
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
            const scenario = scenarioOf(req.url, undefined);
            res.setHeader("content-type", "text/html");
            res.end(html.replace("</head>", `${bootScript(snapshotOf(scenario))}</head>`));
          })
          .catch(next);
      });
    },
  };
}
