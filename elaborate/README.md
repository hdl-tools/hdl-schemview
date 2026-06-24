# svxprobe-elaborate

The elaboration harness: walks a [slang](https://github.com/MikePopoloski/slang)-elaborated
SystemVerilog design via **pyslang** and serializes the structural spine of the
hierarchy to the Node-model JSON consumed by the Rust core
(`core/`). The JSON contract is `schema/model.schema.json`.

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

### With pip (fallback)

```bash
cd elaborate
python3 -m venv .venv && . .venv/bin/activate
pip install -e '.[dev]'   # or: pip install -e .
pytest -q
```

## Usage

```bash
# Emit the Node-model JSON for a design (pin --top; many files declare >1 module)
uv run svxprobe-elaborate --top picorv32_soc \
    ../fixtures/picorv32_soc/rtl/*.sv -o /tmp/hierarchy.json

# …or drive it from EDA-style filelists (repeatable -f; supports nested -f,
# +incdir+DIR, -I DIR; paths resolve relative to each filelist's own dir):
uv run svxprobe-elaborate --top picorv32_soc \
    -f ../fixtures/picorv32_soc/picorv32_soc.f -o /tmp/hierarchy.json

# Add include directories directly:
uv run svxprobe-elaborate --top core -I ../rtl/include rtl/*.sv -o /tmp/h.json

# Validate any model document against the schema
uv run python -m svxprobe_elaborate.validate /tmp/hierarchy.json
```

## What it emits (Phase 0)

The **structural spine**: `Instance`, `GenBlock`, `Net`, `Port`, `Var` nodes,
each with a canonical `path`, source `def_range`/`inst_range`, and a stable
`symbol_key`. Connectivity (`drivers`/`loads`) and the waveform index are Phase 1.
