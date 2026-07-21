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
                           └─ app/src-tauri/src/lib.rs  → #[tauri::command]s over Mutex<HashMap<SessionId, Session>>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → CSS-grid panes (#98): tree | schematic (SVG/elk.ts) over source | full-width waveform (canvas)
```

### Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

| Crate | Package | Purpose |
| --- | --- | --- |
| `model` | `svxprobe-model` | Elaborated node model + indices (`path_index`, `src_index` interval tree, `wave_index`). The spine. |
| `ingest` | `svxprobe-ingest` | JSON → `Design` deserialization + referential-integrity validation (ref ranges + within-scope name uniqueness, whitelisting the port/backing-net dual-node pattern). **rkyv load cache (#21):** `from_path` gates on a `<model dir>/.schemview_data/<file>.rkyv` archive (header = `RKYV_FORMAT_VERSION` + `schema_version` + source `len`/`mtime_ns`); a fresh hit mmaps + bytecheck-validates + deserializes an owned `Document` (Option A, not zero-copy), skipping the JSON parse; any miss/stale/corrupt falls back to JSON + rewrites. `build_cache`/`svxprobe cache` pre-warm it. **wave_index cache (#153):** `try_load_wave_index`/`write_wave_index` also persist the matcher's resolved `(NodeId, var_ref)` pairs in a sibling `.schemview_data/<model>.<trace>.waveidx.rkyv` archive (header = `WAVE_INDEX_FORMAT_VERSION` + both files' `len`/`mtime_ns` + a `MatchOptions` hash), so a warm launch skips the matcher entirely. |
| `wave` | `svxprobe-wave` | VCD/FST/GHW trace loader via `wellen` (lazy per-signal). |
| `matcher` | `svxprobe-matcher` | Phase-1 canonical-path matcher. **≥95% hit-rate is a hard PR gate.** |
| `xprobe` | `svxprobe-xprobe` | Cross-probe engine: source ↔ waveform ↔ schematic. `CrossProbe::build` matches fresh; `build_cached` reuses the persisted `wave_index` (#153) when a fresh cache exists for `(model, trace, opts)`, else matches + writes it. **`rematch`/`rematch_cached` (#176)** re-match the *same* (unchanged) design against a **new trace in place** — the trace-swap path behind a waveform pane's "Load trace…" (#170), so no model is re-ingested and no designlist re-elaborated. They clear the old `wave_index` first: `run_match` only *inserts*, so a leftover mapping would keep resolving nodes to the previous trace's refs. |
| `schematic` | `svxprobe-schematic` | Layout-agnostic graph extractor: `scope_graph()`, `expand()`, `cone()`. `pin_width` is `pub` (#171) alongside `is_bare_interface`/`module_of` — the GUI's signal picker annotates rows with the same width rule (enum fallback included) the schematic annotates pins with, so the two can't disagree. `is_navigable_scope` (#184) is `pub` the same way — the single scope-root predicate: `scope_graph` resolves against it (rejecting a logic-only generate block), and `gui::is_tree_scope` *is* it, so the tree prunes exactly the blocks the schematic won't open. **Gate-level projection (#157 PR3, ADR 0005):** a `pub Projection { ProcessLevel (default), GateLevel }` selects the internal-logic granularity, taken by new `scope_graph_with`/`expand_with` entry points; the bare `scope_graph`/`expand` delegate with `ProcessLevel`, so **every existing caller and its output are unchanged** (a `_with` variant, not a new required arg — avoids churning ~40 call sites). Under `GateLevel`, `child_boxes` dissolves each combinational block (`Comb`/`Latch`/`Assign`) into its flat gate/mux children (`make_gate_box`: inputs west, one east output pin keyed `(gate,gate)`, the mux select tagged `PinRole::Sel` from `MuxPort::Sel`), wiring gate→signal and gate→gate through the existing signal-join pass (each gate self-drives its own id key so a non-root gate with no `out` edge still connects); `Ff`/`Memory`/instances stay opaque at both levels (their input-cloud decomposition is a deferrable later slice). A model with no gate primitives renders identically either way, and the harness must be run with `--gate-level` to emit them at all. **PR4 wires the plumbing:** `Projection` gains serde derives (kebab-case `"process-level"`/`"gate-level"`) and is re-exported through `gui`; `Session::scope_graph_with`/`expand_with` thread it (the bare `Session::scope_graph`/`expand` delegate with `ProcessLevel`), and the `scope_graph`/`expand_node` Tauri commands take an optional `projection` param (default `ProcessLevel`). **PR5 wires the frontend** (`app/src`): a Settings "Gate-level schematic" toggle (`prefs.ts` `loadGateLevel`/`saveGateLevel`, default off) makes `main.ts`'s `setScope`/`showInSchematic` pass `"gate-level"`; `elk.ts` `muxChild`/`gateChild` size the primitives (mux select on the south wall) and `main.ts` `renderMux`/`renderGate` draw the IEEE distinctive glyphs (flat-back-D AND, curved OR, notched XOR, N-variant output bubbles, Not/Buf triangles, datapath op boxes, wires meeting the shape directly with west input leads to the actual back/arch) — so gate-level is now user-reachable, not just plumbed. **Inverter folding (#157):** `is_foldable_not` collapses a single-fanout `~operand` (a non-root `Not` read by exactly one gate) — `child_boxes` drops the `Not` box and `make_gate_box`/the signal-join rewire the operand straight to the consumer's input pin, tagged `PinRole::Inv` (serialized `"inv"`) so `renderGate` draws an inversion bubble outside that pin instead of a separate inverter. A `~signal` that drives a scope signal directly (`assign y = ~a`, it has an `out` edge) or an inverter with >1 reader stays a standalone `Not`. **Constant/parameter tie values (#199):** a gate/mux/datapath operand that is a hard-coded literal (`a & 8'hFF`) or a parameter (`a & MASK`) now surfaces its value as a tie. The harness (`--gate-level`) emits a literal as a synthetic `Const` node (`kind=Const`, value on `Node.const_value`) with an operand edge, and wires a parameter operand to its real `Param` node (relaxed `_wire_input` guard) after stamping the elaborated value into that `Param`'s `const` (`_param_value` reads the symbol's `.value`, since a param `NamedValue` carries no `.constant`). The value rides **inline on the gate input pin**: `make_gate_box` sets `SchPort.constant` from the operand endpoint's `const_value`, and `renderGate`/`renderMux` draw it as a `const-label` just left of the pin's west wall (`elk.ts` reserves a west margin sized to the value), so the tie value is **traceable right at the gate** rather than a separate source box. `Const` is never a box (excluded from gates/boxes); no separate source node is synthesized for gate operands (instance-port ties keep theirs). **`'x` don't-care branches (`sel ? a : 'x`):** slang leaves `.constant` unset for an `'x` literal, so `_literal_value` reads the `SVInt` off `.value` — otherwise those branches vanished, leaving muxes with a missing input. A concatenation `{a, b, …}` or replication `{n{a}}` operand becomes a **`Concat` primitive** box (`_emit_expr` → `_emit_gate("Concat", …)`, drawn `{ }`) gathering its element expressions — so `mem_la_addr`'s `sel ? {next_pc[31:2] + …, 2'b00} : {reg_op1[31:2], 2'b00}` renders as a full 3-input mux (the nested `+` decomposes to an `Add` feeding the concat, `2'b00` ties inline). **Memory-array read operands (#206):** a gate/mux operand that reads an array element (`cpuregs[decoded_rs1]`) resolves via `_leaf_signal` to the whole array node; `_wire_input` now accepts a `Memory` endpoint (not just `Net`/`Var`/`Port`), so the branch wires to the array instead of vanishing — the index being the same fidelity simplification as a peeled bit-select. The schematic gives such a read a home: a `Memory` box **drives its own array node** in the signal-join (a synthesized east read-out `Dout` pin keyed `(mem,mem)`, added only when an in-scope gate loads the array), so the reader's wire reaches the memory glyph. This fixed the 6 `cpuregs`-read muxes that were the actual "incomplete muxes" (the #206 title said *function-call* operands, but the golden has none — the gap was memory reads). **Remaining gap:** a genuine **function-call** operand still isn't decomposed (like the Div/Mod datapath deferral) — none occur in the committed golden. Additive: `schema_version` stays `1` and the flag-off output is byte-identical. (Frontend `renderSource` highlights the def's span **by line number** (#203): `SourceLoc` carries `line`+`end_line` and `source.ts` `highlightLineRange` lights `line..=end_line`, so Show-in-source lands on the right line regardless of the `def_range` byte-offset basis. This replaced an earlier byte-offset approach that assumed offsets were raw file bytes — wrong for the committed golden, whose offsets are LF-based, so deep constructs drifted up on a CRLF checkout. `lineStarts` is retained only for the source-*click* path `offsetAt`, which shares the same offset basis; a repo-wide **`.gitattributes eol=lf`** (#203) pins the RTL source and committed goldens to LF on every platform, so the working tree matches the LF `def_range` offsets — resolving the drift for both the highlight and click paths, and making golden regeneration deterministic on Windows.) |
| `gui` | `svxprobe-gui` | `Session` logic + serializable DTOs. No UI toolkit — CI-testable. **`scope_signals`/`SignalEntry` (#171)** back the waveform panes' signal picker: the signals declared directly inside a scope, deduped by canonical path and answered *through* the cross-probe (never re-derived), with `is_signal_kind` excluding `Param` (never traceable) and `Interface`/`Modport` (a modport port's children live in another scope). Unions **every** structural node at the path, unlike `hierarchy_tree`'s `find` — one path can carry several `GenBlock`s. |
| `cli` | `svxprobe` | Dev/test binary. Subcommands: `ingest`, `cache` (pre-build the #21 rkyv load cache), `wave`, `match`, `graph`, `probe`. |
| `scale-bench` | `scale-bench` | **Dev-only (#24, Phase 4).** Deterministic synthetic-model generator (`generate`/`build_design`/`synth_signals`, seeded SplitMix64) + criterion benches (`load`/`query`/`matcher`) + a `report` bin. Measures the eager load path, scoped queries, high-fanout `cone()`, and matcher at 665/100K/1M nodes. `publish = false`; 1M gated behind `SCALE_BENCH_FULL`. A **real-design basis** loads any elaborated model via `SCALE_BENCH_MODEL=<hierarchy.json>` (handles auto-derived by `derive_handles`) — e.g. `claude_verilog_test` (`soc_top`, ~5.7K nodes) as a realism anchor. |

Tauri shell (`app/src-tauri/`, package `hdl-schemview-app`) is a thin `cdylib`/`lib`
wrapping `svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

### Frontend (`app/`, vanilla TS + Vite 5 + Vitest, no UI framework)

| File | Role |
| --- | --- |
| `app/index.html` | CSS-grid pane layout (#98): top-left hierarchy tree, a vertical `#col-splitter` (#139, drags the `--tree-w` track), a draggable `#row-splitter`, plus two tab groups (#99): top-right `#content` (source ↔ schematic ↔ **settings** (#17), **source** active by default) and bottom `#bottom-group` (status ↔ waveform, **status** active by default). Each `.tab-group` has a `.tabbar` header; a tab's own controls (zoom bar, marker readout/unit) ride in a per-tab `.tab-aux`. The `#status-pane` holds the log pane (#100, #94 4c): `#status-log` is a scrollable list of timestamped, level-tagged (`log-info`/`log-warn`/`log-error`) rows fed by `main.ts`'s `log()`. The `#settings-pane` (#17) is a preferences form (theme select, excluded-scopes input). The **Schematic** and **Waveform** tab buttons start `hidden` (#17); `activateTab` un-hides a tab when it's activated, so the toolbar `#show-schematic`/`#show-waveform` buttons (or an append/show-in action) reveal + focus the on-demand views. **Detach (#18 PR2):** a `⇱` `#pop-schematic`/`#pop-waveform` button rides in each on-demand tab's `.tab-aux`; the same page reloads with `?pane=schematic\|waveform&win=<label>` in a second Tauri window, where `body.detached`/`body.detached-<pane>` CSS collapses the grid to that one pane full-window (chrome, tree, tabs, pop-out hidden). **Independent schematic panes (#169):** each schematic pop-out gets a *unique* label (`schematic-1`, `schematic-2`, …) so multiple coexist, each parked on its own scope; main keeps its own schematic (no handover). **Independent waveform panes (#170):** the same for waveform (`waveform-1`, `waveform-2`, …) — main keeps its own waveform. A `#load-trace` ("Load trace…") button rides in the waveform `.tab-aux` of **every** window: in main it swaps main's own trace without re-entering the toolbar's design load; in a pop-out it swaps only that pane's trace. A `body.detached-waveform #load-trace` rule re-shows it inside a detached window (the blanket `body.detached .pop-btn` rule would otherwise hide it with the ⇱ detach button). **Per-pane signal picker (#171):** `#wave-picker` (`#wave-picker-tree` over `#wave-picker-sigs`) is a collapsible sub-column **inside `#wave-pane`**, toggled by `#wave-pick-btn` ("☰ Signals", in the waveform `.tab-aux`) or **Ctrl/⌘+B**; it needs the same `body.detached-waveform #wave-pick-btn` escape hatch as `#load-trace`. It lives *inside the pane* rather than in the window's `#hier-pane` column precisely so every pop-out inherits it with **no `body.detached` grid surgery** — `body.detached-waveform` already hands `#bottom-group` the whole window. `#wave-picker[hidden]{display:none}` must outrank its own `display:flex` (the `.tab-aux[hidden]` trick), or a "collapsed" picker still renders. Tree row CSS is `.tree`-scoped, not `#hierarchy`-scoped, so both trees are styled. |
| `app/src/main.ts` | UI logic + app state (graph, nav stack, selection, source cache, pinned waveform traces). Tabs (#99): `activateTab(panelId)` toggles `.active` within a `.tab-group` and redraws the now-visible view (schematic/waveform have 0-size containers while hidden — `refreshSchematic` re-fits a `schematicDirty` schematic, `redrawTracks` the canvas); `showInSource`/`showInSchematic`/`addToWaveform`/`jumpToScope` reveal the matching tab. Source/tree navigation: a tree row's single-click drives the schematic (`jumpToScope`), a **double-click** reveals the node in source (#164 — probes `node.path` → publishes a `["source"]` selection); a **left-click** in the source pane moves the highlight to just the clicked line (#163, `onSourceClick` — a lightweight DOM `.hl`-marker shift, not the whole-block highlight of #158), while right-click keeps the explicit schematic/waveform cross-probe menu. The tree double-click still routes through the selection bus (#18). Settings (#17): `initSettings` populates the `#settings-pane` controls from `prefs.ts` and wires them back — theme applies live (`setTheme`), excluded scopes take effect on the next load (`loadExcluded()` feeds the two load calls). Status/log (#100): `log(level, message)` appends a row to `#status-log` (via pure `formatLogEntry`/`formatTime` in `log.ts`), auto-scrolls, echoes the latest line to the compact toolbar `#status`, and on `error` brings the Status tab forward; design-load/parse progress + API errors route here. **Detached windows (#18 PR2):** `windowMode = paneModeOf(location.search)` splits boot — `init()` runs the full app, `initDetached(pane)` boots a single-pane window (seeded from the main window's `localStorage` snapshot: `detach:<label>:scope` = a schematic pane's scope path (#169), `detach:<label>:wave` = a `WaveSnapshot` JSON `{load,waves,waveView,markers,waveUnit}` (#170), both per unique window label, with `enumMap` as `[value,name][]` pairs; `storeTrace`/`loadTrace` round-trip the Map). Each window knows its own `selfLabel` (from `?win=`; `main` otherwise). `popOut(pane)` writes the snapshot then creates a `WebviewWindow` — schematic (#169) and waveform (#170) each get a fresh unique label every call, so pop-outs are independent. **On pop-out `hideTab` (#205) closes the originating in-app tab** (hiding the tab button, falling back to Source/Status); the toolbar `#show-schematic`/`#show-waveform` button re-reveals it via `activateTab` — main keeps its own backend pane state either way (no handover; the old `markDetached`/`focusPane` mirror machinery is gone). **Per-pane waveform sessions (#170):** a waveform window's `selfLabel` *is* its backend `session_id` (`sid`; undefined elsewhere → the `main` session). `initDetached` calls `loadPaneSession(snap.load, selfLabel)` to load that pane's own design + trace, threads `sid` through `signalValues`/`traceTimescale`/`probeNode`. `loadTraceOnly()` (the `#load-trace` button → `@tauri-apps/plugin-dialog`'s `open`) calls `load_trace` (#176) for **this window's** session (`sid`, so main or one pane) on a newly picked VCD/FST/GHW *keeping the current design*, re-resolving each lane by its stored model `path` (`WaveTrace.path`) and dropping signals the new trace lacks — main then updates `state.loaded` + the toolbar `#trace` field, a pop-out re-persists its snapshot with the re-resolved lanes. Main drops a pane's session on `tauri://destroyed` via `unloadDesign(label)`. `state.loaded` (a `LoadSpec`: model JSON or designlist) captures what main loaded so a pop-out can boot the same design. `handleSelection` gates each pane on `ownsSelection({mode,self}, target, dest, [])`: source → main only; schematic/waveform → the window whose label matches `dest`, or main's own on a broadcast — so a pop-out never follows another (#169/#170). `appendResolved` re-resolves an addressed cross-probe against *this* window's trace by model path before appending (a `signal_ref` isn't portable across traces; the model node path is), and `appendWaveItem`/`waveformDestinations` build the right-click **Append to waveform ▸ [main window | waveform-1 | …]** flyout that addresses one specific pane. **Signal picker (#171):** `setupPicker` (wired from *both* `init` and `initDetached`) binds `#wave-pick-btn` + `Ctrl/⌘+B` (gated on a visible `#wave-pane`; persisted globally under `wavePickerOpen`, closed by default); `initPicker` builds the pane's tree via `createTree` on `state.top`, `showScopeSignals` lists a scope through `api.scopeSignals(scope, sid)`, and `pickSignal` is `probeNode(path, null, sid)` → the **existing** `addToWaveform` (so dedupe/enumMap/lane order/tab reveal are unchanged). `state.top` (the design top) is captured in `load()` and, in a pop-out, from `loadPaneSession`'s **return value** — previously discarded. The picker's tree deliberately does *not* `jumpToScope` (that broadcasts, and would drive main's schematic from a pop-out). After `loadTraceOnly` the design is unchanged, so the tree is **not** rebuilt (it would collapse the user's expansion state) — only the listed scope's `in_trace` flags are refetched. |
| `app/src/api.ts` | Typed wrappers over Tauri `invoke()`. |
| `app/src/types.ts` | DTO interfaces mirroring Rust serde types. |
| `app/src/elk.ts` (+ `elk.test.ts`) | `SchematicGraph` → ELK layout → SVG DOM. |
| `app/src/tree.ts` (+ `tree.test.ts`) | The hierarchy tree: pure `scopeFrames` (breadcrumb frames from a scope path) **+ `createTree({host, fetchChildren, onSelect, onActivate?})` → `{init, highlight, clear}`** (#171) — the DOM factory behind *both* the window's `#hierarchy` tree and every waveform pane's `#wave-picker` tree. `treeItems` is closed over **per instance**, so two trees coexist in one document (a module-level map let one tree's `init` orphan the other's rows and steal its highlight). `fetchChildren` is injected rather than importing `api`, so the module stays transport-free and each tree names its own session (`sid`). **The only DOM outside `main.ts`** — `tree.test.ts` opts into `happy-dom` via a per-file `// @vitest-environment` docblock, so every other suite keeps Vitest's faster DOM-free `node` env. |
| `app/src/wave.ts` (+ `wave.test.ts`) | Waveform geometry (time-window mapping, zoom/pan, segments, value-at-time, ruler ticks) + per-trace/ruler canvas drawing. `WaveTrace.path` (#170) carries the lane's canonical model node path so a pane can re-resolve the lane against a different trace (a `signal_ref` is trace-specific; the model path is the portable key). |
| `app/src/log.ts` (+ `log.test.ts`) | Pure helpers for the status/log pane (#100): `formatTime` (`HH:MM:SS`) + `formatLogEntry` (level + message → renderable entry). |
| `app/src/prefs.ts` (+ `prefs.test.ts`) | Settings preferences (#17): DOM-free `parseExcluded`/`formatExcluded`/`coerceExcluded` (excluded-scopes editor round-trip + default fallback) + thin localStorage wrappers (`loadExcluded`/`saveExcluded`; key `excludedScopes`). Theme stays under the existing `theme` key. |
| `app/src/bus.ts` (+ `bus.test.ts`) | The single cross-pane coordination path (#18). Right-click/tree handlers `publish` a `Selection` (a resolved `ProbeResponse` + which panes to reveal, or a scope path to drill); one `subscribe(handleSelection)` in `main.ts` drives the panes. Transport is Tauri app-global `emit`/`listen` inside the webview (so it also reaches detached windows, #18 PR2), a module-local fan-out in browser/tests — one channel, two transports, chosen by `__TAURI_INTERNALS__` presence. Pure builders `crossProbeSelection`/`scopeSelection` (both take an optional `dest` window label, #169) — plus `paneModeOf` (read `?pane=` mode) and `ownsSelection` (which window drives which pane, keyed on window id + selection `dest`, #18 PR2 / #169 / #170) — are unit-tested; the payload carries model lookups, never geometry. `ownsSelection` is now purely id/dest-keyed for the pane views: `source` → main only, while `schematic` (#169) and `waveform` (#170) share one branch — an addressed selection drives exactly the window whose `self` label equals `dest` (main is addressable as `"main"`), a broadcast drives only main's own pane, and pop-outs never follow a broadcast. |
| `app/src/style.css` | Theme vars. Dark default; light via `:root[data-theme="light"]`, persisted in `localStorage`. |

Deps: `@tauri-apps/api`, `elkjs`. Schematic = SVG; waveform = canvas 2D. Right-click
a schematic box/pin/wire (or a source token) opens an action menu: **Append to
waveform** (stacks the signal as a new lane) / **Show in source**. The waveform pane
carries its own **signal picker** (#171, `Ctrl/⌘+B`) — a scope tree over that scope's
signals (`hierarchy_tree` + `scope_signals`, both on the pane's `session_id`), so a pane
picks its own lanes instead of waiting for another window to address them at it; a
signal absent from the pane's trace is **dimmed and inert**, not pruned. The pane
organizes its lanes into **collapsible groups** (#182, `state.groups: WaveGroup[]`,
each `WaveGroup { name, collapsed, waves }`) — there are no loose lanes. Groups are
user-authored containers: **a group emptied by a move/remove is preserved, not pruned**
(#188), and the pane always ends with a trailing **empty group** as the landing spot for
a new one (`normalizeGroups`/`workingGroupIndex` in `wave.ts`, re-enforced in
`renderWaves` — the invariant is now just "keep every group + ensure a trailing empty",
no interior-empty pruning). A fresh pane is a single empty group. New signals accumulate
in the **working group** (last populated); a lane's name-cell menu **Move to group ▸
[group | Group N (empty)]** regroups it (the non-drag path), or you **drag a lane by its
name cell** to a new slot (#188) — within a group, across groups, or onto an **empty
group** (rendered as a tall dashed `drop-zone` that lights up while dragging over it,
since a bodyless header is too small to hit); the drop lands where an accent line
(`moveLaneTo` in `wave.ts`, keyed by the stable lane `key` so a mid-drag re-render can't
move the wrong lane) or the highlighted zone marks it, and `renderWaves`'s
`normalizeGroups` appends a fresh trailing empty when the last group fills. A drag started
on the name cell's column resizer is suppressed (`suppressLaneDrag`). A group header's twist
folds it away (collapsed groups render no tracks — `redrawTracks`/`markerTimeAt` map
canvases against `visibleLanes`, not `flattenLanes`), and double-clicking the header
renames it. **Right-clicking a group header** (#192, `openGroupMenu`) offers
collapse/expand, rename, and **delete** — delete is enabled only for an *empty* group (a
populated one must have its lanes moved/removed first, so signals are never dropped by a
group delete), and `deleteGroup` splices the group out, `normalizeGroups` keeping the
trailing-empty invariant so the pane never falls to zero groups. Groups round-trip the
pop-out `WaveSnapshot` (`StoredGroup`) and survive a trace swap (`loadTraceOnly`
re-resolves per group in place). Each group's lanes are
fixed-height rows (`name | value@A | track`) with per-row reorder/remove controls keyed
by the stable lane `key` (#179); the name/value columns are drag-resizable
(`state.waveCol`, persisted in `localStorage`). The list is one flat CSS grid
(`#wave-list.has-rows`, group headers span all columns) whose `align-content:start` (#180)
packs the lanes at the top — the grid's default `stretch` would inflate the auto rows to
fill the tall pane and spread the lanes apart — with no container padding so the stack
sits flush. The tracks are
interactive: header buttons + Ctrl/⌘-scroll zoom (`state.waveView`) and drag-pan the
shared time window; left-click sets marker **A**, right-click marker **B**
(`state.markers`) — a top ruler shows tick timestamps and **stays pinned** while the
lanes scroll under it (#181: `position:sticky;top:0` on all four ruler-row cells — the
three `.wave-spacer`s and `.wave-ruler-cell` — with an opaque `--bg`, since the ruler
canvas is transparent), the header shows A/B/Δ, and the
value column reads each trace's value at A. A header unit dropdown (`state.waveUnit`,
ps/ns/µs/ms) rescales the ruler + readout via the trace's real timescale
(`trace_timescale` → `state.timescale`); marker/window state stays in raw ticks.
Right-clicking a signal's **name cell** opens a per-signal value-format menu: change
radix (bin/oct/dec/hex; multi-bit buses default hex via `WaveTrace.radix`), **add
another view** (#179 — stack the same signal as a second lane so it can be read as hex
*and* state name at once; a plain append still dedupes by `ref`, this deliberately does
not), **move to group** (#182), or **create a sub-bus** — a derived track of `parent[hi:lo]` (synthetic negative
`ref`) built by slicing each value's bits, carrying `WaveTrace.slice` + the parent
`path` so a trace swap re-derives it (`reresolveLane`) instead of dropping it. Because a
signal can now be several lanes, `ref` no longer identifies a lane: each carries a
stable `WaveTrace.key` (minted per window, round-tripped in the snapshot, counters
reseeded on load via `laneCounterSeeds` so a pop-out never re-mints a collision);
reorder/remove stay index-keyed and the picker's *added* mark stays path-keyed, so
duplicates don't break them. Native trace values are binary strings; `formatValue` and
`sliceBits` (in `wave.ts`) do the conversion/slicing. **Enum/FSM signals** show the
**state name** by default: the elaboration emits a normalized `enums` table
(value→name), surfaced per-signal via `WaveLink.enum_map` → `WaveTrace.enumMap`;
`enumName`/`displayValue` decode it (x/z or unmapped values fall back to the radix),
and the radix submenu adds a **State name** toggle.

## Tauri commands (`app/src-tauri/src/lib.rs` ↔ `app/src/api.ts`)

Delegate to a global `AppState(Mutex<HashMap<SessionId, Session>>)` (#168) — sessions
keyed by id. **Multi-session (#168):** every command (except `startup_args`) takes an
optional `session_id`; omitting it targets the `"main"` session, so single-session
behavior is unchanged. **#170 is the first consumer:** each independent waveform
window passes its own id (its window label, e.g. `waveform-1`) to load + query a
separate trace of the same design — `load_design`/`elaborate_and_load` insert under
the id, `unload_design` drops it when the window closes (idempotent). Main + schematic
panes still omit the id. Detached pane windows are created from the frontend via
`WebviewWindow` (no Rust command); `capabilities/default.json` grants window/webview-
create + management perms, scopes to the `main`/`waveform`/`waveform-*`/`schematic`/
`schematic-*` labels (#169, #170), and grants `dialog:allow-open` for a waveform pane's
native "Load trace…" picker (`tauri-plugin-dialog`, registered in `run()`).

| Command | Args (all also take optional `session_id`) | Returns |
| --- | --- | --- |
| `load_design` | `model, trace, excluded[], srcRoot` | `String` (top scope) — inserts under `session_id` (default `main`) |
| `elaborate_and_load` | `filelist, top, incdirs[], trace, excluded[], srcRoot` | `String` (top scope) — runs `svxprobe-elaborate` (on PATH) on a `.f` designlist, then loads |
| `load_trace` | `trace` | `()` — swaps the session's trace, **reusing its already-ingested design** (#176): no model re-ingest, no designlist re-elaboration. Backs "Load trace…" (#170). Opens the trace before mutating, so a bad path leaves the session intact. |
| `unload_design` | — | `()` — drops the session (#168); idempotent |
| `scope_graph` | `scope`, `projection?` | `SchematicGraph` — `projection` (#157 PR4) is `"process-level"` (default, omit) or `"gate-level"`; forwarded to `Session::scope_graph_with` |
| `expand_node` | `node` (id), `projection?` | `SchematicGraph` — same optional `projection` as `scope_graph`; forwarded to `Session::expand_with` |
| `hierarchy_tree` | `scope, depth` | `TreeNode` (lazy: children to `depth`, `expandable` beyond) |
| `scope_signals` | `scope` | `SignalEntry[]` (#171) — the `Port`/`Net`/`Var`/`Memory` declarations directly inside a scope, in declaration order, one row per canonical path (the port/backing-net dual node collapses). Each row is the **cross-probe's own** answer (`from_node_path` + `to_wave`), so its `kind`/`width`/`in_trace` can't contradict the `probe_node` a click makes. Errors for a non-scope path. Pairs with `hierarchy_tree`: that lists the scopes, this lists what is *in* one. |
| `cone` | `net` (id), `dir`, `depth` | `SchematicGraph` |
| `probe_node` | `path, context?` | `ProbeResponse \| null` |
| `probe_signal` | `fullName, context?` | `ProbeResponse \| null` |
| `probe_source` | `file` (id), `offset, context?` | `ProbeResponse \| null` |
| `signal_values` | `signalRef` | `ValueChange[]` |
| `source_text` | `file` (id) | `String` |
| `trace_timescale` | — | `TraceTimescale \| null` (factor + normalized unit) |
| `startup_args` | — (no `session_id`) | `StartupArgs \| null` (#136) — CLI launch args (`-f/-top/-I/-trace/-src-root`) parsed by the shell before the window opened; the frontend prefills the load form + auto-loads |

The shell parses `std::env::args()` in `run()` **before** any window (see
`svxprobe-gui::startup`): `-h`/`--help` → usage + exit 0; a usage error → stderr + exit 2;
a missing filelist/trace → stderr + exit 1; no args → normal GUI boot.

## Key data structures

**Schematic** (`core/crates/schematic/src/lib.rs`):
- `SchematicGraph { root, nodes: Vec<SchNode>, edges: Vec<SchEdge> }`
- `SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>, module: Option<String>, modport: Option<String> }` — `modport` (#106) marks an `Interface` node as a modport-qualified *port's* bundle. The frontend draws it as a **square frame pin** (#125) at the view boundary — a small teal square labeled `bus (mem_if.mem)` sharing the `_SEPARATE` frame column with the scope's own boundary `Port` pins (design-facing walls flush), every wired member wire anchored at the square (`FIXED_POS` wall-centre ports) before fanning out with its net label; members with no in-scope edge (e.g. unread `instr`/`addr`) are omitted from the pin. Interface *instances* (`modport == null`) keep the hexagon bundle box, and are now **drillable** (#97): a bare bundle with `Modport` children reports `expandable`, and `scope_graph`/`expand` on its path returns an `interface_interior` — each `Modport` view as a box (kind `Modport`), one directional pin per member (side from the member's `dir`, `path`/width from the underlying bundle signal via `Design::modport_member_nodes`), a wire for every member one view drives and another reads (`net_path` = the member signal), the interface's own ports (`clk`) as boundary frame pins wired into the views that read them, and a per-view boundary frame port (`MODPORT_FRAME_BASE + modport`, kind `Port`, `bundle`) marking each view's external face, wired to its box. The drillability predicate `is_bare_interface` is `pub` in the schematic crate and reused by `gui::is_tree_scope` (single source of truth), so the hierarchy tree lists the bundle too; double-clicking the bundle box (caret `▸`) descends into the views.
- `SchPort { id, name, side: Side, path: String, width: Option<String>, role: Option<PinRole>, bundle: bool, dangling: bool }` — `path` is the pin's canonical model path (empty for synthetic const pins) so a right-click cross-probes it; `bundle` marks a whole-interface pin (#106 consumer bundle pin, #96 access ports), drawn square instead of the directional triangle; `dangling` (#118) marks a pin nothing connects to (an instance port with no model edge and no constant tie-off, or a logic-box output no in-scope box reads) — shown dimmed instead of pruned, and a dangling FF Q gets an in-box name label since no wire labels it. **A dangling gate output (#202)** is relabelled with its driven net (the gate's `out`-edge endpoint `name`/`path`) in the dangling-marking pass, and `renderGate`/`renderMux` float that name just past the east wall — so a root gate whose signal has no in-scope reader (e.g. `mem_busy`, correct #118) keeps a labelled, cross-probeable floating wire instead of an anonymous stub. (The residual gap that leaves the ALU muxes' `alu_add_sub`/`alu_shl`/`alu_shr` readerless is that the harness doesn't lower `case` statements into mux trees — a decomposition-completeness follow-up, sibling of #206.) `width` like `[31:0]`, else `None` (with an enum-table width fallback, e.g. `lane_state_e` → `[1:0]`); `role` (`PinRole { Clk, Reset, Enable, Addr, Din, Dout, Write, Read }`, #59/#112) tags a synthesized FF/latch pin from the model facts (`Node.type_` clock name / `Node.reset` / `Node.enable`) — the frontend's `ffRole` prefers it over its name-regex fallback — or a MEMORY glyph pin (`Addr`/`Din` west, `Dout` east; `Write`/`Read` enables reserved for #157) from the `Edge.mem_port` role. A `Memory` `SchNode` also carries `memDepth`/`initSource` (the array label + INIT tab); `elk.ts` `memoryChild` + `main.ts` `renderMemory` draw the array-stack glyph. A bare interface instance bundle carries **aggregate access ports** (#96) instead of member pins, read off the connection edges: one port per consuming modport view (named after the view, id/path = the `Modport` node, wired straight to the consumer's #106 bundle pin) plus one raw port (named after the interface type, synthetic id, path = the instance) whose direct member taps draw as **one trunk wire per consumer wall** (#117): the backend still emits one `SchEdge` per member tap (the cross-probe truth), but the frontend collapses each (raw port, consumer box, wall) group into a single ELK edge anchored at a representative member pin (`trunkGroups`/`gatherBar` in `elk.ts`) and re-fans the members via a gather bar just off that wall — stubs cross-probe per member, the trunk/bar cross-probe the bundle, and selection links both ways (bundle lights its stubs; a stub lights itself + trunk, never siblings); the interface's real `Port` children (e.g. `clk`) stay ordinary pins.
- `SchEdge { id, source, target, net: Option<String>, net_path: Option<String> }` — `net_path` is the connecting net's canonical model path (absolute, no bit-select), so a wire click cross-probes via `probe_node`; `None` for synthetic constant tie-offs.
- `Side { West, East }` — drives ELK port placement.

**Model** (`core/crates/model/src/lib.rs`):
- `NodeId` = `u32` index into `Document::nodes`.
- `NodeKind { Instance, Net, Port, Var, Param, ModuleDef, GenBlock, Ff, Comb, Latch, Assign, Interface, Modport, Memory, And, Or, Xor, Xnor, Nand, Nor, Not, Buf, Add, Sub, Mul, Cmp, Shift, Mux, Const, Concat }` — `Interface` is an interface instance or a modport-specialized interface port; `Modport` a named view of a bundle; a `GenBlock` is one *elaborated* generate branch — the harness drops uninstantiated branches (`_walk` gates on slang's `isUninstantiated`, #178) so a discarded `if`-branch neither reparents its phantom logic (its `always @(posedge clk)` would double-drive the live `else`'s target) nor collides on the shared LRM-implicit name (`genblk1`) that made one path carry several nodes. A `GenBlock` is a **navigable scope** (a tree row with its own schematic) only if its subtree holds a real design object — an `Instance` or a bare `Interface` (#184, `is_navigable_scope`, `pub` in the schematic crate and reused verbatim by `gui::is_tree_scope` so tree and schematic agree); a **logic-only** block (`comb`/`ff`/`assign` only, e.g. `core.genblk1/2/3`) is a syntactic wrapper whose contents already render dissolved into the enclosing module (`child_boxes`), so `hierarchy_tree`/`scope_graph`/`scope_signals` reject it and a cross-probe onto its path walks *up* to the parent (`showInSchematic` already retries ancestors). `g_lane[0]` is a `GenBlock` too but holds instances → stays navigable; the test is contents, not the generate keyword. `Memory` (#112) a memory array (`logic [W-1:0] ram [0:N-1]` — the unpacked-dimension `Var`, re-kinded) drawn as a MEMORY glyph. A `Memory` node carries `mem_depth` (word count) and `init_source` (`$readmemh`/`$readmemb` arg text → INIT marker; `initial` stays non-logic per ADR 0004). Its addr/din/dout pins come from typed `Edge.mem_port` (`MemPort { Addr, Din, Dout }`) edges the harness emits by classifying `ram[idx]` accesses (addr = index expr, din = written value, dout = read target). Process-granularity per ADR 0004 (amended) — the glyph maps to the array's `def_range`, so cross-probe stays a lookup. The **gate-level primitives** (`And`…`Mux`, #157/ADR 0005) are emitted only by the harness's opt-in `--gate-level` pass (PR1) and now carried by the model spine (PR2): each is a flat child of its process/assign node with a sub-expression `def_range` (the scoped ADR-0004 relaxation), associative chains collapse to one N-input gate, `~` folds onto the base gate, and a `?:` becomes a `Mux`. A datapath primitive keeps its exact operator on `Node.op` (e.g. `Cmp`→`"LessThan"`); a `Mux`'s three inputs are role-tagged on `Edge.mux_port` (`MuxPort { Sel, D0, D1 }`, alongside `MemPort`). All additive — `schema_version` stays `1`, `ingest` indexes them uniformly, and the default (flag-off) output is byte-identical. Schematic rendering is deferred to #157 PR3–5.
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
npm test             # Vitest (e.g. elk.test.ts). Default env is `node` (DOM-free);
                     #   tree.test.ts alone opts into happy-dom per-file (#171).
npm run tauri dev    # Tauri window + Vite HMR
npm run tauri build  # Bundle desktop app (Win/Linux/macOS)

# Python harness (from elaborate/, uv-managed)
uv sync
uv run pytest -q
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o <out.json>
# Opt-in gate-level projection (#157, ADR 0005) — decomposes process/assign
# expressions into gate/mux primitive nodes. Additive + off by default, so the
# default output above stays byte-identical:
uv run svxprobe-elaborate --top <top> -f <filelist.f> --gate-level -o <out.json>
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
  The **rkyv load cache (#21)** then landed (ADR 0003 Phase A / Option A): `ingest`
  caches the parsed `Document` in `.schemview_data/` and mmaps it on repeat launches,
  ~2.9× faster load at 100K (`load` bench `cache_hit` vs `from_slice`). The **wave_index
  cache (#153)** followed, persisting the matcher's output so a warm launch also skips the
  ~5–10s matcher pass — the dominant per-launch cost once the parse is cached. See
  `docs/ROADMAP.md`.

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
