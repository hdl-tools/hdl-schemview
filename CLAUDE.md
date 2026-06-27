# CLAUDE.md — hdl-schemview

> Guidance for Claude Code in this repo. Read first: architecture, commands,
> conventions, and which skills to reach for.

## What this is

`hdl-schemview` is an **open, focused, RTL-level SystemVerilog cross-probe tool**.
It links three views and keeps them in sync — click a signal in any one and the
others jump to the matching object:

- **Source** — SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — generated, navigable diagram of the elaborated design.
- **Waveform** — sim traces (VCD/FST via `wellen`; user plugins out-of-process).

Not Verdi/Indago breadth — a focused tool composed from best-in-class parts
(`slang`/`pyslang` elaboration, `wellen` traces, `elkjs` layout).

**Governing principle:** the elaborated hierarchy is the single source of truth;
source, schematic, and waveform are three _projections_ of it. Source/waveform map
*to* the model; the schematic *is* the model rendered. Cross-probing is lookups,
not heuristics — **never reintroduce guesswork/string-matching where a model lookup
exists.**

## Architecture map

Polyglot monorepo, three trees:

```
core/        Rust workspace — model, ingest, matching, cross-probe, schematic, GUI logic, CLI
app/         Tauri 2 desktop app — vanilla TS + Vite frontend + thin Rust shell
elaborate/   Python (pyslang) elaboration harness — produces the golden model JSON
fixtures/    Committed golden hierarchy + VCD/FST traces (picorv32_soc)
docs/        ROADMAP, fixtures policy, ADRs (docs/decisions/*)
```

Data flow (end to end):

```
SystemVerilog RTL
  └─ elaborate/ (pyslang)          → hierarchy.json (schema-validated model document)
       └─ core/crates/ingest       → Design (deserialize + referential-integrity check)
            └─ core/crates/model    → indices: path_index, src_index (interval tree), wave_index
                 ├─ schematic       → SchematicGraph (scope_graph / expand / cone)
                 ├─ xprobe          → cross-probe resolution
                 └─ wave            → trace ValueChanges (wellen)
                      └─ gui        → Session + serializable DTOs (UI-toolkit-free, CI-testable)
                           └─ app/src-tauri/src/lib.rs  → 9 #[tauri::command]s over Mutex<Session>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → 3 panes: schematic (SVG/elk.ts), source, waveform (canvas)
```

### Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

| Crate | Package | Purpose |
| --- | --- | --- |
| `model` | `svxprobe-model` | Elaborated node model + indices (`path_index`, `src_index` interval tree, `wave_index`). The spine. |
| `ingest` | `svxprobe-ingest` | JSON → `Design` deserialization + referential-integrity validation. |
| `wave` | `svxprobe-wave` | VCD/FST/GHW trace loader via `wellen` (lazy per-signal). |
| `matcher` | `svxprobe-matcher` | Phase-1 canonical-path matcher. **≥95% hit-rate is a hard PR gate.** |
| `xprobe` | `svxprobe-xprobe` | Cross-probe engine: source ↔ waveform ↔ schematic. |
| `schematic` | `svxprobe-schematic` | Layout-agnostic graph extractor: `scope_graph()`, `expand()`, `cone()`. |
| `gui` | `svxprobe-gui` | `Session` logic + serializable DTOs. No UI toolkit — CI-testable. |
| `cli` | `svxprobe` | Dev/test binary. Subcommands: `ingest`, `wave`, `match`, `graph`, `probe`. |

