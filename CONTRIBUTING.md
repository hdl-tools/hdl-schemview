# Contributing to hdl-schemview

Thanks for your interest in contributing! `hdl-schemview` is an open, focused,
RTL-level SystemVerilog cross-probe tool. This guide ties together the issue and
PR templates and points at the gates your change needs to clear.

## Ways to contribute

- **Report a bug** — open an issue with the
  [Bug report](.github/ISSUE_TEMPLATE/bug_report.yml) form.
- **Request a feature** — open an issue with the
  [Feature request](.github/ISSUE_TEMPLATE/feature_request.yml) form. Check
  [`docs/ROADMAP.md`](docs/ROADMAP.md) first — it may already be planned.
- **Open a pull request** — the [PR template](.github/PULL_REQUEST_TEMPLATE.md)
  prefills automatically; fill in every section.
- **Ask a question** — use
  [Discussions](https://github.com/chuanseng-ng/hdl-schemview/discussions).

## Project shape

A polyglot monorepo with three trees:

- **`core/`** — the Rust workspace (model, ingest, wave, matcher, xprobe,
  schematic, gui, the `svxprobe` CLI, and the dev-only `scale-bench`).
- **`elaborate/`** — the Python (pyslang) elaboration harness that emits the
  golden model JSON.
- **`app/`** — the Tauri 2 desktop app (vanilla TS + Vite frontend over a thin
  Rust shell).

**The governing principle:** the elaborated hierarchy is the single source of
truth, and source / schematic / waveform are three *projections* of it.
Cross-probing is lookups, not heuristics — please don't reintroduce
string-matching where a model lookup exists. See [`CLAUDE.md`](CLAUDE.md) and
[`docs/decisions/`](docs/decisions) for the full architecture.

## Dev setup

Setup is documented in the [README](README.md#development-setup) — either the
Nix dev shell (most reproducible) or per-language tooling (Cargo, `uv`,
Verilator). Fixtures are committed under `fixtures/picorv32_soc/`, so you don't
need to regenerate traces for normal work (see [`docs/fixtures.md`](docs/fixtures.md)).

## Before opening a PR

Run the gates that apply to your change:

```bash
# Rust (from core/)
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all

# Matcher gate — if you touched the matcher or model (≥95% hit-rate is a hard gate).
# CI runs BOTH formats; run both locally, since a rule can pass one and fail the other:
cargo run --bin svxprobe -- match \
    ../fixtures/picorv32_soc/golden/hierarchy.json \
    ../fixtures/picorv32_soc/traces/picorv32_soc.fst \
    --excluded ../fixtures/picorv32_soc/excluded_scopes.txt
cargo run --bin svxprobe -- match \
    ../fixtures/picorv32_soc/golden/hierarchy.json \
    ../fixtures/picorv32_soc/traces/picorv32_soc.vcd \
    --excluded ../fixtures/picorv32_soc/excluded_scopes.txt

# Frontend (from app/) — if you touched app/. Both are enforced by app.yml.
npm run build
npm test

# Python harness (from elaborate/) — if you touched elaborate/
uv run ruff check .
uv run pytest -q
uv run python -m svxprobe_elaborate.validate ../fixtures/picorv32_soc/golden/hierarchy.json
# RTL lint gate — illegal always_ff driver combinations (VCS rejects what slang and
# Verilator accept, IEEE 1800 §9.2.2.4), so CI checks it explicitly. From the repo
# root, explicit file list: a *.sv glob would skip picorv32.v (and not expand at all
# in PowerShell):
cd .. && uv run --project elaborate python -m svxprobe_elaborate.lint --top picorv32_soc \
    fixtures/picorv32_soc/rtl/picorv32.v \
    fixtures/picorv32_soc/rtl/soc_pkg.sv \
    fixtures/picorv32_soc/rtl/mem_if.sv \
    fixtures/picorv32_soc/rtl/soc_mem.sv \
    fixtures/picorv32_soc/rtl/picorv32_soc.sv

# Golden reproducibility (from the repo root) — if you touched elaborate/.
# CI re-elaborates the golden and diffs it, so a harness change that alters the
# output fails the build until the golden is regenerated and committed. BOTH
# opt-in flags are required, or the diff reports a stale golden:
uv run --project elaborate svxprobe-elaborate --top picorv32_soc \
    fixtures/picorv32_soc/rtl/picorv32.v \
    fixtures/picorv32_soc/rtl/soc_pkg.sv \
    fixtures/picorv32_soc/rtl/mem_if.sv \
    fixtures/picorv32_soc/rtl/soc_mem.sv \
    fixtures/picorv32_soc/rtl/picorv32_soc.sv \
    --gate-level --name-refs -o /tmp/golden_regen.json
diff <(jq -S . fixtures/picorv32_soc/golden/hierarchy.json) \
     <(jq -S . /tmp/golden_regen.json)
```

**DTO sync:** the Rust serde DTOs in the `gui` and `schematic` crates are the
wire format for the frontend. If you change any of them, mirror the change in
[`app/src/types.ts`](app/src/types.ts) or the TS layer silently desyncs. A model-level
change usually needs a third edit, in
[`elaborate/schema/model.schema.json`](elaborate/schema/model.schema.json).

**What runs where:** `ci.yml` (every push/PR) covers the Rust gates, the matcher on both
trace formats, the Python lint/test/schema gates, the RTL `always_ff` lint, and golden
reproducibility. `app.yml` builds the Tauri app on Ubuntu + Windows and runs `npm test` +
`npm run build` when `app/` or `core/crates/` change. `nightly.yml` has three jobs —
`repro-tier1` (regenerate traces with Verilator), `stress-tier2` (Ibex, `continue-on-error`),
and `scale-bench` (the scalability benchmark, `continue-on-error`, uploads a metrics
artifact). None of the nightly jobs gate a PR.

**Docs are part of the change, not a follow-up.** If your PR alters architecture, commands,
DTOs, gates, or workflow, update `CLAUDE.md` and the relevant `docs/*` in the same PR. A
decision with lasting consequences gets an ADR in [`docs/decisions/`](docs/decisions).

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/): a `type:`
prefix and a concise, imperative summary. The full type table lives in
[`CLAUDE.md`](CLAUDE.md#commit-messages).

```
feat: render inferred always_ff as a flip-flop symbol
fix: connect wires to the centre of pin triangles
```

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
