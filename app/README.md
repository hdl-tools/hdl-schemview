# hdl-schemview desktop app

The Tauri GUI (roadmap Phases 3 + 3e): the linked views — **schematic**, **source**,
**waveform** — over the cross-probe engine. Click a box/wire in the schematic and
the source scrolls to its declaration while the waveform shows its signal; an
ambiguous source position offers a picker; double-click a box to expand it, or drill
into a leaf module to see its **internal logic** (process-level by default,
**gate-level** behind a Settings toggle). The waveform pane is interactive: stack many
traces in collapsible groups, set A/B markers, zoom/pan the shared time window, switch
per-signal radix, slice sub-buses, and read FSM **state names**.

**Layout.** A hierarchy tree on the left (draggable column splitter), a tabbed content
area top-right — **Source** · **C/C++** · **Schematic** · **Settings** — and a tabbed
bottom pane — **Status** · **Waveform** — separated by a draggable row splitter. Source
and Status are the defaults; Schematic, Waveform and C/C++ are revealed on demand.
Schematic and waveform tabs can be **popped out** into their own windows (`⇱`), each
independent: its own scope, its own trace, its own signal picker.

**Standalone:** Tauri renders the frontend in the OS-native webview
(WebKitGTK / WebView2 / WKWebView) — **no Chromium or Playwright at runtime**.

## Architecture

```
app/
  src/                 frontend (TypeScript + Vite) — 15 modules, 12 test files
    main.ts            all panes, app state, and the only DOM outside tree.ts
    api.ts             typed wrappers over the 18 Tauri commands
    types.ts           DTO interfaces mirroring the Rust serde types
    bus.ts             the single cross-pane selection channel     [unit-tested]
    elk.ts             SchematicGraph -> ELK graph (elkjs layout)  [unit-tested]
    tree.ts            hierarchy-tree factory (window tree + pickers) [happy-dom]
    wave.ts            waveform geometry, groups, canvas drawing   [unit-tested]
    syntax.ts          SV + C/C++ lexical tokenizer (ADR 0008)     [unit-tested]
    names.ts           model-driven semantic name overlay (ADR 0007) [unit-tested]
    source.ts          source-pane rendering + line highlighting
    srcoffset.ts       caret -> byte offset across token spans     [happy-dom]
    csrc.ts            routes a SourceLoc to the RTL or C/C++ pane [unit-tested]
    schempick.ts       schematic signal-palette logic              [unit-tested]
    prefs.ts           settings persistence (theme, excluded, toggles) [unit-tested]
    log.ts             status/log pane formatting                  [unit-tested]
  src-tauri/           thin Tauri shell: commands forward to svxprobe-gui
core/crates/gui        all session logic (no UI toolkit) — CI-tested
```

The brain is **`svxprobe-gui`** in the core workspace (load, schematic, probe,
signal values, source) — fully tested without a window. `src-tauri` only locks the
session map and forwards calls: **18 commands**, each taking an optional `session_id`
so a popped-out waveform window can load and query its own trace of the same design.
The pure modules above are DOM-free on purpose, which is why most of the frontend is
unit-testable without a browser environment.

## Interacting

| Action | Where |
| --- | --- |
| Drive the schematic to a scope | single-click a tree row |
| Reveal a node in source | double-click a tree row |
| Move the source highlight | left-click a line in the source pane |
| Cross-probe menu (source ▸ / waveform ▸) | right-click a source token, schematic box/pin/wire |
| Expand an instance / drill internal logic | double-click a schematic box |
| **Search signals in the current scope** | **`a`** over the schematic (`Esc` closes) |
| **Signal picker for a waveform pane** | **Ctrl/⌘+B**, or ☰ Signals |
| Zoom the schematic to fit | Ctrl/⌘+0 |
| Pop a pane into its own window | `⇱` in the tab's control strip |
| Swap the trace of one pane | **Load trace…** in that pane's control strip |
| Regroup / reorder waveform lanes | drag a lane by its name cell, or its name-cell menu |
| Rename / collapse / delete a lane group | double-click or right-click the group header |
| Set markers A / B | left-click / right-click a waveform track |

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

In the window, the load bar offers two modes:

| Mode | Fields |
| --- | --- |
| **Model JSON** | `model` (an already-elaborated `hierarchy.json`) |
| **Designlist** | `filelist` (`.f`), `top`, `incdir` (`;`-separated), and `hlssrc` — *C/C++ sources or dirs*, `;`-separated, for an HLS design |

