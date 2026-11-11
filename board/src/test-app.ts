import type { QueryClient } from "@tanstack/react-query";

import { createQueryClient } from "./lib/query-client";
import { createAppRouter } from "./lib/router";

// The board's query client for a test: the same options and the same one
// retry, without the real second of backoff before it. A test of a failed
// read would otherwise wait that second out, several on a loaded machine,
// against its 5 s timeout.
export function createTestQueryClient(): QueryClient {
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  return queryClient;
}

// Loads every route's split chunk, so a test file that renders the whole app
// pays for transforming and compiling them once, before its first test runs.
// Left to the first test, that work (about a second alone, several on a
// loaded machine) counts against the test's 5 s timeout. Call it with a
// top-level await: the file's import phase has no timeout.
export async function loadRouteChunks() {
  const router = createAppRouter(createQueryClient());
  await Promise.all(Object.values(router.routesById).map((route) => router.loadRouteChunk(route)));
}

// Loads the ask editor (tiptap), which the ask dialog reaches through a
// dynamic import, for the same reason: the first dialog in a file would
// otherwise wait on its transform inside the test.
export async function loadAskEditor() {
  await import("./components/shell/ask-editor");
}
