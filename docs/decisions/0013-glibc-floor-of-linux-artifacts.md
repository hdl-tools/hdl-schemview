# ADR 0013 — The Linux bundle runner is pinned, because it is the glibc floor

- **Status:** Accepted
- **Date:** 2026-09-12
- **Deciders:** project maintainers
- **Relates to:** `.github/workflows/app.yml`, `.github/scripts/linux-package-smoke.sh`,
  ADR 0009 (packaging for isolated environments), ADR 0012 (Nix outputs are a build channel)

## Context

`app.yml` built its Linux legs on `runs-on: ubuntu-latest`. That label is not a constant:
GitHub repoints it at the newest LTS image, and it now resolves to Ubuntu 24.04 (glibc 2.39).

glibc is backward- but **not forward-compatible**. A binary linked against 2.39 records that
requirement in `DT_VERNEED`, and the dynamic loader on an older host refuses it before any
symbol is resolved — so the failure is total, not degraded, and the message names glibc rather
than anything the user did.

The v0.2.0 `.deb` shipped exactly this. Its binary carried:

```
required from libc.so.6:
  ... GLIBC_2.34, GLIBC_2.39
```

with `GLIBC_2.39` pulled in by two weakly-referenced symbols, `pidfd_spawnp` and
`pidfd_getpid`. Rust's std uses the pidfd spawn path when glibc ≥ 2.39 is present *at compile
time*; the weak binding does not help, because the version dependency is checked first. Every
other reference in the binary was ≤ 2.34. On Ubuntu 22.04 — still in LTS support — the package
installs cleanly and then dies at exec with:

```
/lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
```

Three existing checks all passed while this was true, which is the part worth recording:

1. The `bundle` job built and packaged without error — the build host *had* 2.39.
2. The AppImage self re-exec smoke ran on the same runner, so it also had 2.39.
3. `linux-package-smoke.sh` installed the `.deb` in a container pinned to `ubuntu:24.04`,
   chosen — per its own comment — *"to match the build host's glibc"*. Matching the build host
   is precisely what makes the check blind to this class of bug.

The Rust version compounds it but is not the root cause: the app leg picks up the runner's
default stable toolchain (1.97.1), not the repo pin, because `rustup toolchain install` runs
with `working-directory: core` and `app/src-tauri/` has no `rust-toolchain.toml`. Pinning that
would move the floor, not remove it; the next stable that adopts a 2.39-gated libc call brings
it straight back.

## Decision

**The runner that builds the Linux bundles is a declared compatibility floor, and is pinned
to a specific image.** Concretely:

- `app.yml`'s `build` and `bundle` matrices name `ubuntu-22.04`, never `ubuntu-latest`. The
  `release` job stays on `ubuntu-latest` — it aggregates artifacts and compiles nothing.
- The `.deb` smoke container is the **oldest supported target** (`ubuntu:22.04`), not a match
  for the build host. A smoke base at or above the build host cannot fail on glibc.
- A `glibc floor` step in `bundle` reads the release binary's maximum `GLIBC_*` verneed with
  `objdump -T` and fails above `GLIBC_2.35`.

The stated floor is therefore **glibc 2.35** — Ubuntu 22.04, Debian 12, RHEL 9 — documented in
`app/README.md` alongside the build prereqs.

## Alternatives considered

**Leave it on `ubuntu-latest` and tell users to build from source.** Rejected: it makes the
release artifacts useless to the exact audience they exist for (ADR 0009's "connected" tier is
still supposed to be a package you install, not a toolchain you provision).

**Build the Linux leg in a `container: ubuntu:22.04` on `ubuntu-latest`.** Strictly more
durable — it decouples the floor from the hosted-image lifecycle, which is the known weakness
of the chosen option — at the cost of provisioning Rust, Node and the WebKitGTK dev packages
inside the container on every run. Deferred, not dismissed; it is the migration to make when
the `ubuntu-22.04` image is retired.

**Pin the Rust toolchain for `app/src-tauri` too.** Addresses the proximate trigger and not
the mechanism. Worth doing for reproducibility, but it is not a glibc policy.

**Static linking / musl.** Not available: the app links WebKitGTK, which is a system library
by construction.

## Consequences

- Linux artifacts run on glibc ≥ 2.35. The floor is now a stated property with a test, rather
  than a side effect of whatever GitHub last pointed a label at.
- **The pin will expire.** GitHub retires runner images ahead of the distribution's own EOL, so
  `ubuntu-22.04` is on a clock. When it is removed the workflow fails loudly at scheduling
  time — an acceptable failure mode, and the cue to move to the container option above.
- Raising the floor is now a deliberate act: someone must edit the matrix, the smoke base, the
  assertion threshold and this record together. That friction is the point.
- The compile-only `build` leg is held in lockstep with `bundle`, so PRs exercise the same
  glibc as the artifacts users install. It costs nothing — the Ubuntu leg is ~5 billed minutes.
- Nothing here applies to the Nix outputs, which carry their own glibc (ADR 0012).
