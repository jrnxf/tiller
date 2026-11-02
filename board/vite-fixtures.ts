import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { resolve } from "node:path";

import type { Connect, Plugin } from "vite";

import type { BoardRow } from "./src/lib/api-types.ts";
import { fixtureMutation } from "./src/lib/fixture-mutations.ts";
import { SCENARIOS, bootScript, fixtureResource, fixtureSnapshot } from "./src/lib/fixtures.ts";
import type { Scenario } from "./src/lib/fixtures.ts";

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

    if (req.method === "POST" || req.method === "PATCH") {
      readBody(req)
        .then((body) => {
          const { reply, followUps } = fixtureMutation(req.method ?? "POST", url, body);
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

    if (url === "/api/v1/events") {
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
    if (url === "/api/v1/snapshot") return sendJson(res, fixtureSnapshot(scenario));

    const resource = fixtureResource(url, scenario);
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
        return html.replace("</head>", `${bootScript(fixtureSnapshot(scenario))}</head>`);
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
            res.end(html.replace("</head>", `${bootScript(fixtureSnapshot(scenario))}</head>`));
          })
          .catch(next);
      });
    },
  };
}
