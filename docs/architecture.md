# Architecture

How `hdl-schemview` is put together: the three trees, the data flow through them, what
each Rust crate owns, and the Tauri command surface the frontend talks to.

**Governing principle:** the elaborated hierarchy is the single source of truth. Source,
schematic and waveform are three *projections* of it — source and waveform map *to* the
model, the schematic *is* the model rendered. Cross-probing is therefore lookups, never
guesswork. Do not reintroduce string-matching where a model lookup exists.

Related: [data-model.md](data-model.md) for the node/DTO shapes ·
[frontend.md](frontend.md) for the `app/src` internals ·
[decisions/](decisions/) for the architecture decisions behind all of it.

## The three trees

```
core/        Rust workspace — model, ingest, matching, cross-probe, schematic, GUI logic, CLI
app/         Tauri 2 desktop app — vanilla TS + Vite frontend + thin Rust shell
elaborate/   Python (pyslang) elaboration harness — produces the golden model JSON
fixtures/    Committed golden hierarchy + VCD/FST traces (picorv32_soc)
docs/        Roadmap, runbooks, ADRs
flake.nix    Nix flake: packages/apps.svxprobe, checks.{fmt,clippy,test}, overlays, dev shells
```

## Data flow, end to end

```
SystemVerilog RTL
  └─ elaborate/ (pyslang)          → hierarchy.json (schema-validated model document)
       └─ core/crates/ingest       → Design (deserialize + referential-integrity check)
            └─ core/crates/model    → indices: path_index, src_index (interval tree), wave_index
                 ├─ schematic       → SchematicGraph (scope_graph / expand / cone / trace)
                 ├─ xprobe          → cross-probe resolution
                 └─ wave            → trace ValueChanges (wellen)
                      └─ gui        → Session + serializable DTOs (UI-toolkit-free, CI-testable)
                           └─ app/src-tauri/src/lib.rs  → #[tauri::command]s over
                                                          Mutex<HashMap<SessionId, Session>>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → CSS-grid panes + tab groups
```

## Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

### `model` — `svxprobe-model`

The spine. The elaborated node model plus the indices everything else resolves through:
`path_index` (canonical path → nodes), `src_index` (an interval tree over declaration
spans), `wave_index` (node → trace signal), and the three later additions —
`gen_map_index` / `src_map_index` (HLS C↔RTL provenance) and `name_ref_index`
(identifier occurrences).

`name_ref_index` is deliberately **not** merged into `src_index`: a usage span is finer
than the declaration enclosing it, so merging would change every narrowest-covering-node
result. Shapes are in [data-model.md](data-model.md).

### `ingest` — `svxprobe-ingest`

JSON → `Design` deserialization plus referential-integrity validation (reference ranges
and within-scope name uniqueness, whitelisting the port/backing-net dual-node pattern).

Two on-disk caches live under `<model dir>/.schemview_data/`:

| Cache | File | Header (staleness key) | Effect |
| --- | --- | --- | --- |
| Load cache | `<model>.rkyv` | `RKYV_FORMAT_VERSION` + `schema_version` + source `len`/`mtime_ns` | A fresh hit mmaps, bytecheck-validates and deserializes an owned `Document`, skipping the JSON parse (~3.8× faster load at 100K nodes) |
| Wave-index cache | `<model>.<trace>.waveidx.rkyv` | `WAVE_INDEX_FORMAT_VERSION` + both files' `len`/`mtime_ns` + a `MatchOptions` hash | A warm launch skips the matcher pass entirely |

Any miss, staleness or corruption falls back to the JSON path and rewrites the cache.
`build_cache` / `svxprobe cache` pre-warm it. The load cache is Option A of
[ADR 0003](decisions/0003-storage-backend-for-parse-scalability.md) — an owned
deserialize, *not* zero-copy.

`access_cache_checked` / `access_cache_unchecked` are **benchmark-only measurement
hooks**, not load paths: they stop after the mmap and header check (with and without
bytecheck) and return archived `(nodes, edges)` counts, never a `Design`. They exist so
`scale-bench` can price the `deserialize` step that a true zero-copy read-back would
remove, separately from the validation it would keep. The `unchecked` one is `unsafe`.

### `wave` — `svxprobe-wave`

