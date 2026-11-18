import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import type { ProxyOptions } from "vite";
import { defineConfig } from "vitest/config";

import { tillerFixtures } from "./vite-fixtures.ts";

// `TILLER_DAEMON=http://127.0.0.1:7421 bun run dev` serves the board from
// source against a running daemon instead of the fixtures, with hot reload
// (scripts/live.sh runs it per worktree).
//
// The daemon checks Host and Origin. On this machine (a loopback page) the
// proxy speaks as the daemon's own loopback origin, so the daemon treats the
// page as its own board. A `<port>.<TILLER_LIVE_DOMAIN>` page, reached through
// a TLS-terminating proxy, keeps the browser's Host, Origin and
// X-Forwarded-For: the daemon must list that name with `--allow-host`, and
// then gives it what it gives any allowed name (a Secure cookie, per-browser
// sign-in limits, nothing loopback-only).
const daemon = process.env.TILLER_DAEMON ? new URL(process.env.TILLER_DAEMON) : undefined;
// The daemon redirects `localhost` to `127.0.0.1`, which would leave the page.
if (daemon?.hostname === "localhost") daemon.hostname = "127.0.0.1";
const liveDomain = process.env.TILLER_LIVE_DOMAIN;

function atLiveDomain(host: string | undefined): boolean {
  const name = host?.replace(/:\d+$/, "").toLowerCase();
  return Boolean(liveDomain && name?.endsWith(`.${liveDomain.toLowerCase()}`));
}

const live: ProxyOptions | undefined = daemon
  ? {
      target: daemon.origin,
      ws: true,
      configure: (proxy) => {
        const asDaemon = (
          out: { setHeader(name: string, value: string): void; getHeader(name: string): unknown },
          host: string | undefined,
        ) => {
          if (atLiveDomain(host)) return;
          out.setHeader("host", daemon.host);
          if (out.getHeader("origin") !== undefined) out.setHeader("origin", daemon.origin);
        };
        proxy.on("proxyReq", (out, req) => asDaemon(out, req.headers.host));
        proxy.on("proxyReqWs", (out, req) => asDaemon(out, req.headers.host));
      },
    }
  : undefined;

export default defineConfig({
  plugins: [
    // One chunk per route. The router loads the first route's chunk before it
    // renders (main.tsx awaits router.load), and defaultPendingMs keeps the
    // previous screen up while the next one loads, so a split shows no pending
    // state and moves no layout. It keeps the main chunk under 500 kB.
    tanstackRouter({ target: "react", autoCodeSplitting: true }),
    react(),
    ...(live ? [] : [tillerFixtures()]),
  ],
  server: live
    ? {
        proxy: { "/api": live, "/login": live },
        allowedHosts: liveDomain ? [`.${liveDomain}`] : undefined,
      }
    : undefined,
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    testTimeout: 15000,
  },
});
