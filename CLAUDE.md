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
                           └─ app/src-tauri/src/lib.rs  → 12 #[tauri::command]s over Mutex<Session>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → CSS-grid panes (#98): tree | schematic (SVG/elk.ts) over source | full-width waveform (canvas)
```

### Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

| Crate | Package | Purpose |
| --- | --- | --- |
| `model` | `svxprobe-model` | Elaborated node model + indices (`path_index`, `src_index` interval tree, `wave_index`). The spine. |
| `ingest` | `svxprobe-ingest` | JSON → `Design` deserialization + referential-integrity validation (ref ranges + within-scope name uniqueness, whitelisting the port/backing-net dual-node pattern). |
| `wave` | `svxprobe-wave` | VCD/FST/GHW trace loader via `wellen` (lazy per-signal). |
| `matcher` | `svxprobe-matcher` | Phase-1 canonical-path matcher. **≥95% hit-rate is a hard PR gate.** |
| `xprobe` | `svxprobe-xprobe` | Cross-probe engine: source ↔ waveform ↔ schematic. |
| `schematic` | `svxprobe-schematic` | Layout-agnostic graph extractor: `scope_graph()`, `expand()`, `cone()`. |
| `gui` | `svxprobe-gui` | `Session` logic + serializable DTOs. No UI toolkit — CI-testable. |
| `cli` | `svxprobe` | Dev/test binary. Subcommands: `ingest`, `wave`, `match`, `graph`, `probe`. |
| `scale-bench` | `scale-bench` | **Dev-only (#24, Phase 4).** Deterministic synthetic-model generator (`generate`/`build_design`/`synth_signals`, seeded SplitMix64) + criterion benches (`load`/`query`/`matcher`) + a `report` bin. Measures the eager load path, scoped queries, high-fanout `cone()`, and matcher at 665/100K/1M nodes. `publish = false`; 1M gated behind `SCALE_BENCH_FULL`. A **real-design basis** loads any elaborated model via `SCALE_BENCH_MODEL=<hierarchy.json>` (handles auto-derived by `derive_handles`) — e.g. `claude_verilog_test` (`soc_top`, ~5.7K nodes) as a realism anchor. |

Tauri shell (`app/src-tauri/`, package `hdl-schemview-app`) is a thin `cdylib`/`lib`
wrapping `svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

### Frontend (`app/`, vanilla TS + Vite 5 + Vitest, no UI framework)