Both then take `trace` and `srcroot`. The model/trace/source-root fields are prefilled
for the bundled fixture; click **Load**. Paths resolve relative to the app's working
directory — use absolute paths if unsure. **`srcroot` must be the directory the model
was elaborated from**, since the source view resolves `srcroot` + each file's recorded
relative path.

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
| `-trace <path>` | waveform trace (VCD/FST/GHW) to load (optional) |
| `-src-root <dir>` | source root for the source view (optional, default `.`) |
| `-h`, `--help` | print usage and exit |

Long flags take either one dash (`-top`, the EDA convention) or two (`--top`).
Relative paths resolve against the **directory you launched from** (under
`npm run tauri dev`, `INIT_CWD` — not the `src-tauri` build dir). Exit codes:
**0** help, **1** a missing filelist/trace, **2** a usage error. With no arguments
the app opens the normal load form.

There is **no `-hls-src` startup flag**: a design launched from the command line cannot
declare its C/C++ sources, so its C pane will only find sources the provenance comments
resolve to on their own. Use the in-app designlist form's *C/C++ sources* field for an
HLS design.

Elaborating a `.f` designlist (`-f`/`-top`, same as the in-app flow) shells out to
**`svxprobe-elaborate`**, which must be on `PATH`. It ships as a console script in
the `elaborate/` package's venv — launch from a shell that has it on `PATH`, e.g.
prepend `elaborate/.venv/Scripts` (Windows) or `elaborate/.venv/bin` (Unix), or
`uv run` inside `elaborate/`.

The shell **always** passes `--gate-level` and `--name-refs` (plus `--hls-map` and one
`--hls-src` per declared entry when the C/C++ field is non-empty). This is a hard
compatibility requirement on the venv's harness version: those flags are additive, but
the frontend switches on data that must already be in the model — the gate-level toggle
reads gate primitives, and semantic coloring plus usage-click resolution read
`name_refs`. Against an older harness that doesn't accept the flags, elaboration fails
outright; against one that accepts them, a designlist design behaves exactly like the
committed golden.

Under the dev server the extra `--` levels pass argv through Vite → Tauri → the app:

```bash
npm run tauri dev -- -- -- -f ../fixtures/picorv32_soc/picorv32_soc.f -top picorv32_soc
```

## Tests

- Frontend logic: `npm test` (vitest — 12 suites covering the ELK adapter, the
  selection bus, waveform geometry and grouping, the tokenizer and name overlay, the
  tree factory, source offsets, prefs, log and palette helpers).
- Frontend build: `npm run build` (tsc + vite). TS is strict, with
  `noUnusedLocals`/`noUnusedParameters`; there is no ESLint or Prettier, so match the
  surrounding style by hand.
- Backend logic: `cargo test -p svxprobe-gui` (in `core/`).

The default Vitest environment is **`node`** (DOM-free and faster). The two suites that
need a DOM — `tree.test.ts` and `srcoffset.test.ts` — opt into happy-dom with a per-file
`// @vitest-environment` docblock rather than switching it on globally.

## Windows notes

- **Toolchain (to *build*):** Rust MSVC (`rustup default stable-msvc`) + the
  "Desktop development with C++" workload, plus the WebView2 runtime
  (preinstalled on Win 11).
- **To *run* a bundle:** nothing, if it was built with
  `--config tauri.offline.conf.json` — that vendors WebView2 into the install
  directory. A default bundle instead downloads WebView2 at install time and so
  needs network. See *Distribution / offline install*.
- **No console output from a release build?** That was true before #240: the
  release binary is GUI-subsystem, so Windows gave it no std handles and `-h`
  printed nothing. It now attaches to the parent console when launched from a
  terminal. Because the process is still GUI-subsystem, `cmd` returns to the
  prompt immediately, so output can interleave with the next prompt — expected.
- **`cargo` can't reach crates.io** with
  `CRYPT_E_NO_REVOCATION_CHECK (0x80092012)` (schannel can't check certificate
  revocation behind a corporate proxy/VPN): set `check-revoke = false` under
  `[http]` in `%USERPROFILE%\.cargo\config.toml`, or
  `setx CARGO_HTTP_CHECK_REVOKE false` then reopen the shell.
- **Icons:** the app ships a full icon set incl. `icons/icon.ico`, which Windows
  `tauri-build` embeds as a resource. To regenerate from a source image:
  `npm run tauri icon path/to/icon.png`.

## Distribution / offline install

