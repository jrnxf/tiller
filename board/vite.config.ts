import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

import { tillerFixtures } from "./vite-fixtures.ts";

export default defineConfig({
  plugins: [
    // One chunk per route. The router loads the first route's chunk before it
    // renders (main.tsx awaits router.load), and defaultPendingMs keeps the
    // previous screen up while the next one loads, so a split shows no pending
    // state and moves no layout. It keeps the main chunk under 500 kB.
    tanstackRouter({ target: "react", autoCodeSplitting: true }),
    react(),
    tillerFixtures(),
  ],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