| File | Role |
| --- | --- |
| `app/index.html` | CSS-grid pane layout (#98): top-left hierarchy tree, a vertical `#col-splitter` (#139, drags the `--tree-w` track), a draggable `#row-splitter`, plus two tab groups (#99): top-right `#content` (source ↔ schematic, **source** active by default) and bottom `#bottom-group` (status ↔ waveform, **status** active by default). Each `.tab-group` has a `.tabbar` header; a tab's own controls (zoom bar, marker readout/unit) ride in a per-tab `.tab-aux`. The `#status-pane` holds the log pane (#100, #94 4c): `#status-log` is a scrollable list of timestamped, level-tagged (`log-info`/`log-warn`/`log-error`) rows fed by `main.ts`'s `log()`. Toolbar `#show-schematic`/`#show-waveform` buttons reveal + focus the on-demand views. |
| `app/src/main.ts` | UI logic + app state (graph, nav stack, selection, source cache, pinned waveform traces). Tabs (#99): `activateTab(panelId)` toggles `.active` within a `.tab-group` and redraws the now-visible view (schematic/waveform have 0-size containers while hidden — `refreshSchematic` re-fits a `schematicDirty` schematic, `redrawTracks` the canvas); `showInSource`/`showInSchematic`/`addToWaveform`/`jumpToScope` reveal the matching tab. Status/log (#100): `log(level, message)` appends a row to `#status-log` (via pure `formatLogEntry`/`formatTime` in `log.ts`), auto-scrolls, echoes the latest line to the compact toolbar `#status`, and on `error` brings the Status tab forward; design-load/parse progress + API errors route here. |
| `app/src/api.ts` | Typed wrappers over Tauri `invoke()`. |
| `app/src/types.ts` | DTO interfaces mirroring Rust serde types. |
| `app/src/elk.ts` (+ `elk.test.ts`) | `SchematicGraph` → ELK layout → SVG DOM. |
| `app/src/tree.ts` (+ `tree.test.ts`) | Pure helpers for the hierarchy tree pane (breadcrumb frames from a scope path). |
| `app/src/wave.ts` (+ `wave.test.ts`) | Waveform geometry (time-window mapping, zoom/pan, segments, value-at-time, ruler ticks) + per-trace/ruler canvas drawing. |
| `app/src/log.ts` (+ `log.test.ts`) | Pure helpers for the status/log pane (#100): `formatTime` (`HH:MM:SS`) + `formatLogEntry` (level + message → renderable entry). |
| `app/src/style.css` | Theme vars. Dark default; light via `:root[data-theme="light"]`, persisted in `localStorage`. |

Deps: `@tauri-apps/api`, `elkjs`. Schematic = SVG; waveform = canvas 2D. Right-click
a schematic box/pin/wire (or a source token) opens an action menu: **Append to
waveform** (stacks the signal as a new lane) / **Show in source**. The waveform pane
holds many traces (`state.waves`), stacked in scrollable fixed-height rows
(`name | value@A | track`) with per-row reorder/remove controls; the name/value
columns are drag-resizable (`state.waveCol`, persisted in `localStorage`). The tracks are
interactive: header buttons + Ctrl/⌘-scroll zoom (`state.waveView`) and drag-pan the
shared time window; left-click sets marker **A**, right-click marker **B**
(`state.markers`) — a top ruler shows tick timestamps, the header shows A/B/Δ, and the
value column reads each trace's value at A. A header unit dropdown (`state.waveUnit`,
ps/ns/µs/ms) rescales the ruler + readout via the trace's real timescale
(`trace_timescale` → `state.timescale`); marker/window state stays in raw ticks.
Right-clicking a signal's **name cell** opens a per-signal value-format menu: change
radix (bin/oct/dec/hex; multi-bit buses default hex via `WaveTrace.radix`) or **create
a sub-bus** — a derived track of `parent[hi:lo]` (synthetic negative `ref`) built by
slicing each value's bits. Native trace values are binary strings; `formatValue` and
`sliceBits` (in `wave.ts`) do the conversion/slicing. **Enum/FSM signals** show the
**state name** by default: the elaboration emits a normalized `enums` table
(value→name), surfaced per-signal via `WaveLink.enum_map` → `WaveTrace.enumMap`;
`enumName`/`displayValue` decode it (x/z or unmapped values fall back to the radix),
and the radix submenu adds a **State name** toggle.

## Tauri commands (`app/src-tauri/src/lib.rs` ↔ `app/src/api.ts`)

Delegate to a global `AppState(Mutex<Session>)`.

| Command | Args | Returns |
| --- | --- | --- |
| `load_design` | `model, trace, excluded[], srcRoot` | `String` (top scope) |
| `elaborate_and_load` | `filelist, top, incdirs[], trace, excluded[], srcRoot` | `String` (top scope) — runs `svxprobe-elaborate` (on PATH) on a `.f` designlist, then loads |
| `scope_graph` | `scope` | `SchematicGraph` |
| `expand_node` | `node` (id) | `SchematicGraph` |
| `hierarchy_tree` | `scope, depth` | `TreeNode` (lazy: children to `depth`, `expandable` beyond) |
| `cone` | `net` (id), `dir`, `depth` | `SchematicGraph` |
| `probe_node` | `path, context?` | `ProbeResponse \| null` |
| `probe_signal` | `fullName, context?` | `ProbeResponse \| null` |
| `probe_source` | `file` (id), `offset, context?` | `ProbeResponse \| null` |
| `signal_values` | `signalRef` | `ValueChange[]` |
| `source_text` | `file` (id) | `String` |
| `trace_timescale` | — | `TraceTimescale \| null` (factor + normalized unit) |
| `startup_args` | — | `StartupArgs \| null` (#136) — CLI launch args (`-f/-top/-I/-trace/-src-root`) parsed by the shell before the window opened; the frontend prefills the load form + auto-loads |

The shell parses `std::env::args()` in `run()` **before** any window (see
`svxprobe-gui::startup`): `-h`/`--help` → usage + exit 0; a usage error → stderr + exit 2;
a missing filelist/trace → stderr + exit 1; no args → normal GUI boot.

## Key data structures

**Schematic** (`core/crates/schematic/src/lib.rs`):
- `SchematicGraph { root, nodes: Vec<SchNode>, edges: Vec<SchEdge> }`
- `SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>, module: Option<String>, modport: Option<String> }` — `modport` (#106) marks an `Interface` node as a modport-qualified *port's* bundle. The frontend draws it as a **square frame pin** (#125) at the view boundary — a small teal square labeled `bus (mem_if.mem)` sharing the `_SEPARATE` frame column with the scope's own boundary `Port` pins (design-facing walls flush), every wired member wire anchored at the square (`FIXED_POS` wall-centre ports) before fanning out with its net label; members with no in-scope edge (e.g. unread `instr`/`addr`) are omitted from the pin. Interface *instances* (`modport == null`) keep the hexagon bundle box, and are now **drillable** (#97): a bare bundle with `Modport` children reports `expandable`, and `scope_graph`/`expand` on its path returns an `interface_interior` — each `Modport` view as a box (kind `Modport`), one directional pin per member (side from the member's `dir`, `path`/width from the underlying bundle signal via `Design::modport_member_nodes`), a wire for every member one view drives and another reads (`net_path` = the member signal), the interface's own ports (`clk`) as boundary frame pins wired into the views that read them, and a per-view boundary frame port (`MODPORT_FRAME_BASE + modport`, kind `Port`, `bundle`) marking each view's external face, wired to its box. The drillability predicate `is_bare_interface` is `pub` in the schematic crate and reused by `gui::is_tree_scope` (single source of truth), so the hierarchy tree lists the bundle too; double-clicking the bundle box (caret `▸`) descends into the views.
- `SchPort { id, name, side: Side, path: String, width: Option<String>, role: Option<PinRole>, bundle: bool, dangling: bool }` — `path` is the pin's canonical model path (empty for synthetic const pins) so a right-click cross-probes it; `bundle` marks a whole-interface pin (#106 consumer bundle pin, #96 access ports), drawn square instead of the directional triangle; `dangling` (#118) marks a pin nothing connects to (an instance port with no model edge and no constant tie-off, or a logic-box output no in-scope box reads) — shown dimmed instead of pruned, and a dangling FF Q gets an in-box name label since no wire labels it; `width` like `[31:0]`, else `None` (with an enum-table width fallback, e.g. `lane_state_e` → `[1:0]`); `role` (`PinRole { Clk, Reset, Enable }`, #59) tags a synthesized FF/latch pin from the model facts (`Node.type_` clock name / `Node.reset` / `Node.enable`) — the frontend's `ffRole` prefers it over its name-regex fallback. A bare interface instance bundle carries **aggregate access ports** (#96) instead of member pins, read off the connection edges: one port per consuming modport view (named after the view, id/path = the `Modport` node, wired straight to the consumer's #106 bundle pin) plus one raw port (named after the interface type, synthetic id, path = the instance) whose direct member taps draw as **one trunk wire per consumer wall** (#117): the backend still emits one `SchEdge` per member tap (the cross-probe truth), but the frontend collapses each (raw port, consumer box, wall) group into a single ELK edge anchored at a representative member pin (`trunkGroups`/`gatherBar` in `elk.ts`) and re-fans the members via a gather bar just off that wall — stubs cross-probe per member, the trunk/bar cross-probe the bundle, and selection links both ways (bundle lights its stubs; a stub lights itself + trunk, never siblings); the interface's real `Port` children (e.g. `clk`) stay ordinary pins.
- `SchEdge { id, source, target, net: Option<String>, net_path: Option<String> }` — `net_path` is the connecting net's canonical model path (absolute, no bit-select), so a wire click cross-probes via `probe_node`; `None` for synthetic constant tie-offs.
- `Side { West, East }` — drives ELK port placement.

**Model** (`core/crates/model/src/lib.rs`):
- `NodeId` = `u32` index into `Document::nodes`.
- `NodeKind { Instance, Net, Port, Var, Param, ModuleDef, GenBlock, Ff, Comb, Latch, Assign, Interface, Modport }` — `Interface` is an interface instance or a modport-specialized interface port; `Modport` a named view of a bundle.
- `Node { id, name, path, parent, children, kind, symbol_key, def_range, inst_range, type_, dir, const_value, modport, drivers, loads }` — `modport` records the view name on a modport-specialized interface port (e.g. `mem`). Such a port carries directional `Port` children (one per modport member, direction from slang's `ModportPort`); each pin's `path` is the *underlying member's* canonical path (the pin is a view of that signal), wired to it by an edge. Bare interface instances stay member-pin-less.
- `Design { doc, path_index, src_index, conn_index, wave_index }`.
- `Document.enums: HashMap<String, EnumDef>` — normalized enum table keyed by canonical type string (matches `Node.type_`); `EnumDef { width, members: Vec<EnumMember{name, value}> }`. Looked up via `Design::enum_for_type` and surfaced on `WaveLink.enum_map` for FSM state-name display.
- `Node.members: Option<Vec<ModportMember{name, dir}>>` — per-modport membership + directions on `Modport` nodes (descriptive metadata; the modport stays a view). A member's own node resolves via `Design::modport_member_nodes` (`<parent interface path>.<name>` path lookup).
- `Node.reset` / `Node.enable: Option<String>` (#59) — canonical paths of an inferred FF's async-reset signal and an inferred latch's gating (enable) signal. Structural facts from the harness: the reset is the timing-control edge whose signal the process body *also reads* (and `type_` then names the true clock); the enable is the sole signal read by the body's top-level conditional. Ambiguity ⇒ absent — never a name guess (a sync reset is structurally untaggable and stays untagged).

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
  (done, incl. 3d internal-logic drill-down); Phase 4 = scalability hardening (active
  area — benchmark → lazy/LoD audit → rkyv cache → redb/SQLite). The benchmark step
  landed first: the `scale-bench` crate (#24) measures the eager path at 665/100K/1M
  and already shows `scope_graph`/`expand`'s full-edge scan blowing up ~300× by 100K.
  See `docs/ROADMAP.md`.

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

<!-- code-review-graph MCP tools -->
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
