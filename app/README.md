# hdl-schemview desktop app

The Tauri GUI (roadmap Phase 3): three linked views — **schematic**, **source**,
**waveform** — over the cross-probe engine. Click a box/wire in the schematic and
the source scrolls to its declaration while the waveform shows its signal; an
ambiguous source position offers a picker; double-click a box to expand it, or drill
into a leaf module to see its **internal logic** (process-level FF/comb boxes). The
waveform pane is interactive: stack many traces, set A/B markers, zoom/pan the shared
time window, switch per-signal radix, slice sub-buses, and read FSM **state names**.

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

## Command line

Launch straight into a design, EDA-tool style — the shell parses argv before the
window opens, prefills the load form, and auto-loads (identical to clicking **Load**):

```bash
hdl-schemview -f soc_top.f -top soc_top -I rtl/include -trace sim.fst -src-root .
```

| Flag | Meaning |
| --- | --- |
| `-f <filelist.f>` | designlist (`.f`) to elaborate — **required with `-top`** |
| `-top <name>` | top module name — **required with `-f`** |
| `-I <incdir>` | include directory (repeatable) |
| `-trace <path>` | waveform trace (VCD/FST) to load (optional) |
| `-src-root <dir>` | source root for the source view (optional, default `.`) |
| `-h`, `--help` | print usage and exit |

Long flags take either one dash (`-top`, the EDA convention) or two (`--top`).
Relative paths resolve against the **directory you launched from** (under
`npm run tauri dev`, `INIT_CWD` — not the `src-tauri` build dir). Exit codes:
**0** help, **1** a missing filelist/trace, **2** a usage error. With no arguments
the app opens the normal load form.

Elaborating a `.f` designlist (`-f`/`-top`, same as the in-app flow) shells out to
**`svxprobe-elaborate`**, which must be on `PATH`. It ships as a console script in
the `elaborate/` package's venv — launch from a shell that has it on `PATH`, e.g.
prepend `elaborate/.venv/Scripts` (Windows) or `elaborate/.venv/bin` (Unix), or
`uv run` inside `elaborate/`.

Under the dev server the extra `--` levels pass argv through Vite → Tauri → the app:

```bash
npm run tauri dev -- -- -- -f ../fixtures/picorv32_soc/picorv32_soc.f -top picorv32_soc
```

## Tests

- Frontend logic: `npm test` (vitest — the ELK adapter).
- Frontend build: `npm run build` (tsc + vite).
- Backend logic: `cargo test -p svxprobe-gui` (in `core/`).

## Windows notes

- **Toolchain:** Rust MSVC (`rustup default stable-msvc`) + the "Desktop
  development with C++" workload, plus the WebView2 runtime (preinstalled on
  Win 11).
- **`cargo` can't reach crates.io** with
  `CRYPT_E_NO_REVOCATION_CHECK (0x80092012)` (schannel can't check certificate
  revocation behind a corporate proxy/VPN): set `check-revoke = false` under
  `[http]` in `%USERPROFILE%\.cargo\config.toml`, or
  `setx CARGO_HTTP_CHECK_REVOKE false` then reopen the shell.
- **Icons:** the app ships a full icon set incl. `icons/icon.ico`, which Windows
  `tauri-build` embeds as a resource. To regenerate from a source image:
  `npm run tauri icon path/to/icon.png`.

## CI

The **App** workflow (`.github/workflows/app.yml`) builds the desktop app on a
matrix of **Ubuntu + Windows** for any PR/push that touches `app/**` or
`core/crates/**` (and on demand via *Run workflow*). The Windows leg exercises the
`tauri-build` Windows Resource/icon embed. **macOS is not CI-validated** — the
`.icns` is generated and the `cargo build` works locally, but no macOS runner is
in the matrix (it bills at 10× minutes on a private repo).

The fast PR gate (`ci.yml`) does **not** build the app (no webkit); the
`svxprobe-gui` logic it wraps is what that gate covers.

## Headless note

This app needs a display. Locally it boots under `xvfb-run` for smoke testing.
