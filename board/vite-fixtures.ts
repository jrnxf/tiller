import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { resolve } from "node:path";

import type { Connect, Plugin } from "vite";

import { fixtureMutation } from "./src/lib/fixture-mutations.ts";
import { SCENARIOS, bootScript, fixtureSnapshot, serverResponses } from "./src/lib/fixtures.ts";
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
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
      res.write("retry: 1000\n\n");
      streams.add(res);
      const keepalive = setInterval(() => res.write(": keepalive\n\n"), 15_000);
      req.on("close", () => {
        clearInterval(keepalive);
        streams.delete(res);
      });
      return;
    }
    if (url === "/api/v1/snapshot") return sendJson(res, fixtureSnapshot(scenario));

    const responses = serverResponses(scenario) as Record<string, unknown>;
    if (url in responses) return sendJson(res, responses[url]);
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
