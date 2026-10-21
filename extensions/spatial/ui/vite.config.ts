import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { mapLibreWorkerPlugin } from "./build/maplibre-worker.mjs";
import { mapLibreLifecyclePlugin } from "./build/maplibre-lifecycle.mjs";

export default defineConfig({
  plugins: [react(), mapLibreWorkerPlugin(), mapLibreLifecyclePlugin()],
  test: { environment: "jsdom", include: ["tests/**/*.test.tsx", "tests/build-assets*.test.mjs", "tests/build-lifecycle.test.mjs"] },
  build: {
    lib: { entry: "src/index.tsx", formats: ["iife"], name: "__tabularis_plugin__", fileName: () => "index.js", cssFileName: "style" },
    rollupOptions: {
      external: ["react", "react/jsx-runtime", "@tabularis/plugin-api"],
      output: { globals: { react: "React", "react/jsx-runtime": "ReactJSXRuntime", "@tabularis/plugin-api": "__TABULARIS_API__" } },
    },
  },
});
