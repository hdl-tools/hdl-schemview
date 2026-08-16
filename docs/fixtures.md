# Reference fixtures

The fixtures are the regression target for every phase's exit gate (roadmap §5).
Per the roadmap, **do not invent ad-hoc test files in later phases — extend these.**

## The Phase-1 threshold (pinned here)

The cross-probe matcher's go/no-go gate, measured on these fixtures:

> **≥ 95% of design-scope signals matched, with every miss attributable to a
> named normalization-rule gap (zero mystery misses), against both the FST and
> the VCD.**

"Design-scope" excludes the scopes listed in each fixture's `excluded_scopes.txt`
(testbench wrappers + simulator-internal scopes). This threshold is enforced
starting in Phase 1; Phase 0 only establishes the fixtures and the round-trip.

## Two-tier policy

| Tier | Fixture | Core | Gates PRs? | Where it runs |
|---|---|---|---|---|
| 1 | `picorv32_soc/` | PicoRV32 + SV wrapper (vendored) | **Yes** | `ci.yml` (committed traces) + `nightly.yml` (regen) |
| 2 | `ibex_soc/` | lowRISC Ibex (fetched, pinned) | No | `nightly.yml` only (`continue-on-error`) |
| — | `hls_min/` | synthetic HLS provenance micro-fixture | **Yes**, via `cargo test` | `ci.yml` (Rust test job) |

Tier 1 is small enough that the golden hierarchy is hand-verifiable and CI is
fast and Verilator-free. Tier 2 stresses the elaboration/match path on a real
production core; it is experimental until the hardening tasks in
`ibex_soc/README.md` are done.

`hls_min/` sits outside the tiers on purpose. It is not a *design* fixture — it is the
smallest artifact that exercises one construct the reference design cannot contain
(see the amended rule in [`ROADMAP.md`](ROADMAP.md) §9): HLS provenance comments only
appear in **generated** RTL, so adding them to a hand-written core would make it not
hand-written. It gates PRs through the Rust test suite rather than the golden-diff job,
and `fixtures/regen.sh` does not know about it — see below.

## Tier-1: `picorv32_soc`

A PicoRV32 core wrapped in SystemVerilog to exercise the four constructs the
matcher exists to handle:

