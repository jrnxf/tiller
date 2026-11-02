import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./styles.css";
import type { Snapshot } from "./lib/api-types";
import { hydrateFromSnapshot, readBootSnapshot } from "./lib/boot";
import { fetchJson } from "./lib/queries";
import { createQueryClient } from "./lib/query-client";
import { createAppRouter } from "./lib/router";
import { SNAPSHOT_PATH, connectEvents } from "./lib/sse";

const root = document.getElementById("root");
if (!root) {
  throw new Error("missing #root element");
}

// The daemon inlines the snapshot, so this is synchronous. Without it (a dev
// server that is not the daemon) fetch one before the first render.
async function loadSnapshot(): Promise<Snapshot | null> {
  const inlined = readBootSnapshot();
  if (inlined) return inlined;
  try {
    return await fetchJson<Snapshot>(SNAPSHOT_PATH);
  } catch {
    return null;
  }
}

// The first paint has to use the real font, or the text re-flows when it
// arrives and the layout shifts. The font ships inside the binary, so this takes
// a few milliseconds; the cap keeps a font that cannot load from holding the
// board back.
async function fontReady(): Promise<void> {
  const cap = new Promise<void>((resolve) => setTimeout(resolve, 300));
  try {
    await Promise.race([
      Promise.all([document.fonts.load('13px "Geist"'), document.fonts.load('13px "Geist Mono"')]),
      cap,
    ]);
  } catch {
    // The fallback font is fine.
  }
}

const queryClient = createQueryClient();
const snapshot = await loadSnapshot();
const lastEventSeq = snapshot ? hydrateFromSnapshot(queryClient, snapshot) : null;

const router = createAppRouter(queryClient);
// Load the first route before rendering, so the first paint is the finished
// screen and not a frame of nothing.
await Promise.all([router.load(), fontReady()]);

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);

connectEvents({ queryClient, lastEventSeq });
