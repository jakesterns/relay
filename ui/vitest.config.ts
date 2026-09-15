import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const fake = fileURLToPath(new URL("./src/test/tauriMock.ts", import.meta.url));

/**
 * Component tests. Deliberately jsdom, not WebDriver: every interaction is a
 * DOM event dispatched inside this process, so nothing reaches the real
 * desktop's mouse or keyboard. See src/test/README.md.
 */
export default defineConfig({
  plugins: [react()],
  resolve: {
    // The only three doors out of the webview. Point all of them at the
    // in-process fake so no test can reach the real desktop even by mistake.
    alias: {
      "@tauri-apps/api/core": fake,
      "@tauri-apps/api/event": fake,
      "@tauri-apps/api/window": fake,
    },
  },
  test: {
    environment: "jsdom",
    // Without this, every .css id resolves to an empty string -- including
    // `shell.css?raw`, which keyboard.test.tsx reads to check the focus and
    // text-selection rules are still there.
    css: true,
    globals: true,
    setupFiles: ["src/test/setup.ts"],
    include: ["src/**/*.test.tsx", "src/**/*.test.ts"],
    restoreMocks: true,
    // Screens mount timers and poll; give teardown a chance rather than
    // letting a stray interval fail an unrelated file.
    clearMocks: true,
  },
});
