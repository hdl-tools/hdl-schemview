# ADR 0009 — Packaging for isolated environments

- **Status:** Accepted — tier 1 shipped in `v0.1.0` (2026-08-01), pending offline-install
  verification on real hardware (#240); tier 2 gated on the PyInstaller spike (#277).
  Amended 2026-08-17: pyslang builds hermetically under Nix (#280) — evidence for the
  loop-back clause, but not a substitute for the #277 spike
- **Date:** 2026-07-26
- **Deciders:** project maintainers
- **Relates to:** `app/src-tauri` (bundle config), `core/crates/gui` (`elaborate_and_load`),
  `elaborate/` (pyslang harness), `core/crates/scale-bench`, ADR 0002 (distribution),
  ADR 0003 (storage/benchmark gate)

## Context

The tool currently assumes a developer machine: a pinned Rust toolchain, `npm`, and a
Python venv with `pyslang` reachable as `svxprobe-elaborate` **on PATH**
(`core/crates/gui/src/lib.rs:285`, `Command::new("svxprobe-elaborate")`). That is a
reasonable assumption for contributors and a fatal one for the actual deployment target:
an **isolated environment that cannot install external libraries**, where an engineer
copies one artifact across and runs it.

Two capabilities have to survive that trip:

1. **The app** — hierarchy, schematic, source, waveform, cross-probe.
2. **Benchmark metrics** — #22 and #155 established that storage and performance
   decisions here are settled by measurement, not argument. A locked-down machine is
   precisely where `cargo bench` is unavailable, so the ability to produce
   `metrics-<stamp>.md` must travel with the app or those decisions cannot be revisited
   on real hardware.

What the target machine actually needs today:

| OS | Webview runtime | Situation |
| --- | --- | --- |
| Windows | WebView2 | `tauri.conf.json` sets no `webviewInstallMode`, so Tauri defaults to `downloadBootstrapper` — **network required at install time**. |
| Linux | WebKitGTK 4.1 | `.deb`/`.rpm` declare it as a system dependency (package manager + repo access) — see the amendment below. AppImage bundles it. |
| macOS | WKWebView | Part of the OS; unsigned apps are blocked by Gatekeeper. |

`bundle.targets` is already `"all"`, so the bundler side is configuration rather than new
code. The elaboration harness is the only hard external dependency in the runtime path.

## Decision

**Adopt a two-tier packaging strategy, and treat the tiers as independently shippable.**

**Tier 1 — offline viewer + offline benchmark.** Ship an app that runs with no toolchain
and no network, without solving elaboration. Models are elaborated on a connected machine
and copied in as `hierarchy.json` — already the workflow that produced the #22/#155
real-design basis. Concretely: `webviewInstallMode: fixedRuntime` on Windows, AppImage as
the supported Linux artifact, ad-hoc signing on macOS, a clear error when the harness is
absent, and a **`--bench` subcommand** that runs the scenario matrix and writes the same
metrics file the collector scripts produce.

**Tier 2 — frozen harness sidecar.** Freeze `svxprobe-elaborate` (CPython + pyslang) into
one self-contained executable per OS, ship it via `bundle.externalBin`, and resolve it as
a Tauri sidecar with the existing PATH lookup kept as a fallback. **Gated on a spike**
proving PyInstaller handles pyslang's compiled extension on all three OSes. Tracked
separately as #277, so a shipped tier 1 is not held open behind an unstarted tier 2.

For the benchmark specifically: **ship the scenario layer only, and move the orchestration
into Rust.** Criterion stays dev-only.

## Options evaluated

1. **Two-tier bundle (chosen)** — viewer-only first, sidecar after a spike.
2. **Single full bundle** — do tier 1 and tier 2 together, ship nothing until elaboration
   works offline.
3. **Document the manual install** — tell users to install Python, pyslang, WebView2, and
   WebKitGTK themselves.
4. **Native slang bindings** — drop Python; link slang (C++) into the Rust core.

### Forces / rationale

- **The tiers have very different risk profiles.** Tier 1 is configuration plus a
  contained refactor; tier 2 rests on an unverified assumption about freezing a compiled
  Python extension. Coupling them (option 2) would let the risky half block the safe half
  from shipping, for no benefit — the viewer is useful on its own precisely because
  models are already portable JSON.
- **Option 3 is a non-answer.** "Cannot install external libraries" is the defining
  constraint of the target environment, not an inconvenience to document around.
- **Option 4 is the right long-term shape and the wrong next step.** It removes the
  Python dependency at its root, but it is a project-sized effort against a harness that
  currently works. Revisit only if the sidecar proves unworkable.
- **The benchmark must not drag a toolchain along.** Criterion needs `cargo bench`; the
  scenario layer needs nothing — `scale-bench/src/mem.rs` hand-declares its RSS probes
  (`K32GetProcessMemoryInfo`, `/proc/self/status`) specifically so the measurement layer
  has no dependencies. Packaging the scenario layer is therefore nearly free, and it
  carries the memory axes that #22 and #155 turned on. The loss is statistical
  confidence, which the packaged output must state plainly rather than imply.
- **Orchestration in shell scripts does not survive packaging.** The matrix is currently
  driven by PowerShell, bash, and a Python table renderer. A packaged app can rely on
  none of them — the Windows `python3` shim (present on PATH, fails on execution) already
  broke the bash collector once. Folding the logic into Rust removes both the dependency
  and the ps1/sh duplication.

## Consequences

- **Bundle size grows substantially.** fixedRuntime WebView2 is ~180 MB; a PyInstaller +
  pyslang sidecar plausibly adds 50–100 MB. Acceptable for a copy-once artifact,
  unacceptable to pretend away in release notes.
- **macOS becomes a supported target with no CI coverage today** — `app.yml` builds Ubuntu
  and Windows only. Tier 1 must add a macOS runner, and signing/notarization is its own
  workstream.
- **A packaged benchmark run is single-shot.** No criterion, so latency figures are
  order-of-magnitude; the memory axes are unaffected. The output header must say which
  layer produced it so a packaged run is never compared against a `cargo bench` run.
- **The `golden` basis needs rehoming.** `scale-bench` resolves the committed fixture via
  `CARGO_MANIFEST_DIR` at compile time, which does not exist in a packaged build — either
  `include_bytes!` the 2.1 MB golden or drop that basis when packaged.
- **Benchmark code lands in the product binary** unless gated behind a cargo feature. The
  synthetic generator is small and dependency-free, but a lean build should be able to
  exclude it.
- **Elaboration stays reproducible across tiers.** The sidecar must produce a model
  identical to the connected-machine one; if it cannot, the tier fails its own exit
  criterion rather than shipping a second dialect of the golden format.
- This ADR concerns **distribution only**. It does not revisit the FSDB/plugin boundary
  (ADR 0002) or the storage backend (ADR 0003).

## Amendment — 2026-08-02: `.rpm` joins the connected tier (#260)

When this ADR was written the table above named `.deb`/`.rpm` together, but CI built
only the `.deb`: `app.yml` excluded `.rpm` on the theory that bundling it on Ubuntu
"can fail and take the job down". That premise did not hold. Tauri 2 writes the RPM
itself through the pure-Rust `rpm` crate — the pinned `@tauri-apps/cli` 2.11.3 carries
a full `RpmConfig` and its native binary contains no `rpmbuild` string — so
`ubuntu-latest` needs no extra toolchain. The Linux leg now builds `appimage,deb,rpm`.

What this amendment does **not** change: **AppImage remains the supported
isolated-machine artifact.** `.deb` and `.rpm` are one tier — *"Linux (connected)"* —
both declaring WebKitGTK as a system dependency and both requiring a package manager
with repo access. Neither is a candidate for the no-network target this ADR exists for.

Two consequences worth recording:

- **An RPM with no `depends` is the failure mode to fear.** Unlike the `.deb`, Tauri
  emits no dependencies at all unless `bundle.linux.rpm.depends` is set — a package
  that installs cleanly and then cannot launch. The names differ from Debian's
  (`webkit2gtk4.1`/`gtk3` vs `libwebkit2gtk-4.1-0`/`libgtk-3-0`), so this cannot be
  copied across. CI asserts the declaration is present *and* installs the package in
  a clean Fedora container, because the build runner has WebKitGTK already and would
  pass regardless.
- **The `.deb` still has no such coverage** and has never been installed by CI or by
  the runbook (#261). It ships on the strength of its config alone.

## Loop-back

If the PyInstaller spike fails on any target OS, tier 2 is not merely delayed — the
alternatives are a per-OS system Python requirement (which violates the constraint that
motivated this ADR) or native bindings (option 4). Record the spike result here either
way; a negative result is the input that promotes option 4 from "later" to "next".

## Amendment — 2026-08-17: pyslang builds hermetically under Nix (#280)

The Loop-back clause above asks for the spike result to be recorded "either way,"
because a negative one promotes option 4 (native slang bindings) from *later* to
*next*. #280 is a **different** spike — Nix, not PyInstaller — but it bears on the
same underlying question, so the result belongs here.

**Result: yes.** `pyslang==11.0.0` builds from the PyPI sdist inside a
`sandbox = true` derivation, with no network and no patches, and the resulting
harness reproduces the committed `picorv32_soc` golden byte-for-byte. It ships as
`packages.svxprobe-elaborate`.

The blocker was never "slang uses FetchContent". Modern slang declares every
dependency as `FetchContent_Declare(... FIND_PACKAGE_ARGS <version>)`, and
`FIND_PACKAGE_ARGS` (CMake >= 3.24) runs `find_package` **first** — the git clone
is only a fallback. Supplying `fmt >= 12.1` and `pybind11 >= 3.0` is the whole
mechanism; neither existed in `nixos-25.05`, which is why the flake now tracks
`nixos-25.11`. Boost is optional behind a vendored single header, and
mimalloc/cpptrace/Catch2 never fire for a pylib build.

Two consequences worth recording:

- **This does not close #277, and does not by itself change option 4's standing.**
  The two spikes fail for different reasons: Nix *builds* the extension from source
  in a controlled environment, whereas PyInstaller must *relocate* an already-built
  `.so` onto a machine with no Python at all. A hermetic source build lowers the
  prior that the extension is intractable to package, but it does not demonstrate
  the property tier 2 actually needs. #277 still has to be run on all three OSes.
- **It does not serve the isolated machine either.** #243 already recorded that a
  machine which cannot install libraries cannot install Nix; this is a
  reproducible-dev-and-CI channel, not a second attempt at tier 1. The workflow
  `harness_missing_message()` names — elaborate elsewhere, copy the JSON across —
  remains the answer for the isolated case until tier 2 lands.

A third, incidental finding: packaging is what exposed `validate.py` resolving
`model.schema.json` as a *sibling* of its package directory, so the schema was
absent from every wheel ever built and a non-editable `pip install .` produced a
harness that raised on any validation call. Fixed in #306. This is the ordinary
value of packaging something — it exercises the install path that day-to-day
`uv run` never touches.
