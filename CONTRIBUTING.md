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

- **`core/`** — the Rust workspace (model, ingest, matcher, xprobe, schematic,
  gui, CLI).
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

# Matcher gate — if you touched the matcher or model (≥95% hit-rate is a hard gate)
cargo run --bin svxprobe -- match \
    ../fixtures/picorv32_soc/golden/hierarchy.json \
    ../fixtures/picorv32_soc/traces/picorv32_soc.fst \
    --excluded ../fixtures/picorv32_soc/excluded_scopes.txt

# Frontend (from app/) — if you touched app/
npm run build
npm test

# Python harness (from elaborate/) — if you touched elaborate/
uv run pytest -q
```

**DTO sync:** the Rust serde DTOs in the `gui` and `schematic` crates are the
wire format for the frontend. If you change any of them, mirror the change in
[`app/src/types.ts`](app/src/types.ts) or the TS layer silently desyncs.

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