- **package** — `soc_pkg.sv` (types/params, a packed enum + struct)
- **interface** — `mem_if.sv` (the memory bus, one instance per lane, with modports)
- **parameterized instances** — `N_CORES` PicoRV32 lanes
- **generate block** — `g_lane[0..N-1]` (produces `genblk`/array scope naming —
  the matcher's hardest case)

```
picorv32_soc/
  rtl/        picorv32.v (vendored, ISC) + soc_pkg.sv + mem_if.sv + soc_mem.sv + picorv32_soc.sv
  tb/         tb_picorv32_soc.sv (deterministic) + gen_firmware.py (tiny RV32I assembler)
  traces/     picorv32_soc.vcd, picorv32_soc.fst   (frozen, committed)
  golden/     hierarchy.json   (elaborated spine incl. parameters, gate-level projection, and name_refs; reproducible from RTL)
  excluded_scopes.txt          (TOP, tb, soc_pkg)
```

### Regenerating

```bash
# Traces (needs Verilator; produces BOTH formats from one deterministic run):
bash fixtures/regen.sh picorv32_soc

# Golden hierarchy (needs the elaborate harness; deterministic). The explicit
# file order matches ci.yml — a bare *.sv glob would skip picorv32.v.
#
# BOTH opt-in flags are required, exactly as ci.yml passes them — omit either and
# the regenerated golden differs from the committed one and CI fails the diff:
#   --gate-level (#199)  the gate/mux/const projection. Additive; process-level
#                        rendering ignores it, so the default view is unchanged
#                        while the fixture also exercises the gate-level toggle.
#   --name-refs (#225)   identifier-occurrence spans. Also additive (schema_version
#                        stays 1); they feed the source pane's semantic coloring and
#                        usage-click resolution. Adding this field bumped ingest's
#                        RKYV_FORMAT_VERSION 1 -> 2.
# The app's own designlist path (elaborate_and_load) always passes both, so the
# committed golden mirrors what the GUI loads.
uv run --project elaborate svxprobe-elaborate --top picorv32_soc \
    fixtures/picorv32_soc/rtl/picorv32.v \
    fixtures/picorv32_soc/rtl/soc_pkg.sv \
    fixtures/picorv32_soc/rtl/mem_if.sv \
    fixtures/picorv32_soc/rtl/soc_mem.sv \
    fixtures/picorv32_soc/rtl/picorv32_soc.sv \
    --gate-level \
    --name-refs \
    -o fixtures/picorv32_soc/golden/hierarchy.json

# Verify committed traces are still reproducible (structural; restores committed):
bash fixtures/verify_reproducible.sh picorv32_soc
```

CI guards both: `ci.yml` re-elaborates the golden and diffs it (deterministic),
and `nightly.yml` regenerates the traces with Verilator and checks structural
equivalence.

> **Line endings (#203).** The golden's `def_range` byte offsets are computed
> against LF source. A repo-wide `.gitattributes` pins `*.v`/`*.sv`/`*.svh` and
> `fixtures/**/*.json` to `eol=lf`, so every checkout (including Windows) has an LF
> working tree that matches those offsets — regenerate the golden directly, with no
> manual `tr -d '\r'` step. If you have a pre-`.gitattributes` CRLF checkout, run
> `git add --renormalize .` once.

## `hls_min` — HLS C↔RTL provenance (#159, #222)

A synthetic two-file stand-in for HLS output, exercising [ADR 0006](decisions/0006-hls-cpp-rtl-source-tracing.md):

```
hls_min/
  foo.cpp     the "original" C source
  foo.sv      generated-style RTL (module mac) carrying `// foo.cpp:N` provenance comments
  hls_min.f   designlist, for loading it through the app's designlist path
  golden/     hierarchy.json — language + source_map (--hls-map) and gate primitives (--gate-level)
```

Gated by `cargo test` (`core/crates/ingest/src/lib.rs:631` ingests the golden and checks the
map), **not** by the golden-diff job — `ci.yml`'s reproducibility step regenerates tier-1 only.
`fixtures/regen.sh` doesn't handle it either; regenerate by hand from the repo root, with the
flag set this golden was built from (verified to reproduce it byte-for-byte):

```bash
uv run --project elaborate svxprobe-elaborate --top mac \
    fixtures/hls_min/foo.sv \
    --gate-level --hls-map --hls-src fixtures/hls_min/foo.cpp \
    -o fixtures/hls_min/golden/hierarchy.json
```

Note this golden is **not** elaborated with `--name-refs` (unlike tier-1's) — nothing in its
test path reads identifier spans, and leaving them out keeps it minimal. There are no traces
either: it exercises the source-map path, never the matcher.

## Tier-2: `ibex_soc`

See [`ibex_soc/README.md`](../fixtures/ibex_soc/README.md). Fetched, not
vendored; experimental until hardened.

## Pinned tooling

| Tool | Pin | Where |
|---|---|---|
| Rust | `1.94` + rustfmt/clippy | `core/rust-toolchain.toml` |
| Rust deps | `Cargo.lock` | `core/Cargo.lock` |
| pyslang | `==11.0.0` | `elaborate/pyproject.toml` + `uv.lock` |
| Python | `>=3.11` | `elaborate/pyproject.toml` |
| Verilator | 5.x (apt `5.020` verified; `flake.lock` pins **`5.040`**) | nightly / local |
| nixpkgs | `flake.lock` (`nixos-25.11`) | repo root |

Byte-for-byte trace reproducibility requires a pinned Verilator — use
`nix develop .#verilator`. The apt package is the verified fallback and is
checked structurally (scope/var counts), not byte-for-byte.

> **The FST leg needs zlib (#280).** Verilator compiles its FST writer
> (`fstapi.c`) in *your* environment rather than inside its own derivation, and
> nixpkgs carries no zlib in `verilator`'s `buildInputs`. Both dev shells
> therefore ship `pkgs.zlib`; without it `regen.sh` dies on `fatal error:
> zlib.h: No such file or directory` and only the VCD leg builds. This went
> unnoticed because `nightly.yml` uses **apt** Verilator on a runner that already
> has `zlib1g-dev` — so CI was exercising a different environment than the one
> this page recommends.

That pin only holds because **`flake.lock` is committed** (#243). Before it,
`nixpkgs` floated to whatever the channel currently pointed at, so two machines
could resolve two different Verilator builds — the exact failure this table exists
to prevent. Naming `5.040` in the table above is only possible *because* of the
lock: without one the question "which Verilator does `nix develop` give me?" had no
answer. Relocking (`nix flake update`) is therefore a deliberate act that can
change a pinned tool version; re-verify the tier-1 traces after one, and update
this row.

**That clause was first exercised on 2026-08-16 (#280).** Moving to `nixos-25.11`
— required for `fmt >= 12.1` and `pybind11 >= 3.0`, which pyslang needs in order to
build hermetically — carried Verilator `5.034` -> `5.040`, and the tier-1 traces
were regenerated under it. The FST scope count moved **18 -> 17** while `vars`
stayed at 1682, and the regenerated FST now agrees with its VCD sibling, which has
always reported 17: 5.040 stopped emitting one variable-less scope that 5.034 wrote
into the FST only. Both formats were re-checked before the new bytes were
committed — 477 signals linked with identical rule counts (`AnchorRebase` 1577,
`ArrayElement` 1088, `InterfaceAlias` 16) on FST and VCD alike, matcher gate PASS.
