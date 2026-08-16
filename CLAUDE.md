# CLAUDE.md — hdl-schemview

> Orientation for Claude Code in this repo: what this is, where things live, which gates
> apply, and which doc to open next. Reference detail lives in `docs/` — this file routes
> you there rather than restating it.

## What this is

`hdl-schemview` is an **open, focused, RTL-level SystemVerilog cross-probe tool**. It
links three views and keeps them in sync — click a signal in any one and the others jump
to the matching object:

- **Source** — SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — generated, navigable diagram of the elaborated design.
- **Waveform** — sim traces (VCD/FST/GHW via `wellen`; user plugins out-of-process).

Not Verdi/Indago breadth — a focused tool composed from best-in-class parts
(`slang`/`pyslang` elaboration, `wellen` traces, `elkjs` layout).

**Governing principle:** the elaborated hierarchy is the single source of truth; source,
schematic and waveform are three _projections_ of it. Source and waveform map *to* the
model; the schematic *is* the model rendered. Cross-probing is lookups, not heuristics —
**never reintroduce guesswork or string-matching where a model lookup exists.**

## Where things live

Polyglot monorepo, three trees:

```
core/        Rust workspace — model, ingest, matching, cross-probe, schematic, GUI logic, CLI
app/         Tauri 2 desktop app — vanilla TS + Vite frontend + thin Rust shell
elaborate/   Python (pyslang) elaboration harness — produces the golden model JSON
fixtures/    Committed golden hierarchy + VCD/FST traces (picorv32_soc)
docs/        Roadmap, runbooks, ADRs
flake.nix    Nix flake: packages/apps.svxprobe, checks.{fmt,clippy,test}, overlays, dev shells
```

Data flow, end to end:

```
SystemVerilog RTL
  └─ elaborate/ (pyslang)          → hierarchy.json (schema-validated model document)
       └─ core/crates/ingest       → Design (deserialize + referential-integrity check)
            └─ core/crates/model    → indices: path_index, src_index, wave_index
                 ├─ schematic       → SchematicGraph (scope_graph / expand / cone / trace)
                 ├─ xprobe          → cross-probe resolution
                 └─ wave            → trace ValueChanges (wellen)
                      └─ gui        → Session + serializable DTOs (UI-toolkit-free, CI-testable)
                           └─ app/src-tauri/src/lib.rs  → #[tauri::command]s over sessions
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → CSS-grid panes + tab groups
```

### Which doc to open

