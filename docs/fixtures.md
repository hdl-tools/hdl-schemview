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

Tier 1 is small enough that the golden hierarchy is hand-verifiable and CI is
fast and Verilator-free. Tier 2 stresses the elaboration/match path on a real
production core; it is experimental until the hardening tasks in
`ibex_soc/README.md` are done.

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
  golden/     hierarchy.json   (elaborated spine incl. parameters; reproducible from RTL)
  excluded_scopes.txt          (TOP, tb, soc_pkg)
```

### Regenerating

```bash
# Traces (needs Verilator; produces BOTH formats from one deterministic run):
bash fixtures/regen.sh picorv32_soc

# Golden hierarchy (needs the elaborate harness; deterministic):
uv run --project elaborate svxprobe-elaborate --top picorv32_soc \
    fixtures/picorv32_soc/rtl/*.sv -o fixtures/picorv32_soc/golden/hierarchy.json

# Verify committed traces are still reproducible (structural; restores committed):
bash fixtures/verify_reproducible.sh picorv32_soc
```

CI guards both: `ci.yml` re-elaborates the golden and diffs it (deterministic),
and `nightly.yml` regenerates the traces with Verilator and checks structural
equivalence.

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
| Verilator | 5.x (apt `5.020` verified; nixpkgs-pinned via `flake.nix`) | nightly / local |

Byte-for-byte trace reproducibility requires a pinned Verilator — use
`nix develop .#verilator`. The apt package is the verified fallback and is
checked structurally (scope/var counts), not byte-for-byte.
