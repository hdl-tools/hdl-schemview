# CLAUDE.md — hdl-schemview

> Guidance for Claude Code when working in this repository. Read this first; it
> captures the architecture, commands, conventions, and which skills to reach for.

## What this is

`hdl-schemview` is an **open, focused, RTL-level SystemVerilog cross-probe tool**.
It links three views of a digital design and keeps them in sync:

- **Source** — the SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — a generated, navigable diagram of the elaborated design.
- **Waveform** — simulation traces (VCD/FST via `wellen`; user plugins out-of-process).

Click a signal in any view and the other two jump to the corresponding object.
Not trying to match Verdi/Indago breadth — a focused, open tool composed from
best-in-class components (`slang`/`pyslang` for elaboration, `wellen` for traces,
`elkjs` for layout).

### The governing principle

**The elaborated hierarchy is the single source of truth. Source, schematic, and
waveform are three _projections_ of it.** Source and waveform get bidirectional
maps *to* the elaborated model; the schematic *is* the elaborated model rendered.
Get this right and cross-probing is lookups, not heuristics. Preserve this
invariant in any change — do not reintroduce guesswork/string-matching where a
model lookup exists.

## Architecture map

Polyglot monorepo with three trees:

```
core/        Rust workspace — model, ingest, matching, cross-probe, schematic, GUI logic, CLI
app/         Tauri 2 desktop app — vanilla TS + Vite frontend + thin Rust shell
elaborate/   Python (pyslang) elaboration harness — produces the golden model JSON
fixtures/    Committed golden hierarchy + VCD/FST traces (picorv32_soc)
docs/        ROADMAP, fixtures policy, ADRs (docs/decisions/*)
```

### Data flow (end to end)

```
SystemVerilog RTL
  └─ elaborate/ (pyslang harness)      → hierarchy.json (model document, schema-validated)
       └─ core/crates/ingest           → Design (deserialize + referential-integrity check)
            └─ core/crates/model        → indices: path_index, src_index (interval tree), wave_index
                 ├─ core/crates/schematic  → SchematicGraph (scope_graph / expand / cone)
                 ├─ core/crates/xprobe      → cross-probe resolution
                 └─ core/crates/wave        → trace ValueChanges (wellen)
                      └─ core/crates/gui     → Session + serializable DTOs (UI-toolkit-free, CI-testable)
                           └─ app/src-tauri/src/lib.rs  → 9 #[tauri::command]s over Mutex<Session>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → 3 panes: schematic (SVG via elk.ts), source, waveform (canvas)
```

### Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

| Crate | Package | Purpose |
| --- | --- | --- |
| `model` | `svxprobe-model` | Elaborated node model + indices (`path_index`, `src_index` interval tree, `wave_index`). Spine of the tool. |
| `ingest` | `svxprobe-ingest` | JSON → `Design` deserialization + referential-integrity validation. |
| `wave` | `svxprobe-wave` | VCD/FST/GHW trace loader via `wellen` (lazy per-signal). |
| `matcher` | `svxprobe-matcher` | Phase-1 canonical-path matcher. **≥95% hit-rate is a hard PR gate.** |
| `xprobe` | `svxprobe-xprobe` | Cross-probe engine: resolves source ↔ waveform ↔ schematic selections. |
| `schematic` | `svxprobe-schematic` | Layout-agnostic graph extractor: `scope_graph()`, `expand()`, `cone()`. |
| `gui` | `svxprobe-gui` | `Session` logic + serializable DTOs. No UI toolkit — fully CI-testable. |
| `cli` | `svxprobe` | Dev/test binary. Subcommands: `ingest`, `wave`, `match`, `graph`, `probe`. |

