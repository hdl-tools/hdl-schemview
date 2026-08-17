# ADR 0012 — Nix outputs are a build channel, not a release channel

- **Status:** Accepted
- **Date:** 2026-08-17
- **Deciders:** project maintainers
- **Relates to:** `flake.nix`, `nix/`, `.github/workflows/{nix,nightly}.yml`,
  ADR 0009 (packaging for isolated environments), #243, #279, #280

## Context

The flake now has outputs at three different levels of assurance, and nothing said
which of them the project stands behind.

| Output | Built by | Assurance |
| --- | --- | --- |
| `packages.svxprobe` | `nix flake check`, every PR | gated |
| `packages.{svxprobe-elaborate,pyslang}` | `nightly.yml` | best-effort |
| `packages.hdl-schemview-app` (Linux) | `nightly.yml` | best-effort |

The costs are not comparable. The Rust gate runs in ~2.5 min. The harness compiles
slang from source — **21m22s measured on 8 cores** — and the app links webkitgtk.
There is **no binary cache** anywhere in this project (`docs/releasing.md`), so every
build is from source, on every run, for every consumer.

Two facts shape what gating buys. First, `nix flake check` **evaluates** every output
attribute but **builds only `checks.<system>`** — so a malformed or eval-erroring
package is caught for free, and only *build-time* rot escapes. Second, the things
these packages wrap are already gated elsewhere: `ci.yml` runs the harness's 97 tests
through `uv` on every PR, and `app.yml` builds the Tauri app and runs its frontend
tests on every PR. Gating the Nix outputs adds *packaging* coverage, not correctness
coverage.

Nix here also serves a different audience than ADR 0009. #243 recorded that a machine
which cannot install libraries cannot install Nix either; this is a reproducible
dev/CI channel, not a second attempt at the isolated-machine tier.

## Decision

**`checks` defines the supported surface. Everything else in `packages` is
best-effort, and is never a release artifact.**

1. **`checks` stays Rust-only** — `fmt`, `clippy`, `test`, `svxprobe`. `nix flake
   check` must stay in the low minutes, because it runs on every `core/**` PR.
2. **The harness and the app are `packages` only.** `nightly.yml` builds both,
   `continue-on-error`, so rot surfaces within a day without any PR paying for it.
   The harness job additionally asserts the packaged harness reproduces the committed
   golden byte-for-byte; the app job asserts the wrapper carries its GTK environment.
3. **The desktop app is Linux-only**, expressed as attribute *presence*
   (`lib.optionalAttrs stdenv.hostPlatform.isLinux`) rather than `meta.platforms` — a
   meta restriction leaves the attribute evaluable on darwin, where it would hit
   `webkitgtk`. `packages.<system>` should not offer what it cannot build.
4. **The app is not in `overlays.default`; the harness and pyslang are.** An overlay
   attribute is part of a consumer's `pkgs` and gets walked by `nix search`, NixOS
   module evaluation and the like. A Linux-only, CI-unbuilt attribute referencing
   webkitgtk would fail for a darwin consumer who never asked for it. `pyslang` earns
   its place independently: it is absent from nixpkgs, so a downstream flake otherwise
   has to vendor the derivation.
5. **Nothing Nix-built is attached to a GitHub release.** The tagged artifacts remain
   #240's AppImage/`.deb`/`.rpm`.

## Options evaluated

1. **Gate everything** — add `checks.elaborate` and `checks.app`. Rejected: it puts a
   from-source slang build and a webkitgtk link on every `core/**` PR, in a repo with
   no cache, to re-prove what `ci.yml` and `app.yml` already prove.
2. **Gate nothing, not even nightly** — rejected. These outputs are public API; a
   consumer hitting a stale `npmDepsHash` before we do is the failure to avoid.
3. **Add a binary cache (Cachix) and gate everything** — the principled fix, and it
   would make option 1 affordable. Out of scope here: it needs an account, a secret,
   and is a recurring-cost decision already tracked as an open question on #243.
4. **The chosen split** — gate the cheap thing, watch the expensive things nightly.

## Consequences

- **`packages.hdl-schemview-app` may be broken at any given commit.** Accepted, and
  stated in `docs/releasing.md` rather than left for a consumer to discover. The
  residual rot surface is build-time only: a stale `npmDepsHash` after
  `app/package-lock.json` changes, a lockfile change pulling a crate that needs a
  system library, a nixpkgs `webkitgtk`/`gtk3` bump, `tauri.conf.json` drift, or a
  `core/crates/{gui,wave,schematic}` API break — `core/**` *is* in `nix.yml`'s filter,
  but the app is not built there.
- **CI no longer proves `devShells.default` builds.** `nix.yml` asserts the rustc pin
  against a toolchain-only `devShells.ci`, because entering the default shell now
  builds pyslang (#280). Both shells share one `toolchain` binding, so the pin itself
  is still asserted; the nightly harness job covers the rest of that closure.
- **A consumer builds everything from source.** Without a cache, `nix run
  .#hdl-schemview-app` compiles the Rust tree *and* pulls webkitgtk. Adding a cache
  would change this decision's arithmetic and should reopen it.
- **The app output is not covered by the tag-vs-manifest version guard.** It reads its
  version from `tauri.conf.json` (`lib.importJSON`), the manifest the bundle job treats
  as authoritative, so it cannot report a version the release does not have. Note the
  asymmetry this creates: `core`'s crates are all `0.0.0`, so `packages.svxprobe`
  reports `0.0.0` regardless of tag, while the app tracks a real release number.
- **On a non-NixOS host the app needs help reaching the GPU.** A Nix-built WebKitGTK
  binds the store's GL stack, which will not match a distro's driver; the answer is
  [nixGL](https://github.com/nix-community/nixGL), or `WEBKIT_DISABLE_DMABUF_RENDERER=1`
  for software rendering. Documented in `app/README.md`, deliberately **not** baked into
  the wrapper — hard-setting it would degrade rendering on hosts that do not need it.
