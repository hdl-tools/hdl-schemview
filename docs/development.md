# Development & CI

Where each command lives, and what CI actually runs.

This file is an **index plus the CI account** — it deliberately does not restate command
blocks that are canonical elsewhere, so there is one copy of each to keep correct.

## Where the commands live

| You want to… | Canonical source |
| --- | --- |
| Run the PR gates before pushing | [`CONTRIBUTING.md`](../CONTRIBUTING.md) — the full block: Rust fmt/clippy/test, the matcher on **both** trace formats, frontend, Python, the RTL `always_ff` lint, golden reproducibility |
| Set up a dev environment | [`README.md`](../README.md) — Nix flake or per-language tooling |
| Elaborate a design / use a harness flag | [`elaborate/README.md`](../elaborate/README.md) — the full flag table (`--gate-level`, `--name-refs`, `--hls-map`, `--hls-comment-re`, `--hls-src`) and worked invocations |
| Run or debug the desktop app | [`app/README.md`](../app/README.md) — `npm run tauri dev`, the launch-arg flags, tests, Windows notes, bundling |
| Run the scalability benchmark | [`benchmarking.md`](benchmarking.md) — one-command `collect`, the packaged app's `--bench`, and the measured findings |
| Regenerate or reason about fixtures | [`fixtures.md`](fixtures.md) — the two-tier policy, regen commands, pinned tool versions |
| Cut a release | [`releasing.md`](releasing.md) — version bump, tag, draft release, artifact verification |

Quick reference for the two most-used loops:

```bash
# Rust (from core/) — the three PR gates
cargo test --all
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

# Frontend (from app/)
npm test            # Vitest
npm run build       # tsc && vite build
npm run tauri dev   # Tauri window + Vite HMR
```

## Toolchain pins

| Tool | Pin | Where |
| --- | --- | --- |
| Rust | 1.94 | `core/rust-toolchain.toml` — the Nix flake reads this file at eval time, so `nix develop`'s rustc cannot drift from the rustup pin |
| nixpkgs | committed `flake.lock` | Without it the "pinned Verilator" claim would be false |
| pyslang / Python / Verilator | see [fixtures.md](fixtures.md) | |

