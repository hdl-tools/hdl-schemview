# ibex_soc — tier-2 stress fixture (experimental)

The **real production core** used for local + nightly stress testing, per the
two-tier fixture policy (see [`../../docs/fixtures.md`](../../docs/fixtures.md)).
Unlike tier-1, this core is **not vendored** — it is fetched at a pinned ref to
keep the repo lean.

- **Core:** [lowRISC Ibex](https://github.com/lowRISC/ibex) — a heavily
  parameterized, package-and-generate-rich production RV32 core, far larger than
  the tier-1 PicoRV32 SoC.
- **Role:** never gates a PR. Runs only in `nightly.yml` (`continue-on-error`)
  and locally via these scripts.

## Scripts

```bash
bash fixtures/ibex_soc/fetch.sh       # clone Ibex at IBEX_REF into build/ (gitignored)
bash fixtures/ibex_soc/elaborate.sh   # run the pyslang harness over the core RTL
```

## Status & hardening tasks

This fixture is **provisioned but not yet hardened end-to-end** (the authoring
environment had no network access to github.com to validate the flow). Before
enabling tier-2 as a reliable signal:

1. **Pin `IBEX_REF`** in `fetch.sh` to a verified commit SHA (reproducibility).
2. **Confirm the RTL file list + include dirs** in `elaborate.sh` elaborate
   cleanly (Ibex pulls in `vendor/lowrisc_ip/.../prim` packages).
3. **Add a deterministic testbench + firmware** and wire `regen.sh`-style
   VCD/FST generation for the matcher stress test.
