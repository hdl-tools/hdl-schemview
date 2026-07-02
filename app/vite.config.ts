import { defineConfig } from "vite";

// Tauri serves the built assets from ../dist; use relative base.
export default defineConfig({
  base: "./",
  clearScreen: false,
  // Don't watch src-tauri: Tauri has its own Rust watcher, and chokidar hitting
  // the linker-locked .exe in target/ crashes Vite with EBUSY on Windows.
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { outDir: "dist", emptyOutDir: true, target: "es2021" },
});