Tauri shell (`app/src-tauri/`, package `hdl-schemview-app`) is a thin `cdylib`/`lib`
wrapping `svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

### Frontend (`app/`, vanilla TS + Vite 5 + Vitest, no UI framework)

| File | Role |
| --- | --- |
| `app/index.html` | Three-pane layout + toolbar/breadcrumb. |
| `app/src/main.ts` | UI logic + app state (graph, nav stack, selection, source cache). |
| `app/src/api.ts` | Typed wrappers over Tauri `invoke()`. |
| `app/src/types.ts` | DTO interfaces mirroring Rust serde types. |
| `app/src/elk.ts` (+ `elk.test.ts`) | `SchematicGraph` → ELK layout → SVG DOM. |
| `app/src/style.css` | Theme vars. Dark default; light via `:root[data-theme="light"]`, persisted in `localStorage`. |

Deps: `@tauri-apps/api`, `elkjs`. Schematic = SVG; waveform = canvas 2D.

## Tauri commands (`app/src-tauri/src/lib.rs` ↔ `app/src/api.ts`)

Delegate to a global `AppState(Mutex<Session>)`.

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

## Key data structures

**Schematic** (`core/crates/schematic/src/lib.rs`):
- `SchematicGraph { root, nodes: Vec<SchNode>, edges: Vec<SchEdge> }`
- `SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>, module: Option<String> }`
- `SchPort { id, name, side: Side, width: Option<String> }` — `width` like `[31:0]`, else `None`.
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

```bash
# Rust (from core/)
cargo test --all                                    # unit + integration (committed fixtures)
cargo fmt --all --check                             # PR gate
cargo clippy --all-targets -- -D warnings           # PR gate
cargo run --bin svxprobe -- match <model> <trace>   # Phase-1 cross-probe gate (≥95% hit-rate)

# Frontend (from app/)
npm install
npm run dev          # Vite dev server (http://localhost:5173)
npm run build        # tsc && vite build → dist/
npm test             # Vitest (e.g. elk.test.ts)
npm run tauri dev    # Tauri window + Vite HMR
npm run tauri build  # Bundle desktop app (Win/Linux/macOS)

# Python harness (from elaborate/, uv-managed)
uv sync
uv run pytest -q
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o <out.json>
```

Fixtures: `fixtures/picorv32_soc/` (committed golden + VCD/FST). PR-gate tests run
against them — **no Verilator regeneration needed** for normal work. See
`docs/fixtures.md` for the two-tier policy and pinned tool versions.

**CI:** `ci.yml` — Rust (fmt, clippy, test, match gate on FST+VCD) + Python (lint,
test, schema validation, golden reproducibility), Ubuntu, on push/PR. `app.yml` —
cross-platform Tauri build (Ubuntu + Windows) when `app/` or `core/crates/` change.
Nightly — Verilator trace regeneration.

## Workflow gates

- **Review before commit.** Never commit on the user's behalf without an explicit
  review pass. Before any `git commit`, show the diff (`git diff`/`git status`); when
  the change affects schematic/source/waveform views, also let the user verify it
  **visually** (`npm run tauri dev` / `npm run dev` or a screenshot). Wait for explicit
  go-ahead. "Implement X" is not standing approval to commit X.
- **Keep docs in sync after a PR.** After opening a new PR (or landing a change that
  alters architecture, commands, DTOs, gates, or workflow), update `CLAUDE.md` and the
  relevant `docs/*` in the same change so they never drift from the code. Treat doc
  updates as part of the PR, not a follow-up.
- **Label created issues.** Always attach existing labels (`gh label list`) — type
  (`bug`, `enhancement`/`feature`), area (`schematic`, `frontend`, `model`, `docs`),
  and an **effort** label (`effort/S|M|L` or `effort/xs…xl`, sized by files/layers
  touched). Create a label only when none fits; never leave an issue unlabeled.

## Conventions & gotchas

- **Rust toolchain pinned to 1.94** (`core/rust-toolchain.toml`) — match locally.
- **DTO sync** — Rust serde DTOs (`gui`, `schematic`) ↔ `app/src/types.ts` ↔
  `elaborate/schema/model.schema.json` must stay aligned.
- **TS is strict** (`strict`, `noUnusedLocals/Parameters`). No ESLint/Prettier — match
  existing style by hand.
- **No heuristics** — resolve via model indices (single source of truth).
- **Roadmap** — Phase 0–2 = model/matcher/cross-probe; Phase 3 = schematic + Tauri app
  (active area). See `docs/ROADMAP.md`.

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
- **Fan out for DTO-sync changes.** When a change touches all three layers (Rust serde
  ↔ `types.ts` ↔ `model.schema.json`), spin up one agent per layer **in a single
  message** to run them in parallel.
- **Review per language at the commit gate.** `ecc:rust-reviewer` (`core/crates/*`),
  `ecc:typescript-reviewer` (`app/src/*`), `ecc:python-reviewer` (`elaborate/*`) — in
  parallel when cross-cutting. Complements, never replaces, the user's review.
- **`fork` to preserve context** for sub-tasks needing the current conversation; a
  fresh `general-purpose` agent for self-contained work.
- **Don't double-run** a search you've already delegated.
