import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri-tuned Vite config: relative base so the bundled HTML loads from
// tauri://localhost, fixed dev port, and src-tauri ignored by the watcher.
export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2020",
    outDir: "dist",
  },
});
