# Releasing — cutting a desktop bundle release

> How a `v*` tag turns into a **draft GitHub Release** carrying the desktop bundles
> plus a `SHA256SUMS` file, and what a human still has to do afterwards.
> Implements #248; the deployment context is ADR 0009 and `app/README.md`
> §Distribution.

## Why a release and not just the CI artifacts

`app.yml`'s `bundle` job has produced the three artifacts since #240, but as
`actions/upload-artifact` uploads: **14-day retention**, reachable only through the
Actions UI, with no integrity check. The deployment story is *"copy one installer to
a machine with no network"*, so an expiring, awkward-to-fetch, unchecksummed ~200 MB
file is the wrong vehicle. A tag-triggered release is durable, linkable and
checksummed. It reuses the **same** bundle job — there is deliberately no second
workflow (#246's precedent: two thin wrappers over one matrix diverge).

## Prerequisites — check these before tagging

A tag push is expensive and mostly unattended, so confirm both first. Neither
produces a partial release: they produce **no** release.

- **The `WEBVIEW2_CAB_URL` repository variable must be set** (Settings → Secrets
  and variables → Actions → Variables). The Windows leg of `bundle` vendors a
  pinned WebView2 fixed-version runtime, and its *Fetch WebView2 fixed runtime*
  step fails outright when the variable is absent — get the `.cab` URL from
  <https://developer.microsoft.com/microsoft-edge/webview2/> (Fixed Version). It is
  pinned on purpose: the runtime version is part of what the bundle ships.

  Because `release` is `needs: [build, bundle]`, **one failed OS leg skips the
  release job entirely**. That is the intended behaviour — no half-populated
  releases — but it means an unset variable turns a tag push into a full 3-OS
  build that publishes nothing.

  Set as of 2026-08-01, pinning
  `Microsoft.WebView2.FixedVersionRuntime.150.0.4078.99.x64` (a ~284 MB `.cab`).
  Re-pinning is an ordinary edit of the variable; the cache key includes the URL,
  so a new one invalidates the cached runtime automatically.

- **Actions quota.** `bundle` builds on all three OSes and macOS bills at 10× on a
  private repo. A tag pushed with the quota exhausted fails instantly with
  zero-step jobs and no logs — an unhelpful signature that looks nothing like a
  code failure, so check the billing page before assuming the workflow is broken.

## Cutting a release

1. **Bump the version** — the same number in all three manifests:

   | File | Field | Role |
   | --- | --- | --- |
   | `app/src-tauri/tauri.conf.json` | `version` | **Load-bearing.** Names the bundle files (`hdl-schemview_0.2.0_x64-setup.exe`). |
   | `app/package.json` | `version` | The frontend package (`private`, never published). |
   | `app/src-tauri/Cargo.toml` | `[package] version` | The Tauri shell crate. Run `cargo metadata` afterwards so `app/src-tauri/Cargo.lock` follows. |

   Tauri's config version wins for bundle naming, so only the first affects the
   artifacts — but **CI enforces all three**, so missing one fails the tag build
   (see *The version guard*).

2. **Commit and merge** that bump to `main` as normal.

3. **Tag and push** — `v` + the exact config version:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

   The tag must match, or `bundle` fails within seconds of starting (see *The
   version guard* below).

## What CI does

A `refs/tags/v*` push runs `app.yml` in full. The workflow's `paths:` filter does
not suppress this: GitHub does not evaluate path filters for pushes of tags.

| Job | On a tag | What it contributes |
| --- | --- | --- |
| `build` | ✅ | `npm test`, `npm run build`, `cargo build` (Ubuntu + Windows). A red test suite **blocks the release**. |
| `bundle` | ✅ | The real `tauri build` on Ubuntu + Windows + macOS. Windows uses the `tauri.offline.conf.json` overlay, so WebView2 is vendored. Linux additionally installs the `.deb` and `.rpm` in clean containers, runs each headlessly, and validates its desktop entry and icons (#260, #261). |
| `release` | ✅ | Downloads every bundle artifact, stages the installers flat, writes `SHA256SUMS`, and creates a **draft** release. |

Notes on the `release` job:

- **One aggregating job**, not per-matrix-leg uploads — three legs racing on
  `gh release create` would fight over creating the same release.
- **`needs: [build, bundle]`** on a `fail-fast: false` matrix: if any single OS leg
  fails, the release job is skipped entirely. There are no partial releases.
- **`permissions: contents: write` is job-level.** The workflow default stays
  `read`, so `build` and `bundle` — which run on every PR — never hold a write
  token.
- It selects assets by extension (`.exe`, `.AppImage`, `.deb`, `.rpm`, `.dmg`)
  rather than uploading the bundle trees: macOS's `.app` alone is a directory of
  thousands of files.

### The version guard

The first step of `bundle` on a tag push asserts that the tag and all three
manifests carry the same version. It catches two distinct failures, and reports
both in one run so they can be fixed in one pass:

| Failure | Consequence |
| --- | --- |
| tag ≠ `tauri.conf.json` | **Breaks the release.** Bundle filenames come from the config, not the tag, so a `v0.2.0` tag against a `0.1.0` config produces a release full of `hdl-schemview_0.1.0_*` assets. |
| the three manifests disagree | Cosmetic — nothing reads `package.json`/`Cargo.toml` for naming — but the repo then reports two versions of itself. |

Both are hard failures. It is a step rather than its own job because a job skipped
by its own `if:` also skips everything that `needs:` it. The `Cargo.toml` read is
anchored (`^version = `) so it picks `[package]`, not the inline dependency
versions.

## What the human does

The release is created as a **draft** on purpose: macOS is unverified end-to-end
(ADR 0009), so auto-publishing a `.dmg` nobody has opened would overclaim.

1. Open the draft under **Releases**.
2. Check the assets — the installers plus `SHA256SUMS`, all carrying the expected
   version in their filenames.
3. Sanity-run at least the Windows installer and the AppImage. The `.deb` and
   `.rpm` need no manual check: `bundle` installs each in a clean container,
   runs it headlessly, and validates its desktop entry and icons (#260, #261).
   What CI cannot check is whether the launcher entry *looks* right in a desktop
   environment — if that matters for a release, open it once by hand.
4. Edit the generated notes if needed, then **Publish**.

## Verifying an artifact after copying

On the target machine, next to the downloaded files:

```bash
sha256sum -c SHA256SUMS          # Linux/macOS, or Git Bash on Windows
```

Windows without coreutils:

```powershell
certutil -hashfile hdl-schemview_0.2.0_x64-setup.exe SHA256
# compare against the matching line in SHA256SUMS
```

`SHA256SUMS` carries bare basenames (the assets are staged flat before hashing), so
it works directly against a directory of downloaded assets.

## Which artifact to hand over

Per `app/README.md` §Distribution:

| OS | Asset | Note |
| --- | --- | --- |
| Windows | `*-setup.exe` (NSIS) | WebView2 vendored — no network, no admin rights. |
| Linux (isolated) | `*.AppImage` | Self-contained. **Use this one** on a locked-down box. |
| Linux (connected) | `*.deb` | Debian/Ubuntu. Declares WebKitGTK as a system dependency, so it needs a package manager with repo access. |
| Linux (connected) | `*.rpm` | Fedora/RHEL. Same tier as the `.deb` — declares WebKitGTK, needs repo access. |
| macOS | `*.dmg` | Ad-hoc signed only — see below. |

macOS is signed ad-hoc (`signingIdentity: "-"`), which does **not** clear Gatekeeper
on a downloaded app. After copying:

```bash
xattr -dr com.apple.quarantine /Applications/hdl-schemview.app
```

> ⚠️ macOS remains **unverified end-to-end** (ADR 0009): CI proves it builds and
> bundles, but no maintainer has a macOS machine.

## The Nix channel — a tag is already the release

Nix is a **second channel with a different audience** (#243, and explicitly not a
second attempt at ADR 0009: a machine that cannot install libraries cannot install
Nix either). It needs no step in this runbook, because the flake outputs are
addressable at any tag the moment it is pushed:

```bash
nix run   github:chuanseng-ng/hdl-schemview/v0.2.0#svxprobe -- --help
nix build github:chuanseng-ng/hdl-schemview/v0.2.0#svxprobe
```

Downstream flakes consume the package through `overlays.default`.

**Nothing Nix-built is attached to the GitHub release, on purpose.** A Nix store
path is not a usable asset without `nix copy` and a binary cache, and a tarball of
one is worse than the tag it came from — the tag is content-addressed, the tarball
is not. The flake covers `packages.svxprobe` (the CLI) and, since #280,
`packages.svxprobe-elaborate` (the elaboration harness, `pyslang` and all),
`packages.pyslang` on its own, and — on Linux only, since #279 —
`packages.hdl-schemview-app`.

Neither the harness nor the app is in `checks`, so `nix flake check` does not
build them: the harness compiles slang from source, the app links webkitgtk, and
there is no binary cache. `nightly.yml`'s `nix-harness` and `nix-app` jobs cover
them instead, non-blocking. See [ADR 0012](decisions/0012-nix-outputs-are-a-build-channel.md),
which records that `checks` is the supported surface and everything else in
`packages` is best-effort.

**The Nix app output is not a release artifact and may be broken at any given
commit.** The tagged Linux artifacts remain the AppImage/`.deb`/`.rpm` above. It
is also outside the tag-vs-manifest version guard — it reads its version from
`tauri.conf.json`, the manifest that guard already treats as authoritative, so it
cannot report a version the release does not have. Before pushing a `v*` tag,
optionally confirm it still builds:

```bash
nix build .#hdl-schemview-app -L && ./result/bin/hdl-schemview --bench --help
```

A failure there files a follow-up; it does not block the release.

Two consequences worth knowing before cutting a tag:

- **The bundle version guard does not cover the flake.** `core/`'s crates are all
  `0.0.0` (`[workspace.package]`), so `nix build …#svxprobe` reports `0.0.0`
  regardless of the tag. The tag is the identifier; the package version is not.
- **Consumers rebuild from source.** There is no binary cache, so a first
  `nix run` compiles the whole Rust dependency tree. Adding one (e.g. Cachix) is
  a recurring-cost decision tracked on #243, not part of cutting a release.

`.github/workflows/nix.yml` keeps the outputs from rotting: it runs
`nix flake check` (which mirrors `ci.yml`'s Rust gate through the flake), builds
and runs the binary, and asserts both that the dev shell's rustc matches
`core/rust-toolchain.toml` and that `flake.lock` is committed and current.

## Caveat — this is a staging point, not a distribution channel

The repository is **private**, so release assets are visible only to collaborators.
If the engineers running the isolated machines are not on the repo, a release does
not reach them. What it gives is a durable, checksummed place for a maintainer to
download from and hand-carry — strictly better than 14-day Actions artifacts, but it
does not by itself solve last-mile delivery. Making that work is a separate,
undecided question: a public releases repo, an internal mirror, or accepting
hand-carry.

Elaboration is likewise **not** in the bundle (#277, tier 2 of #240, gated on a
PyInstaller spike). Getting a design onto the isolated machine still means
elaborating a `hierarchy.json` on a connected box and copying it across — see
`app/README.md` §*Getting a design in*.

## Cost note

macOS runners bill at **10× minutes** on a private repo, and `bundle` builds on all
three OSes. That is why the trigger is `tags: ["v*"]` rather than every push to
`main`, and why tags should be cut deliberately rather than used as scratch markers.

A full matrix costs roughly **49 billed minutes** (macOS ~2 min × 10, Windows
~12 min × 2, Ubuntu ~5 min × 1). Since `bundle` skips pull requests, a packaging
change can only be proven by dispatching the workflow on its branch — so
`workflow_dispatch` takes a `bundle_os` input that narrows the matrix (#260):

```bash
gh workflow run app.yml --ref feat/my-packaging-branch -f bundle_os=ubuntu-latest
```

That is ~5 billed minutes instead of ~49. The input affects **dispatch only** — a
push to `main` and a `v*` tag always build all three, so no release can be cut
from a partial matrix.
