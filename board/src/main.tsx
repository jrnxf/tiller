import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./styles.css";
import { SetupScreen } from "./components/states/setup-screen";
import { hydrateFromSnapshot, loadBoot } from "./lib/boot";
import { createQueryClient } from "./lib/query-client";
import { createAppRouter } from "./lib/router";
import { connectEvents } from "./lib/sse";

const root = document.getElementById("root");
if (!root) {
  throw new Error("missing #root element");
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

// The daemon inlines the snapshot, so this is synchronous. Without it (a dev
// server that is not the daemon, or a machine nobody has set up) it asks.
const [boot] = await Promise.all([loadBoot(), fontReady()]);

if (boot.kind === "setup") {
  // Once `tiller init` has run, the daemon inlines a snapshot: a reload is the
  // ordinary first load.
  createRoot(root).render(
    <StrictMode>
      <SetupScreen command={boot.command} onReady={() => window.location.reload()} />
    </StrictMode>,
  );
} else {
  const queryClient = createQueryClient();
  const lastEventSeq = boot.snapshot ? hydrateFromSnapshot(queryClient, boot.snapshot) : null;

  const router = createAppRouter(queryClient);
  // Load the first route before rendering, so the first paint is the finished
  // screen and not a frame of nothing.
  await router.load();

  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  );

  connectEvents({ queryClient, lastEventSeq });
}
