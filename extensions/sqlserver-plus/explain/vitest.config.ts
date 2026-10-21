import { defineConfig } from "vitest/config";

// Keeps this package's tests independent of the monorepo root vitest.config.ts.
export default defineConfig({
  test: {
    root: import.meta.dirname,
    include: ["tests/**/*.test.ts"],
  },
});
