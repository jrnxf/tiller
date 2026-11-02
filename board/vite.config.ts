import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

import { tillerFixtures } from "./vite-fixtures.ts";

export default defineConfig({
  plugins: [
    // One bundle: route chunks would add a request to the first render, and the
    // board is served from the local daemon, so the size is not the cost.
    tanstackRouter({ target: "react", autoCodeSplitting: false }),
    react(),
    tillerFixtures(),
  ],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