The deployment target is an **isolated machine: no network, no toolchain, no
package manager** (#240, ADR 0009). An engineer copies one artifact across and
runs it. Elaboration is *not* part of that — see "Getting a design in" below.

Built artifacts come from a **tagged release** (#248): pushing a `v*` tag publishes
a draft GitHub Release carrying these bundles plus a `SHA256SUMS` file, so a
hand-carried installer can be integrity-checked on arrival. **`docs/releasing.md`**
is the runbook — including the caveat that a private repo's release assets reach
collaborators only.

| OS | Artifact | Webview runtime |
| --- | --- | --- |
| Windows | NSIS installer, built with `--config tauri.offline.conf.json` | **Vendored.** `webviewInstallMode: fixedRuntime` puts a pinned WebView2 inside the install directory — no network, no admin rights, no dependency on what the machine already has (the pinned 150.0.4078.99 cab is ~284 MB, more once expanded). |
| Linux | **AppImage** | Bundled inside the AppImage. |
| macOS | `.app` / `.dmg` | WKWebView is part of the OS. |

The Linux leg also builds a `.deb` (Debian/Ubuntu) and an `.rpm` (Fedora/RHEL,
#260). Both declare WebKitGTK as a system dependency, so they need a package
manager with repo access — **use the AppImage** on an isolated box. CI installs
the `.rpm` in a clean Fedora container and runs it headlessly, which is what
proves its declared dependency names are real; the `.deb` is not yet covered
(#261).

The default config deliberately carries **no** WebView2 payload path, so an
ordinary `npm run tauri build` still works for contributors. The offline Windows
bundle is an overlay:

```bash
# One-time: download the "Fixed Version" runtime .cab from
#   https://developer.microsoft.com/microsoft-edge/webview2/
# and expand it to app/src-tauri/webview2-runtime/ (gitignored; the cab is
#   ~284 MB for the pinned 150.0.4078.99, more once expanded). On Windows use
#   expand.exe by full path — in Git Bash a bare `expand` is GNU coreutils'
#   tabs-to-spaces filter, not the cab extractor.
# --config is relative to the CWD (app/), not to src-tauri/ — hence the prefix.
npm run tauri build -- --config src-tauri/tauri.offline.conf.json --bundles nsis
```

In CI that URL is the `WEBVIEW2_CAB_URL` repository variable — pinned on
purpose, since the runtime version is part of what the bundle ships.

**macOS is signed ad-hoc** (`signingIdentity: "-"`), which is *not* enough to
clear Gatekeeper on a downloaded app. After copying, run:

```bash
xattr -dr com.apple.quarantine /Applications/hdl-schemview.app
```

> ⚠️ macOS is **unverified end-to-end** — CI proves it builds and bundles, but
> no maintainer has a macOS machine, so Gatekeeper behaviour and signing/
> notarization remain an open workstream.

### Getting a design in

Elaboration needs Python + pyslang (`svxprobe-elaborate` on PATH) and is **not
bundled** — freezing it into a sidecar is tier 2 of #240, gated on a PyInstaller
spike. On a machine without it, the designlist (`.f`) flow reports so explicitly
and points here. The supported workflow is:

1. Elaborate on a connected machine: `svxprobe-elaborate --top <top> -f <list.f>
   --gate-level --name-refs -o hierarchy.json`
2. Copy `hierarchy.json` (and any trace) to the isolated machine.
3. Open it with **Load model**.

This is already how the #22/#155 benchmark's real-design basis was produced.

## CI

The **App** workflow (`.github/workflows/app.yml`) has three jobs:

- **`build`** — the fast signal on every PR/push touching `app/**` or
  `core/crates/**`: `npm test`, `npm run build`, then `cargo build` on
  **Ubuntu + Windows**. The Windows leg exercises the `tauri-build` Windows
  Resource/icon embed.
- **`bundle`** — the packaging path, on **push to `main`, a `v*` tag, and manual
  dispatch**, across **Ubuntu + Windows + macOS**. It runs a real `tauri build`
  (which `cargo build` never exercises, so NSIS/AppImage/`.app` breakage would
  otherwise surface at release time) and uploads each bundle as an artifact.
  It skips pull requests because macOS bills at 10× minutes on a private repo.
- **`release`** — on a **`v*` tag only** (#248): downloads the bundle artifacts,
  stages the installers flat, writes `SHA256SUMS`, and opens a **draft** GitHub
  Release for a human to check and publish. See **`docs/releasing.md`**.

`release` is `needs: [build, bundle]`, so **one failed OS leg publishes nothing**
rather than a partial release. `docs/releasing.md` §Prerequisites lists what to
confirm before pushing a tag (`WEBVIEW2_CAB_URL`, Actions quota).

The fast PR gate (`ci.yml`) does **not** build the app (no webkit); the
`svxprobe-gui` logic it wraps is what that gate covers.

## Headless note

This app needs a display. Locally it boots under `xvfb-run` for smoke testing.