| Working on… | Read |
| --- | --- |
| Rust crates, the Tauri command surface, how it all fits | [`docs/architecture.md`](docs/architecture.md) |
| Node model, `NodeKind`, schematic DTOs, the wire format | [`docs/data-model.md`](docs/data-model.md) |
| `app/src` internals — panes, bus, ELK, waveform, source | [`docs/frontend.md`](docs/frontend.md) |
| Running the app, launch flags, bundling | [`app/README.md`](app/README.md) |
| Harness flags (`--gate-level`, `--name-refs`, `--hls-*`) | [`elaborate/README.md`](elaborate/README.md) |
| Commands, PR gates, CI workflows | [`docs/development.md`](docs/development.md) + [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| Why something is the way it is | [`docs/decisions/`](docs/decisions/) (indexed) |
| Phase status, what's next | [`docs/ROADMAP.md`](docs/ROADMAP.md) |
| Scalability numbers | [`docs/benchmarking.md`](docs/benchmarking.md) |
| Fixtures, the matcher gate threshold | [`docs/fixtures.md`](docs/fixtures.md) |
| Cutting a release | [`docs/releasing.md`](docs/releasing.md) |

## Commands

Full gate block: [`CONTRIBUTING.md`](CONTRIBUTING.md). Command index:
[`docs/development.md`](docs/development.md). The loops you'll actually run:

```bash
# Rust (from core/) — the PR gates
cargo test --all
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo run --bin svxprobe -- match <model> <trace>   # >=95% hit-rate is a hard gate

# Frontend (from app/)
npm test              # Vitest — default env is node; DOM suites opt into happy-dom
npm run build         # tsc && vite build
npm run tauri dev     # Tauri window + Vite HMR

# Python harness (from elaborate/, uv-managed)
uv sync && uv run pytest -q
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o <out.json>

# Nix (from repo root)
nix develop           # verilator + the pinned rust + python311 + uv + jq
nix flake check       # mirrors the Rust gate through the flake
```

`core/` is a virtual workspace with several binaries, so `--bin svxprobe` is required —
a bare `cargo run` there cannot pick one.

## Workflow gates

- **Review before commit.** Never commit on the user's behalf without an explicit review
  pass. Before any `git commit`, show the diff (`git diff`/`git status`); when the change
  affects schematic/source/waveform views, also let the user verify it **visually**
  (`npm run tauri dev` / `npm run dev` or a screenshot). Wait for explicit go-ahead.
  "Implement X" is not standing approval to commit X.
- **Keep docs in sync in the same change.** After opening a PR (or landing a change that
  alters architecture, commands, DTOs, gates or workflow), update the relevant `docs/*`
  and this file. Treat doc updates as part of the PR, not a follow-up.
- **Label created issues.** Always attach existing labels (`gh label list`) — type
  (`bug`, `enhancement`/`feature`), area (`schematic`, `frontend`, `model`, `docs`), and
  an **effort** label (`effort/S|M|L` or `effort/xs…xl`, sized by files/layers touched).
  Create a label only when none fits; never leave an issue unlabeled.

## Conventions & gotchas

- **Rust toolchain pinned to 1.94** (`core/rust-toolchain.toml`) — match locally.
- **No heuristics.** Resolve via model indices; the elaborated hierarchy is the single
  source of truth.
- **DTO sync.** Rust serde DTOs (`gui`, `schematic`) ↔ [`app/src/types.ts`](app/src/types.ts)
  ↔ `elaborate/svxprobe_elaborate/schema/model.schema.json` must stay aligned. There is no compile-time link
  between them, so the TS layer desyncs **silently**. Details:
  [`docs/data-model.md`](docs/data-model.md).
- **Harness flags are additive.** Every opt-in flag leaves the default output
  byte-identical and `schema_version` at `1`. `elaborate_and_load` nonetheless always
  passes `--gate-level --name-refs`, because the frontend switches on data that must
  already be in the model.
- **TS is strict** (`strict`, `noUnusedLocals/Parameters`). No ESLint/Prettier — match
  existing style by hand.
- **Line endings.** A repo-wide `.gitattributes eol=lf` pins RTL sources and committed
  goldens to LF on every platform, because `def_range` offsets in the golden are LF-based.
  Regenerating a golden on a CRLF checkout breaks source highlighting.
- **Roadmap.** Phases 0–3e are done; **Phase 4 (scalability hardening) is active** and
  measurement-first. Status: [`docs/ROADMAP.md`](docs/ROADMAP.md). Measured numbers, and
  the two open Phase-4 decisions they gate (#22 demand-loading, #155 zero-copy read-back):
  [`docs/benchmarking.md`](docs/benchmarking.md) §Findings — **read it before scoping
  either issue**, since the headline finding is that 1M nodes load fully in ~1.1 GB, so
  #22's "too large to materialize" premise is not met at 1M and the real bottleneck is
  `cone()` under fan-out.

## Commit messages

Conventional Commits: `<type>: <imperative summary>`, one logical change per commit.
Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`. Examples:

```
feat: render inferred always_ff as a flip-flop symbol
fix: connect wires to the centre of pin triangles
docs: add architecture map and skill routing to CLAUDE.md
test: cover cone() depth limits in schematic crate
```

## Skills & agents

Reach for these by task type (invoke via `Skill` or the named agent). Process skills
(brainstorming, TDD, debugging, planning) come **first** — they decide *how* — then
domain skills guide execution.

| When working on… | Use |
| --- | --- |
| Exploring the codebase | `claude-mem:learn-codebase`, `claude-mem:smart-explore` |
| UI/design direction | `ecc:design-system`, `ecc:frontend-design` |
| Frontend TS patterns (`app/src/*`) | `ecc:frontend-patterns` |
| Reviewing TS/JS changes | `ecc:typescript-reviewer` (agent) + `/code-review` |
| Shaping an idea before building | `superpowers:brainstorming` |
| Features / bug fixes | `superpowers:test-driven-development`, `superpowers:systematic-debugging` |
| Planning multi-step work | `ecc:plan`, `superpowers:writing-plans` |
| Reviewing Rust changes (`core/crates/*`) | `ecc:rust-reviewer` (agent) |
| Idiomatic Rust | `ecc:rust-patterns` |

### Sub-agents (delegation)

Delegate via the `Agent` tool to keep the main thread focused and exploit this repo's
polyglot fan-out — you get the conclusion back, not the file dumps.

- **Explore broadly, then act.** For "where is X / how is Y wired" sweeps across
  `core/crates/*` + `app/src/*` + `elaborate/*`, dispatch an `Explore` (or
  `general-purpose`) agent; reserve direct reads for files you'll edit.
- **Fan out for DTO-sync changes.** When a change touches all three layers (Rust serde ↔
  `types.ts` ↔ `model.schema.json`), spin up one agent per layer **in a single message**
  to run them in parallel.
- **Review per language at the commit gate.** `ecc:rust-reviewer` (`core/crates/*`),
  `ecc:typescript-reviewer` (`app/src/*`), `ecc:python-reviewer` (`elaborate/*`) — in
  parallel when cross-cutting. Complements, never replaces, the user's review.
- **`fork` to preserve context** for sub-tasks needing the current conversation; a fresh
  `general-purpose` agent for self-contained work.
- **Don't double-run** a search you've already delegated.

## MCP Tools: code-review-graph

**IMPORTANT: This project has a knowledge graph. ALWAYS use the
code-review-graph MCP tools BEFORE using Grep/Glob/Read to explore
the codebase.** The graph is faster, cheaper (fewer tokens), and gives
you structural context (callers, dependents, test coverage) that file
scanning cannot.

### When to use graph tools FIRST

- **Exploring code**: `semantic_search_nodes` or `query_graph` instead of Grep
- **Understanding impact**: `get_impact_radius` instead of manually tracing imports
- **Code review**: `detect_changes` + `get_review_context` instead of reading entire files
- **Finding relationships**: `query_graph` with callers_of/callees_of/imports_of/tests_for
- **Architecture questions**: `get_architecture_overview` + `list_communities`

Fall back to Grep/Glob/Read **only** when the graph doesn't cover what you need.

### Key Tools

| Tool | Use when |
| ------ | ---------- |
| `detect_changes` | Reviewing code changes — gives risk-scored analysis |
| `get_review_context` | Need source snippets for review — token-efficient |
| `get_impact_radius` | Understanding blast radius of a change |
| `get_affected_flows` | Finding which execution paths are impacted |
| `query_graph` | Tracing callers, callees, imports, tests, dependencies |
| `semantic_search_nodes` | Finding functions/classes by name or keyword |
| `get_architecture_overview` | Understanding high-level codebase structure |
| `refactor_tool` | Planning renames, finding dead code |

### Workflow

1. The graph auto-updates on file changes (via hooks).
2. Use `detect_changes` for code review.
3. Use `get_affected_flows` to understand impact.
4. Use `query_graph` pattern="tests_for" to check coverage.
