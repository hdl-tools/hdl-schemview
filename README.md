# hdl-schemview

**An open, focused, RTL-level SystemVerilog cross-probe tool.**

`hdl-schemview` links three views of a digital design and keeps them in sync:

- **Source** — the SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — a generated, navigable diagram of the elaborated design.
- **Waveform** — simulation traces (VCD/FST, with user-supplied readers for others).

Click a signal in any view and the other two jump to the corresponding object.
The commercial equivalents are Synopsys Verdi and Cadence Indago; we are **not**
matching their breadth. We are building a focused, open, RTL-level tool by
composing existing best-in-class components.

## The four goals

1. **Full SystemVerilog elaboration** — via [slang](https://github.com/MikePopoloski/slang) (IEEE 1800-2023, through elaboration).
2. **Scalable visualization** — hierarchical, on-demand schematics that survive a real SoC.
3. **Accurate source / schematic / waveform cross-probing** — lookups against one elaborated model, not heuristics.
4. **VCD & FST support plus user-brought plugins** — built-in [wellen](https://github.com/ekiwi/wellen); user readers (e.g. FSDB) load out-of-process so no proprietary bits ship.

## The one principle that governs the design

**The elaborated hierarchy is the single source of truth. Source, schematic, and
waveform are three *projections* of it.** Source and waveform get bidirectional
maps *to* the elaborated model; the schematic *is* the elaborated model rendered.
Get this right and cross-probing is lookups, not guesswork.

## Status

**Phase 1 gate PASSED — the project is GO.** The execution plan lives in
**[docs/ROADMAP.md](docs/ROADMAP.md)**; architecture decisions are ADRs in
**[docs/decisions/](docs/decisions/)**; the reference fixtures and the pinned gate
threshold are documented in **[docs/fixtures.md](docs/fixtures.md)**.

What exists today:

- `elaborate/` — the **pyslang** harness that elaborates SystemVerilog and emits
  the Node-model JSON (`schema/model.schema.json`), including parameters.
- `core/` — the **Rust** workspace: `model` (Node model + indices), `ingest`
  (deserialize the harness JSON), `wave` (waveform access via **wellen**),
  `matcher` (the canonical-path matcher + hit-rate report), and the `svxprobe` CLI.
- `fixtures/picorv32_soc/` — the tier-1 reference fixture (PicoRV32 + a SystemVerilog
  wrapper exercising package / interface / parameterized-instance / generate), with
  frozen Verilator **FST + VCD** traces and a golden hierarchy.

The project go/no-go was the **Phase 1 matcher gate**: on the frozen fixture,
≥ 95% of design-scope signals matched with zero mystery misses, against both FST
and VCD. The matcher clears it at **100% on both formats** (DUT anchor
auto-detected), enforced in CI:

```bash
cd core
cargo run --bin svxprobe -- match \
    ../fixtures/picorv32_soc/golden/hierarchy.json \
    ../fixtures/picorv32_soc/traces/picorv32_soc.fst \
    --excluded ../fixtures/picorv32_soc/excluded_scopes.txt
```

## Development setup

The project is polyglot: a Rust core, a Python (pyslang) elaboration harness, and
Verilator for generating fixture traces.

### Option A — Nix (most reproducible)

A flake provides a dev shell with a pinned Verilator plus the Rust and Python
toolchains:

```bash
nix develop            # full shell: verilator + rust + python + uv + jq
nix develop .#verilator  # just a pinned Verilator (for trace regen)
```

### Option B — per-language tools

**Rust core** (toolchain pinned by `core/rust-toolchain.toml`):

```bash
cd core
cargo test --all     # builds model/ingest/wave/cli; ingests the golden; loads both traces
cargo run --bin svxprobe -- ingest ../fixtures/picorv32_soc/golden/hierarchy.json
cargo run --bin svxprobe -- wave   ../fixtures/picorv32_soc/traces/picorv32_soc.fst
```

**Python harness** — with [uv](https://docs.astral.sh/uv/) (recommended; locked via
`elaborate/uv.lock`):

```bash
cd elaborate
uv sync                # create .venv from pinned deps
uv run pytest -q
uv run svxprobe-elaborate --top picorv32_soc -f ../fixtures/picorv32_soc/picorv32_soc.f -o /tmp/h.json
```

…or with pip as a fallback:

```bash
cd elaborate
python3 -m venv .venv && . .venv/bin/activate
pip install -e '.[dev]'
pytest -q
```

**Verilator** (for regenerating fixture traces) — via Nix (pinned, above) or apt
(`sudo apt-get install -y verilator`, 5.x):

```bash
bash fixtures/regen.sh picorv32_soc              # regenerate VCD + FST
bash fixtures/verify_reproducible.sh picorv32_soc # check committed traces still reproduce
```

See **[docs/fixtures.md](docs/fixtures.md)** for the two-tier fixture policy and
the full list of pinned tool versions.

## License

MIT — see [LICENSE](LICENSE).