VCD / FST / GHW trace loading via [wellen](https://github.com/ekiwi/wellen), lazy
per-signal.

### `matcher` — `svxprobe-matcher`

The canonical-path matcher that links model nodes to trace signals. **A ≥95% hit-rate on
the committed fixture is a hard PR gate** — see [fixtures.md](fixtures.md).

### `xprobe` — `svxprobe-xprobe`

The cross-probe engine: source ↔ waveform ↔ schematic.

- `CrossProbe::build` matches fresh; `build_cached` reuses the persisted `wave_index`
  when a fresh cache exists for `(model, trace, opts)`, else matches and writes one.
- `rematch` / `rematch_cached` re-match the *same, unchanged* design against a **new
  trace in place** — the trace-swap path behind a waveform pane's "Load trace…", so no
  model is re-ingested and no designlist re-elaborated. **They must clear the old
  `wave_index` first:** `run_match` only *inserts*, so a leftover mapping would keep
  resolving nodes to the previous trace's refs.
- **Usage resolution.** `from_source` resolves by span first (`resolve_source_range`). If
  that anchors a concrete **leaf signal** (`Var`/`Net`/`Port`) it is trusted — a
  declaration click resolves via its `def_range`, and a generate-unrolled declaration
  reaches every lane since they share one template span. Only when the span anchor is a
  process or gate block does it try `resolve_name_ref`: look up the identifier occurrence
  covering the offset, resolve its scope-relative `rel` against each elaborated instance
  of the module whose body contains the click, and hand the hits to `resolve_candidates`.
  So a click on `clk` inside an `always_ff` lands on the signal, with sibling instances as
  `alternatives`. It falls through to the span result on any miss, so a model elaborated
  without `--name-refs` behaves exactly as before.

### `schematic` — `svxprobe-schematic`

The layout-agnostic graph extractor. Four entry points, all returning a
`SchematicGraph`:

| Entry point | Produces |
| --- | --- |
| `scope_graph(_with)` | One scope's boxes and wires — the hierarchy view |
| `expand(_with)` | A box drilled open |
| `cone` | The legacy uncapped fan-in/out cone |
| `cone_with` / `trace_graph` | The capped, incremental trace view |

**Projections** ([ADR 0005](decisions/0005-optional-gate-level-projection.md)).
`Projection { ProcessLevel (default), GateLevel }` selects internal-logic granularity.
The bare `scope_graph`/`expand` delegate with `ProcessLevel`, so every pre-existing caller
and its output are unchanged. Under `GateLevel`, `child_boxes` dissolves each
combinational block into flat gate/mux children; `Ff`/`Memory`/instances stay opaque at
both levels. A model with no gate primitives renders identically either way, and the
harness must be run with `--gate-level` to emit them at all.

**Trace mode** ([ADR 0010](decisions/0010-schematic-trace-mode.md)).
`trace_graph(design, &[TraceStep], limits, projection)` walks an ordered list of expansion
steps; `cone_with` is one step of it. Key properties:

- It **re-derives rather than merges**. `PinAlloc` mints pin ids from a per-call counter,
  so the same `(box, signal)` gets a different id in a second call — two graphs' pins
  cannot be reconciled. One call per render is the only place ids are comparable, so the
  frontend holds the step list and the backend holds no trace state.
- What makes several steps *one* graph is shared walk state, not a merge pass:
  `emitted`/`emitted_pins` dedupe boxes, `PinAlloc` memoizes on `(box, signal)`,
  `seen_pairs` dedupes wires on the `(min,max)` pin pair, `visited` gates only the next
  hop, and — the load-bearing one — `stubs` is shared, so expanding a net's fan-in and
  then its fan-out re-uses one stub as a junction instead of drawing the net twice.
- `ConeLimits { depth, fanout, boxes }` caps the walk **visibly**, via `SchPort.more` and
  `SchematicGraph.truncated` — never silently. `TraceStep.fanout` overrides the shared
  `fanout` for one step (backing the "N more…" badge) and is deliberately **not** clamped
  to it; exceeding the shared cap is the point, and `limits.boxes` still bounds the graph.
  A stub is not charged against the box budget — it is a wire anchor, and starving it
  would return an empty graph rather than a small one.

**Shared anchoring.** `ScopeAnchors::for_scope` holds one scope's whole anchoring
vocabulary (`box_set`, `boundary_of`, `iface_pin`, `iface_owner`, `raw_port`).
`scope_graph_with` resolves every edge through its `resolve`; `cone_with`, which picks
boxes itself across many scopes, resolves through `pin_in_box` — the half of `resolve`
that stays true once the box is known. It is keyed on the *scope*, never a caller's
reached subset, because `resolve`'s bundle-member rule anchors only when **one** bundle
views the member, an arity test that answers differently against a subset. `box_set` is
cached apart from the rest, since the walk-up tests membership at every ancestor while
only a box that anchors a wire needs the (quadratic) `boundary_of` pass.

**Invariants worth knowing before you touch this crate:**

- `cone_with` never repeats a node or pin id, and every emitted box is a box the scope
  graph would also emit. Both are guarded by tests
  (`cone_with_never_repeats_a_node_or_pin_id`,
  `cone_with_agrees_with_the_scope_graph_on_what_is_a_box`).
- The bare `cone` **does not delegate** to `cone_with`. It is the `svxprobe graph --cone`
  output contract *and* the `scale-bench` fan-out baseline that ADR 0003's 190.8 ms
  finding is measured against — delegating to a capped implementation would overwrite that
  evidence. It also carries a known direction quirk: `Edge.dir` is relative to `e.port`
  and the legacy filter ignores which side the walk stands on, so for a net seed
  `cone(net, Dir::Out)` returns fan-in. `cone_with`'s `follows` flips the test on the near
  side and gets it right.
- `join_signal` (drivers × loads with `(min,max)` pin-pair dedup) backs both views, so
  they cannot drift on what counts as a connection.
- `is_navigable_scope` is the single scope-root predicate, `pub` and reused verbatim by
  `gui::is_tree_scope`, so the tree prunes exactly the blocks the schematic won't open.
  `is_bare_interface` and `pin_width` are `pub` for the same reason — the signal picker
  annotates rows with the same width rule the schematic annotates pins with.

### `gui` — `svxprobe-gui`

`Session` logic plus the serializable DTOs. **No UI toolkit**, so it is CI-testable.

- `scope_signals` / `SignalEntry` back the waveform panes' signal picker: the signals
  declared directly inside a scope, deduped by canonical path and answered *through* the
  cross-probe so a row's `kind`/`width`/`in_trace` cannot contradict the `probe_node` a
  click makes. It unions **every** structural node at the path — one path can carry
  several `GenBlock`s.
- `trace_graph` / `TraceStepReq` resolve each step's canonical path to a `NodeId`,
  default `depth` to one hop, clamp it to `limits.depth`, and **error** on a path that
  names no node rather than returning a quietly smaller graph.
- **Elaboration.** The harness spawn is one private `harness_command` behind two output
  shapes: `elaborate_to_json` (`-o -`, in-memory — what `Session::elaborate_and_load`
  uses) and `elaborate_to_file` (`-o <path>`, for the benchmark's `real` basis, streaming
  the harness's stderr line by line instead of withholding it until a multi-minute
  elaboration ends). Sharing them is what stops the app's designlist load and the
  benchmark model from drifting on `--gate-level` / `--name-refs`; the argv is pinned by
  a unit test, because the integration test self-skips when the harness is absent — i.e.
  on CI. `SVXPROBE_ELABORATE` overrides the `PATH` lookup.

### `cli` — `svxprobe`

The dev/test binary. Subcommands: `ingest`, `cache` (pre-build the rkyv load cache),
`wave`, `match`, `graph`, `probe`.

### `scale-bench`

Dev-only (`publish = false`): a deterministic synthetic-model generator, criterion
benches, per-process RSS scenarios, and a `collect` driver that produces one paste-ready
metrics file. The packaged app shares its code via `--bench`. Full runbook and the
measured findings are in **[benchmarking.md](benchmarking.md)**.

## The Tauri shell

`app/src-tauri/` (package `hdl-schemview-app`) is a thin `cdylib`/`lib` wrapping
`svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

The shell parses `std::env::args()` in `run()` **before** any window opens
(`svxprobe-gui::startup`): `-h`/`--help` → usage + exit 0; a usage error → stderr +
exit 2; a missing filelist/trace → stderr + exit 1; no args → normal GUI boot.
`startup::parse_launch` wraps that with a `Launch { Gui, Bench, BenchScenario }` enum —
the bench arms are recognized **only as the first argument** — and `bench::intercept`
handles them before `tauri::Builder::default()`, so a headless `--bench` constructs no
Tauri state. Errors reaching the UI are flattened with `fmt_err` (`{:#}`) so an anyhow
context chain arrives whole in the status pane instead of only its outermost layer.

Packaging for isolated environments is
[ADR 0009](decisions/0009-packaging-for-isolated-environments.md); the release
mechanics are in [releasing.md](releasing.md).

## Tauri commands

`app/src-tauri/src/lib.rs` ↔ `app/src/api.ts`. Every command delegates to a global
`AppState(Mutex<HashMap<SessionId, Session>>)`.

**Multi-session:** every command except `startup_args` takes an optional `session_id`;
omitting it targets the `"main"` session, so single-session behavior is unchanged. Each
independent waveform window passes its own id (its window label, e.g. `waveform-1`) to
load and query a separate trace of the same design. Main and schematic panes omit it.
Detached pane windows are created from the frontend via `WebviewWindow`, not a Rust
command; `capabilities/default.json` grants the window/webview permissions and scopes them
to the `main` / `waveform` / `waveform-*` / `schematic` / `schematic-*` labels.

| Command | Args (all also take optional `session_id`) | Returns |
| --- | --- | --- |
| `load_design` | `model, trace, excluded[], srcRoot` | `String` (top scope) |
| `elaborate_and_load` | `filelist, top, incdirs[], trace, excluded[], srcRoot, hlsSrc[]?` | `String` (top scope) |
| `load_trace` | `trace` | `()` — swaps the trace, reusing the ingested design |
| `unload_design` | — | `()` — drops the session; idempotent |
| `scope_graph` | `scope, projection?` | `SchematicGraph` |
| `expand_node` | `node` (id), `projection?` | `SchematicGraph` |
| `hierarchy_tree` | `scope, depth` | `TreeNode` (lazy; `expandable` beyond `depth`) |
| `scope_signals` | `scope` | `SignalEntry[]` |
| `cone` | `net` (id), `dir, depth` | `SchematicGraph` |
| `trace_graph` | `steps: [{path, dir, depth?, fanout?}], limits?, projection?` | `SchematicGraph` |
| `probe_node` | `path, context?` | `ProbeResponse \| null` |
| `probe_signal` | `fullName, context?` | `ProbeResponse \| null` |
| `probe_source` | `file` (id), `offset, context?` | `ProbeResponse \| null` |
| `signal_values` | `signalRef` | `ValueChange[]` |
| `source_text` | `file` (id) | `String` |
| `source_files` | — | `SourceFile[]` (each with a `language` tag) |
| `name_refs` | `file` (id) | `NameRefDto[]` — every identifier occurrence in the file |
| `trace_timescale` | — | `TraceTimescale \| null` |
| `startup_args` | — (no `session_id`) | `StartupArgs \| null` — parsed CLI launch args |

Notes on the less obvious ones:

- **`elaborate_and_load`** runs `svxprobe-elaborate` on a `.f` designlist with
  **`--gate-level` and `--name-refs` always passed**. Both are additive, but the frontend
  switches on data that must already be in the model (the gate-level toggle at
  `scope_graph` time; semantic coloring and usage-click resolution off `name_refs`) — so
  without them a designlist design shows combinational logic as opaque blocks even with
  the toggle on, and its source pane renders lexically only. When `hlsSrc` is non-empty it
  also passes `--hls-map` plus one `--hls-src` per entry.
- **`load_trace`** opens the trace before mutating, so a bad path leaves the session
  intact.
- **`scope_signals`** errors for a non-scope path. It pairs with `hierarchy_tree`: that
  lists the scopes, this lists what is *in* one.
- **`trace_graph`** seeds are canonical **paths**, not ids — that is what a pin, wire or
  box already carries and what survives a pop-out's `localStorage` snapshot. An
  unresolvable path errors rather than quietly returning a smaller graph.
- **`name_refs`** is a bulk feed (one call per file, not a probe per token) and is empty
  for a model elaborated without `--name-refs`.

> ⚠️ These serde types are the wire format for the frontend. Any field change must be
> mirrored in `app/src/types.ts` — see the sync rule in [data-model.md](data-model.md).
