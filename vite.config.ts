import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { configDefaults } from "vitest/config";
import pkg from "./package.json";

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  // Claude Code worktrees live under `.claude/`; without this, `vitest run` picks up
  // duplicate copies of `src/**/*.test.ts` and inflates counts.
  test: {
    exclude: [...configDefaults.exclude, "**/.claude/**"],
  },
});
