# svxprobe-elaborate

The elaboration harness: walks a [slang](https://github.com/MikePopoloski/slang)-elaborated
SystemVerilog design via **pyslang** and serializes the structural spine of the
hierarchy to the Node-model JSON consumed by the Rust core
(`core/`). The JSON contract is `svxprobe_elaborate/schema/model.schema.json`.

This is the out-of-process elaboration boundary from the
[roadmap](../docs/ROADMAP.md) §8: no FFI, just a serialized model.

## Setup

### With uv (recommended)

[uv](https://docs.astral.sh/uv/) gives a locked, reproducible environment from
`pyproject.toml` + `uv.lock`:

```bash
cd elaborate
uv sync            # create .venv and install pinned deps (incl. dev tools)
uv run pytest -q   # run the harness tests
```

### With Nix

`nix develop` from the repo root already carries the harness — `pyslang` included,
built from source, no PyPI fetch (#280). `svxprobe-elaborate` is on `PATH`, and
`PYTHONPATH` points at this directory, so `python -m svxprobe_elaborate.<mod>` runs
your **working tree** while the `svxprobe-elaborate` binary is the fixed store build.

```bash
nix develop                        # from the repo root
nix build .#svxprobe-elaborate     # the package on its own (runs the test suite)
nix run   .#svxprobe-elaborate -- --help
```

### With pip (fallback)

```bash
cd elaborate
python3 -m venv .venv && . .venv/bin/activate
pip install -e .
pip install pytest ruff   # dev tools live in PEP 735 [dependency-groups], which is
                          # not an extra — `.[dev]` does not exist
pytest -q
```

## Usage

```bash
# Emit the Node-model JSON for a design (pin --top; many files declare >1 module).
# List files explicitly rather than globbing — a *.sv glob skips picorv32.v, and
# globs do not expand in PowerShell:
uv run svxprobe-elaborate --top picorv32_soc \
    ../fixtures/picorv32_soc/rtl/picorv32.v \
    ../fixtures/picorv32_soc/rtl/soc_pkg.sv \
    ../fixtures/picorv32_soc/rtl/mem_if.sv \
    ../fixtures/picorv32_soc/rtl/soc_mem.sv \
    ../fixtures/picorv32_soc/rtl/picorv32_soc.sv -o /tmp/hierarchy.json

# …or drive it from EDA-style filelists (repeatable -f; supports nested -f,
# +incdir+DIR, -I DIR; paths resolve relative to each filelist's own dir):
uv run svxprobe-elaborate --top picorv32_soc \
    -f ../fixtures/picorv32_soc/picorv32_soc.f -o /tmp/hierarchy.json

# Add include directories directly:
uv run svxprobe-elaborate --top core -I ../rtl/include rtl/top.sv -o /tmp/h.json

# Validate any model document against the schema
uv run python -m svxprobe_elaborate.validate /tmp/hierarchy.json

# Lint RTL for illegal always_ff driver combinations (a CI gate — VCS rejects a
# variable written by always_ff that has any other procedural driver, IEEE 1800
# §9.2.2.4, while slang and Verilator accept it):
uv run python -m svxprobe_elaborate.lint --top picorv32_soc <files…>
```

### Options

| Flag | Meaning |
| --- | --- |
| `files…` (positional) | SystemVerilog sources. Combine freely with `-f`. |
| `--top <name>` | Top module. Recommended — most file sets declare >1 module. |
| `-f`, `--filelist <f>` | EDA-style filelist (repeatable; nested `-f`, `+incdir+DIR`, `-I DIR`). |
| `-I`, `--include <dir>` | Include directory (repeatable). |
| `-o`, `--out <path>` | Output path. Defaults to `-` (stdout). |
| `--gate-level` | Decompose process/assign expressions into gate/mux primitive nodes (#157, [ADR 0005](../docs/decisions/0005-optional-gate-level-projection.md)). |
| `--name-refs` | Emit identifier-occurrence spans for semantic source coloring and usage-click resolution (#225). |
| `--hls-map` | Scan the generated RTL's `// foo.cpp:42` provenance comments and emit the C↔RTL `source_map` (#159, [ADR 0006](../docs/decisions/0006-hls-cpp-rtl-source-tracing.md)). |
| `--hls-comment-re <REGEX>` | Override the provenance comment pattern (named groups `file` + `line`). Only with `--hls-map`. |
| `--hls-src <path>` | Declare a C/C++ source *file* (registered even if no comment references it) or a *directory* (used as a search root). Repeatable; only with `--hls-map` (#222). |

Every flag past `-o` is **opt-in and additive**: `schema_version` stays `1` and the
default output is byte-identical without them.

### Reproducing the committed golden

`ci.yml` regenerates `fixtures/picorv32_soc/golden/hierarchy.json` and diffs it, so
the golden must be produced with **both** `--gate-level` and `--name-refs`. Run from
the repo root:

```bash
uv run --project elaborate svxprobe-elaborate --top picorv32_soc \
    fixtures/picorv32_soc/rtl/picorv32.v \
    fixtures/picorv32_soc/rtl/soc_pkg.sv \
    fixtures/picorv32_soc/rtl/mem_if.sv \
    fixtures/picorv32_soc/rtl/soc_mem.sv \
    fixtures/picorv32_soc/rtl/picorv32_soc.sv \
    --gate-level --name-refs \
    -o fixtures/picorv32_soc/golden/hierarchy.json
```

See [`docs/fixtures.md`](../docs/fixtures.md) for the fixture policy and the LF
line-ending requirement (#203).

## What it emits

The **structural spine** the Rust core indexes: `Instance`, `GenBlock`, `Net`,
`Port`, `Var`, `Param`, `Interface`/`Modport`, and `Memory` nodes, each with a
canonical `path`, source `def_range`/`inst_range`, and a stable `symbol_key`;
`edges` for connectivity; and the inferred process nodes (`Ff`, `Comb`, `Latch`,
`Assign`) that back the schematic's internal-logic drill-down.

Alongside those, depending on the opt-in flags above:

- **Gate/mux primitives** (`--gate-level`) — `And`…`Xnor`, `Not`/`Buf`, the
  datapath ops (`Add`/`Sub`/`Mul`/`Cmp`/`Shift`), `Mux`, `Const`, `Concat`, with
  `mux_port` roles on the mux inputs.
- **`source_map` + per-file `language`** (`--hls-map`) — the bidirectional C↔RTL
  line-region provenance map.
- **`name_refs`** (`--name-refs`) — every identifier occurrence, classified off the
  symbol the elaboration resolved.

An enum table (`enums`) is emitted whenever the design declares one; it backs the
waveform pane's FSM state names.
