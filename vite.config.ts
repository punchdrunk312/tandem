import { defineConfig } from "vite";
import { resolve } from "node:path";

// Two pages: the island (index.html — Bob at the top of the screen) and the
// pairing/settings window (settings.html).
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "chrome110",
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        island: resolve(__dirname, "index.html"),
        settings: resolve(__dirname, "settings.html"),
      },
    },
  },
});
