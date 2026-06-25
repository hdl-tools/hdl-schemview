import { defineConfig } from "vite";

// Tauri serves the built assets from ../dist; use relative base.
export default defineConfig({
  base: "./",
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true, target: "es2021" },
});
