# Building and installing from source

For **running hdl-schemview on your own machine** when the published artifacts do not fit
it. If you are here to *contribute*, you want [CONTRIBUTING.md](../CONTRIBUTING.md) (the PR
gate block) and [development.md](development.md) (command index, toolchain pins) instead —
this doc is about getting a working install, not a working dev loop.

## When you need this

| Symptom | What it means |
| --- | --- |
| `libc.so.6: version 'GLIBC_2.39' not found` (or any `GLIBC_2.x`) | The release was linked against a newer glibc than your host has. glibc is backward- but not forward-compatible, so the loader refuses it outright. Released Linux bundles target **glibc ≥ 2.35** (Ubuntu 22.04, Debian 12, RHEL 9) — see [ADR 0013](decisions/0013-glibc-floor-of-linux-artifacts.md). Below that floor, build locally. |
| Your distro is older than the floor, or not Debian/Fedora-shaped | The `.deb`/`.rpm` declare WebKitGTK by package name; the AppImage needs FUSE. Building locally sidesteps both. |
| No artifact for your platform | Releases cover Windows, Linux x86-64 and macOS. Anything else — Linux aarch64, BSD — is source-only. |
| The designlist (`.f`) flow reports the harness is missing | Elaboration is deliberately not bundled (#277, [ADR 0009](decisions/0009-packaging-for-isolated-environments.md)). See [Getting the harness](#getting-the-harness). |

Check your own glibc first — this is the number that matters:

```bash
ldd --version | head -1
```

And check what a downloaded artifact actually demands, before concluding anything:

```bash
dpkg-deb -R hdl-schemview_*.deb /tmp/x          # or: ./hdl-schemview_*.AppImage --appimage-extract
objdump -T /tmp/x/usr/bin/hdl-schemview-app | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1
```

## Which path

| Path | Gets you | System packages needed | First build |
| --- | --- | --- | --- |
| **[A — Nix](#path-a--nix-linux)** | The desktop app, harness included | none | long (~30 min; slang compiles from source) |
| **[B — native](#path-b--native-toolchain)** | The desktop app + an installable `.deb`/`.rpm`/AppImage | WebKitGTK + GTK dev packages | ~10 min |
| **[C — CLI only](#path-c--cli-only)** | `svxprobe` (match, probe, headless) | none | ~3 min |

Path A is the least setup and the most reproducible; path B is what you want if you need an
**installed** app (desktop entry, icons, `dpkg`-managed) or a bundle to hand to someone else.

---

## Path A — Nix (Linux)

Requires Nix with flakes. No system libraries — the closure carries its own glibc and
WebKitGTK, so your host's glibc is never consulted and the floor question disappears.

```bash
nix build .#hdl-schemview-app     # -> ./result/bin/hdl-schemview
./result/bin/hdl-schemview
```

The harness is baked into the wrapper via `SVXPROBE_ELABORATE`, so designlist (`.f`) loading
works with nothing else installed. The trade is closure size and build time: `nix/pyslang.nix`
builds slang from the PyPI sdist with no binary cache — **~21 min on 8 cores**, once.

This output is **best-effort, not a release artifact** ([ADR 0012](decisions/0012-nix-outputs-are-a-build-channel.md)).
On a non-NixOS host it also needs [nixGL](https://github.com/nix-community/nixGL) for the GL
stack — details and the exact invocation in [`app/README.md` §Running from Nix](../app/README.md#running-from-nix-linux).

Also available: `nix build .#svxprobe` (CLI, any system) and `nix develop` (full dev shell).

---

## Path B — native toolchain

### Prerequisites

**Rust** — `core/rust-toolchain.toml` pins 1.94, and `app/src-tauri/` has no toolchain file,
so it uses your rustup default. Install both:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
cd core && rustup toolchain install     # reads the 1.94 pin
```

A distro-packaged `rustc` is usually too old and cannot honour the pin — rustup is the
supported route.

**Node 20+** — `app/package-lock.json` is committed, so use `npm ci`, not `npm install`.

**Linux system libraries** — this exact set is what CI installs and what `app.yml` proves:

```bash
sudo apt-get install -y pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libsoup-3.0-dev librsvg2-dev libayatana-appindicator3-dev
```

On non-Debian distros the equivalents are the WebKitGTK 4.1, GTK 3, libsoup 3, librsvg and
libayatana-appindicator **development** packages; the authoritative per-distro list is
[Tauri's prerequisites page](https://tauri.app/start/prerequisites/). Only the Debian/Ubuntu
set above is exercised by this project's CI.

### Build

```bash
cd app
npm ci
npm run tauri build                              # all bundle formats for your OS
npm run tauri build -- --bundles deb             # or just the one you want
```

Bundles land in `app/src-tauri/target/release/bundle/{deb,rpm,appimage}/`, and the bare
binary in `app/src-tauri/target/release/`.

> `npm run tauri build` passes Tauri's `custom-protocol` feature for you. That flag is what
> makes the binary serve its embedded frontend instead of `devUrl`; a build that omits it
> launches a window rendering `Could not connect to localhost: Connection refused`.

### Install

```bash
sudo apt install ./app/src-tauri/target/release/bundle/deb/hdl-schemview_*_amd64.deb
```

`apt install ./file.deb` (rather than `dpkg -i`) resolves the declared WebKitGTK/GTK
dependencies. For the AppImage, make it executable and run it — it needs FUSE 2
(`libfuse2` on Ubuntu 22.04, `libfuse2t64` on 24.04 and later).

A binary you built inherits **your** glibc, so it runs on this machine by construction. It
is not portable to anything older — see [Verify what you built](#verify-what-you-built).

---

## Path C — CLI only

The Rust core links no system libraries at all — no WebKitGTK, no pkg-config:

```bash
cd core
cargo build --release --bin svxprobe      # -> core/target/release/svxprobe
```

`core/` is a virtual workspace with several binaries, so `--bin svxprobe` is required; a bare
`cargo run` there cannot pick one. This gives you `match`, `probe` and the headless paths —
everything except the windowed views.

---

## Getting the harness

Elaboration (`.f` → `hierarchy.json`) runs through `svxprobe-elaborate`, a Python/pyslang
tool the app looks up via `SVXPROBE_ELABORATE`, then `PATH`. Path A bakes it in. Otherwise:

```bash
cd elaborate && uv sync                   # uv is the supported non-Nix path
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o hierarchy.json
```

Full flag table: [`elaborate/README.md`](../elaborate/README.md). Without it, the app still
opens an already-elaborated `hierarchy.json` — the **Model JSON** load mode.

---

## Verify what you built

```bash
BIN=app/src-tauri/target/release/hdl-schemview        # or .../hdl-schemview-app

ldd "$BIN" | grep 'not found'                          # expect no output
objdump -T "$BIN" | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1
"$BIN" --bench --bases golden --out /tmp/bench.md      # headless, no display needed
```

The `objdump` line is the glibc floor of *your* binary — the same check `app.yml`'s
`glibc floor` step runs against release builds. Then open the committed fixture and confirm
a click in one pane moves the others:

```bash
cd app/src-tauri && ./target/release/hdl-schemview \
  -f ../../fixtures/picorv32_soc/picorv32_soc.f -top picorv32_soc \
  -trace ../../fixtures/picorv32_soc/traces/picorv32_soc.fst -src-root ../..
```

## Troubleshooting

| Error | Cause and fix |
| --- | --- |
| `version 'GLIBC_2.x' not found` | A binary built on a newer host than yours. Build locally — that is what this doc is for. Nothing can patch it after the fact. |
| `pkg-config: command not found`, or `webkit2gtk-4.1` not found | Missing `-dev` packages. The runtime `.so`s alone are not enough to build; install the set under [Prerequisites](#prerequisites). |
| `Could not create default EGL display: EGL_BAD_PARAMETER`, blank window | Nix build on a non-NixOS host: the closure's GL stack does not match your driver. Use nixGL — [`app/README.md` §Running from Nix](../app/README.md#running-from-nix-linux). A **native** build does not hit this; it uses your distro's WebKitGTK. |
| `Could not connect to localhost: Connection refused` in the window | The binary was built without Tauri's `custom-protocol` feature, so it wants a dev server. `npm run tauri build` passes it; a hand-rolled `cargo build` must too. |
| AppImage will not start | FUSE 2 missing — `libfuse2` (Ubuntu ≤ 22.04) or `libfuse2t64` (24.04+). |
| Designlist load reports the harness is missing | See [Getting the harness](#getting-the-harness). Set `SVXPROBE_ELABORATE` to its absolute path if it is installed but not on `PATH`. |
| `error: rustc 1.x is not supported` | Distro rustc predates the pin. Install rustup and re-run `rustup toolchain install` from `core/`. |

## See also

- [`app/README.md`](../app/README.md) — running the app, launch flags, bundling, offline install
- [`development.md`](development.md) — command index, toolchain pins, what each CI workflow runs
- [`releasing.md`](releasing.md) — cutting a tagged release and verifying its artifacts
- [ADR 0009](decisions/0009-packaging-for-isolated-environments.md) — packaging tiers for isolated machines
- [ADR 0013](decisions/0013-glibc-floor-of-linux-artifacts.md) — why the Linux runner is pinned, and the 2.35 floor