The Tauri shell (`app/src-tauri/`, package `hdl-schemview-app`) is a thin
`cdylib`/`lib` that wraps `svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

### Frontend (`app/`, vanilla TS + Vite 5 + Vitest, no UI framework)

| File | Role |
| --- | --- |
| `app/index.html` | Entry; three-pane layout + toolbar/breadcrumb. |
| `app/src/main.ts` | UI logic + app state (graph, nav stack, selection, source cache). |
| `app/src/api.ts` | Typed wrappers over Tauri `invoke()`. |
| `app/src/types.ts` | DTO interfaces mirroring the Rust serde types. |
| `app/src/elk.ts` (+ `elk.test.ts`) | `SchematicGraph` → ELK layout → SVG DOM. |
| `app/src/style.css` | Theme CSS vars. Dark is default; light via `:root[data-theme="light"]`, persisted in `localStorage`. |

Deps: `@tauri-apps/api`, `elkjs`. Schematic = SVG; waveform = canvas 2D.

## Tauri command reference (`app/src-tauri/src/lib.rs` ↔ `app/src/api.ts`)

| Command | Args | Returns |
| --- | --- | --- |
| `load_design` | `model, trace, excluded[], srcRoot` | `String` (top scope) |
| `scope_graph` | `scope` | `SchematicGraph` |
| `expand_node` | `node` (id) | `SchematicGraph` |
| `cone` | `net` (id), `dir`, `depth` | `SchematicGraph` |
| `probe_node` | `path, context?` | `ProbeResponse \| null` |
| `probe_signal` | `fullName, context?` | `ProbeResponse \| null` |
| `probe_source` | `file` (id), `offset, context?` | `ProbeResponse \| null` |
| `signal_values` | `signalRef` | `ValueChange[]` |
| `source_text` | `file` (id) | `String` |

Commands delegate to a global `AppState(Mutex<Session>)`.

## Key data structures

**Schematic** (`core/crates/schematic/src/lib.rs`):
- `SchematicGraph { root: String, nodes: Vec<SchNode>, edges: Vec<SchEdge> }`
- `SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>, module: Option<String> }`
- `SchPort { id, name, side: Side, width: Option<String> }` — `width` like `[31:0]` or `None` for scalar.
- `SchEdge { id, source, target, net: Option<String> }`
- `Side { West, East }` — drives ELK port placement.

**Model** (`core/crates/model/src/lib.rs`):
- `NodeId` = `u32` index into `Document::nodes`.
- `NodeKind { Instance, Net, Port, Var, Param, ModuleDef, GenBlock }`.
- `Node { id, name, path, parent, children, kind, symbol_key, def_range, inst_range, type_, drivers, loads }`.
- `Design { doc, path_index, src_index, conn_index, wave_index }`.

> ⚠️ These serde types are the wire format for the frontend. Any field change in
> `gui`/`schematic` DTOs must be mirrored in `app/src/types.ts`, or the TS layer
> silently desyncs.

## Build, test & PR gates

**Rust** (run from `core/`):
```bash
cargo test --all                              # unit + integration (uses committed fixtures)
cargo fmt --all --check                       # PR gate
cargo clippy --all-targets -- -D warnings     # PR gate
cargo run --bin svxprobe -- match <model> <trace>   # Phase-1 cross-probe gate (≥95% hit-rate)
```

**Frontend** (run from `app/`):
```bash
npm install
npm run dev          # Vite dev server (http://localhost:5173)
npm run build        # tsc && vite build → dist/
npm test             # Vitest (e.g. elk.test.ts)
npm run tauri dev    # Tauri window + Vite HMR
npm run tauri build  # Bundle desktop app (Win/Linux/macOS)
```

**Python harness** (run from `elaborate/`, `uv`-managed):
```bash
uv sync
uv run pytest -q
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o <out.json>
```

Fixtures live at `fixtures/picorv32_soc/` (committed golden + VCD/FST). PR-gate
tests run against them — **no Verilator regeneration needed** for normal work.
See `docs/fixtures.md` for the two-tier policy and pinned tool versions.

## CI

- `.github/workflows/ci.yml` — Rust (fmt, clippy, test, match gate on FST+VCD) + Python (lint, test, schema validation, golden reproducibility). Ubuntu, on push/PR.
- `.github/workflows/app.yml` — cross-platform Tauri build (Ubuntu + Windows matrix). Triggers when `app/` or `core/crates/` change.
- Nightly — Verilator trace regeneration.

## Workflow gates

- **Review before commit** — never commit on the user's behalf without an explicit
  review pass first. Before any `git commit`, surface the local changes for the user
  to review: show the diff (`git diff`/`git status`) for code review, *and* — when the
  change affects the schematic, source, or waveform views — let the user verify it
  **visually** in the running app (`npm run tauri dev` / `npm run dev`) or via a
  screenshot. Wait for the user's explicit go-ahead before committing. This applies
  even when the user asked for the feature; "implement X" is not standing approval to
  commit X.
- **Label created issues** — whenever you file a GitHub issue, automatically attach
  appropriate labels (e.g. `bug`, `enhancement`/`feature`, `schematic`, `frontend`,
  `model`, `docs`, area/severity tags). Pick labels that already exist in the repo
  (`gh label list`); only create a new label when no existing one fits, and prefer the
  conventional-commit-aligned categories above. Never leave a new issue unlabeled.
  Where feasible, also attach an **effort label** estimating the work involved
  (e.g. `effort/S`, `effort/M`, `effort/L` — or `effort/xs`…`effort/xl`). Create the
  effort label set once if it doesn't exist yet, keep the scale consistent across
  issues, and base the estimate on scope (files/layers touched, fixture regeneration,
  cross-layer changes) rather than guesswork.

## Conventions & gotchas

- **Rust toolchain pinned to 1.94** (`core/rust-toolchain.toml`) — match it locally.
- **DTO sync** — Rust serde DTOs (`gui`, `schematic`) ↔ `app/src/types.ts` must stay aligned.
- **TS is strict** (`tsconfig.json`: strict, `noUnusedLocals/Parameters`). No ESLint/Prettier configured — match existing style by hand.
- **Don't reintroduce heuristics** — resolve via model indices (the single-source-of-truth principle).
- **Roadmap context** — Phase 0–2 = model/matcher/cross-probe; Phase 3 = schematic extractor + Tauri app (current active area). See `docs/ROADMAP.md`.

## Commit messages

Follow Conventional Commits: a `type:` prefix, then a concise, imperative
summary. Keep the subject focused on a single logical change.

```
<type>: <imperative summary>
```

| Type | Use for |
| --- | --- |
| `feat` | New feature for the user (not a new build-script feature). |
| `fix` | Bug fix for the user (not a build-script fix). |
| `docs` | Documentation-only changes. |
| `style` | Formatting, missing semicolons, etc.; no production-code change. |
| `refactor` | Refactoring production code, e.g. renaming a variable. |
| `test` | Adding or refactoring tests; no production-code change. |
| `chore` | Build tasks, tooling, deps, etc.; no production-code change. |

Examples in this repo's context:

```
feat: render inferred always_ff as a flip-flop symbol
fix: connect wires to the centre of pin triangles
docs: add architecture map and skill routing to CLAUDE.md
style: apply cargo fmt to schematic crate
refactor: extract scope-graph builder from Session
test: cover cone() depth limits in schematic crate
chore: bump Rust toolchain pin to 1.94
```

## Skills to use

Reach for these installed skills/agents by task type (invoke via the `Skill` tool
or the named agent):

| When working on… | Use |
| --- | --- |
| Getting oriented / exploring the codebase | `claude-mem:learn-codebase`, `claude-mem:smart-explore` |
| UI/design direction & visual consistency | `ecc:design-system`, `ecc:frontend-design` |
| Frontend TS architecture/patterns (`app/src/*`) | `ecc:frontend-patterns` |
| Reviewing TS/JS changes | `ecc:typescript-reviewer` (agent) + `ecc:code-review` (or built-in `/code-review`) |
| Shaping an idea before building | `superpowers:brainstorming` |
| Writing features / fixing bugs | `superpowers:test-driven-development`, `superpowers:systematic-debugging` |
| Planning multi-step work | `ecc:plan`, `superpowers:writing-plans` |
| Reviewing Rust changes (`core/crates/*`) | `ecc:rust-reviewer` (agent) |
| Writing/refactoring idiomatic Rust | `ecc:rust-patterns` |

Process skills (brainstorming, TDD, debugging, planning) come **first** — they
decide *how* to approach the work — then the domain skills guide execution.

