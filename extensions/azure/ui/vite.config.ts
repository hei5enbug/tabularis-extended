import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: { environment: "jsdom", include: ["tests/**/*.test.tsx"] },
  build: {
    lib: { entry: "src/index.tsx", formats: ["iife"], name: "__tabularis_plugin__", fileName: () => "index.js", cssFileName: "style" },
    rollupOptions: {
      external: ["react", "react/jsx-runtime", "@tabularis/plugin-api"],
      output: { globals: { react: "React", "react/jsx-runtime": "ReactJSXRuntime", "@tabularis/plugin-api": "__TABULARIS_API__" } },
    },
  },
});
