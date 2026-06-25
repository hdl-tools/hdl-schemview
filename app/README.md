# hdl-schemview desktop app

The Tauri GUI (roadmap Phase 3): three linked views — **schematic**, **source**,
**waveform** — over the cross-probe engine. Click a box/wire in the schematic and
the source scrolls to its declaration while the waveform shows its signal; an
ambiguous source position offers a picker; double-click a box to expand it.

**Standalone:** Tauri renders the frontend in the OS-native webview
(WebKitGTK / WebView2 / WKWebView) — **no Chromium or Playwright at runtime**.

## Architecture

```
app/
  src/                 frontend (TypeScript + Vite)
    elk.ts             SchematicGraph -> ELK graph (elkjs layout)   [unit-tested]
    api.ts             typed wrappers over the Tauri commands
    main.ts            the three panes + one selection store
  src-tauri/           thin Tauri shell: commands forward to svxprobe-gui
core/crates/gui        all session logic (no UI toolkit) — CI-tested
```

The brain is **`svxprobe-gui`** in the core workspace (load, schematic, probe,
signal values, source) — fully tested without a window. `src-tauri` only locks the
session and forwards calls.

## Develop / run

Prereqs: Rust, Node 20+, and on Linux the Tauri deps
(`libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev
libayatana-appindicator3-dev`).

```bash
cd app
npm install
npm run tauri dev        # launches the desktop window
# or build a bundle:
npm run tauri build
```

In the window, the model/trace/source-root fields are prefilled for the bundled
fixture; click **Load**. (Paths are resolved relative to the app's working
directory — use absolute paths if needed.)

## Tests

- Frontend logic: `npm test` (vitest — the ELK adapter).
- Frontend build: `npm run build` (tsc + vite).
- Backend logic: `cargo test -p svxprobe-gui` (in `core/`).

## Headless note

This app needs a display. The core workspace CI does **not** build it (no webkit);
the `svxprobe-gui` logic it wraps is what CI covers. A nightly job builds the app
against webkit. Locally it boots under `xvfb-run` for smoke testing.
