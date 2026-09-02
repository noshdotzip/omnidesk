import { defineConfig } from "vitest/config";

// Only the pure, platform-independent logic is unit-tested here (projection state
// mirror + coordinate mapping mirror). Electron main/renderer runtime code needs a
// real Electron process and is covered by the manual test plan in docs/testing.md.
export default defineConfig({
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