The Python harness **is** packaged by Nix (#280): `packages.svxprobe-elaborate`, built
from source including `pyslang`, with `nix/pyslang.nix` carrying the derivation because
`pyslang` is absent from nixpkgs (the `slang` there is jedsoft's S-Lang, unrelated). So
`nix develop` is self-contained — no `uv sync`, no PyPI fetch at run time — and
`svxprobe-elaborate` is on `PATH`, which is what `Session::elaborate_and_load` resolves.

The build is hermetic without patching. slang declares each dependency as
`FetchContent_Declare(... FIND_PACKAGE_ARGS <version>)`, and `FIND_PACKAGE_ARGS`
(CMake >= 3.24) runs `find_package` **first**, so the git clone the sandbox forbids is
only a fallback. Supplying `fmt >= 12.1` and `pybind11 >= 3.0` is the entire mechanism —
which is why the flake tracks `nixos-25.11`, the first channel carrying both.

It is **not** in `checks`, on purpose: the derivation builds slang from source (21m22s
measured on 8 cores) and there is no binary cache, so gating it per-PR would dominate
`nix flake check`. `nightly.yml`'s `nix-harness` job builds it instead and asserts the
packaged harness still reproduces the committed golden byte-for-byte.

`uv` left the Nix dev shell, but **only there**. It remains the supported path for
contributors without Nix, and `ci.yml`/`nightly.yml` still drive the harness through it;
`elaborate/uv.lock` stays the pin of record. Inside `nix develop` the interpreter also
carries the harness's dependencies and `PYTHONPATH` points at `elaborate/`, so
`python -m svxprobe_elaborate.<mod>` runs your **working tree**, while the
`svxprobe-elaborate` on `PATH` is the fixed store build.

## CI workflows

Four workflows. Only `ci.yml` and `app.yml` gate a PR.

### `ci.yml` — every push and PR (Ubuntu)

- **Rust:** fmt, clippy, test, and the matcher gate on **both FST and VCD**.
- **Python:** `ruff`, pytest, schema validation.
- **RTL `always_ff` driver lint.** VCS rejects a variable written by `always_ff` that has
  any other procedural driver (IEEE 1800 §9.2.2.4) while slang and Verilator accept it —
  so CI checks it explicitly rather than discovering it downstream.
- **Golden reproducibility.** Re-elaborates `fixtures/picorv32_soc/golden/hierarchy.json`
  with **`--gate-level --name-refs`** and diffs it. Both flags are required or the diff
  reports a stale golden.
- Also gates the lean feature shape: `cargo check -p scale-bench --no-default-features`.

### `app.yml` — when `app/` or `core/crates/` change

Three jobs:

| Job | Runs | Notes |
| --- | --- | --- |
| `build` | Ubuntu + Windows | `npm test` + `npm run build` + `cargo build` — the fast PR signal |
| `bundle` | Ubuntu + Windows + macOS | A real `tauri build`, uploading each artifact. `cargo build` never exercises the bundler, so NSIS/AppImage/`.app` breakage would otherwise surface only at release |
| `release` | on a `v*` **tag** push only | `needs: [build, bundle]`, so a red test suite or one failed OS leg blocks it — no partial releases |

**`bundle` skips pull requests**, because macOS bills at 10× minutes on a private repo. A
packaging change is proven by dispatching the workflow on its branch instead;
`workflow_dispatch` takes a **`bundle_os`** input narrowing the matrix to one OS (~5 billed
minutes instead of ~49). It applies to dispatch only, so pushes and tags always build all
three.

The Linux leg builds `appimage,deb,rpm`. Both Linux packages are smoke-tested by
`.github/scripts/linux-package-smoke.sh <deb|rpm>`, which installs each in a **clean
container** — Ubuntu 24.04 to match the build host's glibc, Fedora unpinned so a package
rename surfaces here — and asserts four things: the package declares WebKitGTK, it
installs, the launcher runs headlessly (`--bench`), and its `.desktop`/icons land where a
launcher finds them.

**Three traps this job encodes, each of which has bitten:**

1. **A container is required** for the smoke test. The runner already has WebKitGTK from
   the build deps, so a local install would pass whatever `depends` declares — and unset,
   Tauri emits an RPM with *no* dependencies at all, which installs and then cannot launch.
2. **The launcher is read back out of the payload, not assumed.** Tauri installs the
   **crate** binary, not one named after `productName` — so until #275 set
   `mainBinaryName`, the installed command was `hdl-schemview-app` while every doc said
   `hdl-schemview`, and a package user typing the documented command got
   `command not found`. The config key fixes the name; reading it back out of the payload
   is what would catch the next such drift, so the smoke test still does that rather than
   asserting the name we expect.
3. **Extracting the Windows WebView2 `.cab` must call `expand.exe` by full path.** The step
   runs under Git Bash, where a bare `expand` resolves to GNU coreutils' tabs-to-spaces
   filter and dies on `-F:`. This broke every Windows bundle until it was pinned.

The Windows fixed-runtime payload needs the `WEBVIEW2_CAB_URL` repo variable. Since
`release` is `needs: [build, bundle]`, an unset variable fails the Windows leg and
publishes nothing, rather than shipping a partial release.

On a tag, `bundle`'s first step asserts that the tag and all three manifests
(`tauri.conf.json`, `app/package.json`, `app/src-tauri/Cargo.toml`) carry one version. A
tag/`tauri.conf.json` mismatch *breaks* the release, since the bundle filenames come from
that config; the other two are cosmetic. Both are reported in one run and both fail.

`release` stages just the installers flat (`.exe`/`.AppImage`/`.deb`/`.rpm`/`.dmg` — the
macOS `.app` is a directory of thousands of files), writes `SHA256SUMS`, and creates a
**draft** release for a human to check and publish. Its `contents: write` is **job-level**,
so the PR-running jobs keep the workflow's default read token.

### `nightly.yml` — scheduled

Four jobs, none of which gate a PR: `repro-tier1` (Verilator trace regeneration),
`stress-tier2` (Ibex, `continue-on-error`), `scale-bench` (the scalability collector,
`continue-on-error`, uploading a `scale-bench-metrics` artifact), and `nix-harness`
(`continue-on-error`, #280 — builds `packages.svxprobe-elaborate` and asserts it still
reproduces the committed golden byte-for-byte; it lives here rather than in `checks`
because it compiles slang from source with no binary cache). The benchmark job runs
`--online`, since CI's cold registry cache makes the default `--offline` fail.

### `nix.yml` — on `flake.*` / `core/**` changes (Ubuntu)

`nix flake check` (which builds `packages.*` and runs `checks.{fmt,clippy,test}`,
deliberately re-running `ci.yml`'s Rust gate *through the flake* — that is the thing which
rots), then `nix build` + `--help` so the binary is proven to run and not merely to link.

It also asserts the two invariants the flake exists for: `nix develop`'s rustc matches
`core/rust-toolchain.toml`, and `flake.lock` is committed and current — checked with
`git ls-files --error-unmatch` **and** `git diff --exit-code`, because an *untracked* lock
never shows in a diff, so the tracked test is the one that catches "no lock at all". The
job uploads `flake.lock` on `always()`, since the step that most often needs that artifact
is the one that just failed.

## Conventions that CI cannot check

- **No heuristics.** Resolve through model indices; the elaborated hierarchy is the single
  source of truth. See [architecture.md](architecture.md).
- **DTO sync.** Rust serde ↔ `app/src/types.ts` ↔ `elaborate/svxprobe_elaborate/schema/model.schema.json` —
  the TS layer desyncs *silently*. See [data-model.md](data-model.md).
- **Docs are part of the change.** A PR that alters architecture, commands, DTOs, gates or
  workflow updates the relevant docs in the same PR, not as a follow-up. A decision with
  lasting consequences gets an ADR in [decisions/](decisions/).
