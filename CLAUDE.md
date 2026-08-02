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
docs/        ROADMAP, fixtures policy, benchmarking runbook, release runbook,
             ADRs (docs/decisions/*)
             ADRs: 0001 RTL-vs-netlist · 0002 open-vs-FSDB · 0003 storage backend ·
             0004 internal-logic granularity · 0005 gate-level projection ·
             0006 HLS C↔RTL tracing · 0007 semantic name coloring ·
             0008 lexical source highlighting · 0009 packaging for isolation
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
                           └─ app/src-tauri/src/lib.rs  → 18 #[tauri::command]s over Mutex<HashMap<SessionId, Session>>
                                └─ app/src/api.ts         → typed invoke() wrappers
                                     └─ app/src/main.ts    → CSS-grid panes (#98) + tab groups (#99):
                                            tree │ #content    (source ↔ C/C++ ↔ schematic ↔ settings)
                                                 │ #bottom-group (status ↔ waveform)
                                          detachable: schematic-N / waveform-N pop-out windows (#169/#170)
```

### Rust crates (`core/crates/`, edition 2021, toolchain pinned to 1.94)

| Crate | Package | Purpose |
| --- | --- | --- |
| `model` | `svxprobe-model` | Elaborated node model + indices (`path_index`, `src_index` interval tree, `wave_index`). The spine. |
| `ingest` | `svxprobe-ingest` | JSON → `Design` deserialization + referential-integrity validation (ref ranges + within-scope name uniqueness, whitelisting the port/backing-net dual-node pattern). **rkyv load cache (#21):** `from_path` gates on a `<model dir>/.schemview_data/<file>.rkyv` archive (header = `RKYV_FORMAT_VERSION` + `schema_version` + source `len`/`mtime_ns`); a fresh hit mmaps + bytecheck-validates + deserializes an owned `Document` (Option A, not zero-copy), skipping the JSON parse; any miss/stale/corrupt falls back to JSON + rewrites. `build_cache`/`svxprobe cache` pre-warm it. **`access_cache_checked`/`access_cache_unchecked` (#155 measurement hooks)** stop after the mmap + header check — with and without bytecheck validation — returning the archived `(nodes, edges)` counts and *never* a `Design`: they exist so `scale-bench` can price the `deserialize` step a true zero-copy read-back would remove, separately from the validation it would keep. Not load paths; the `unchecked` one is `unsafe` and benchmark-only. **wave_index cache (#153):** `try_load_wave_index`/`write_wave_index` also persist the matcher's resolved `(NodeId, var_ref)` pairs in a sibling `.schemview_data/<model>.<trace>.waveidx.rkyv` archive (header = `WAVE_INDEX_FORMAT_VERSION` + both files' `len`/`mtime_ns` + a `MatchOptions` hash), so a warm launch skips the matcher entirely. |
| `wave` | `svxprobe-wave` | VCD/FST/GHW trace loader via `wellen` (lazy per-signal). |
| `matcher` | `svxprobe-matcher` | Phase-1 canonical-path matcher. **≥95% hit-rate is a hard PR gate.** |
| `xprobe` | `svxprobe-xprobe` | Cross-probe engine: source ↔ waveform ↔ schematic. `CrossProbe::build` matches fresh; `build_cached` reuses the persisted `wave_index` (#153) when a fresh cache exists for `(model, trace, opts)`, else matches + writes it. **`rematch`/`rematch_cached` (#176)** re-match the *same* (unchanged) design against a **new trace in place** — the trace-swap path behind a waveform pane's "Load trace…" (#170), so no model is re-ingested and no designlist re-elaborated. They clear the old `wave_index` first: `run_match` only *inserts*, so a leftover mapping would keep resolving nodes to the previous trace's refs. **Usage resolution (#225):** `from_source` first resolves by span (`resolve_source_range`); if that anchors a concrete **leaf signal** (`Var`/`Net`/`Port`) it is trusted — a declaration click resolves via its `def_range` *and* a generate-unrolled declaration reaches every lane (they share one template span). Only when the span anchor is a process/gate block does it try `resolve_name_ref`: look up the identifier occurrence covering the offset (`Design::name_ref_at`), resolve its scope-relative `rel` against each elaborated instance of the module whose body contains the click (the instances whose `def_range` covers the offset), and hand the hits to the existing `resolve_candidates` picker — so a click on `clk` inside an `always_ff` lands on the signal, with sibling instances as `alternatives`. Absolute refs (package params, cross-hierarchy) name one path directly. Falls through to the span result on any miss, so a model without `name_refs` is unchanged. |
| `schematic` | `svxprobe-schematic` | Layout-agnostic graph extractor: `scope_graph()`, `expand()`, `cone()`. **Trace extractor `cone_with` (#244):** the rebuilt cone, on the *same* box/pin machinery as the scope graph — `cone_box_of` replaces `nearest_instance` (which matched `Instance` only, so a cone into leaf-module logic returned **edges with no nodes**) by walking up to the first ancestor in its parent's `child_boxes`, so `GenBlock` dissolution, `Projection` and every box kind fall out of the scope graph's own predicate; both ends of every wire get real `PinAlloc` ids; `root` is the nearest `is_navigable_scope`; and `ConeLimits { depth, fanout, boxes }` caps the walk *visibly* via `SchPort.more` + `SchematicGraph.truncated` (never silent — ADR 0003's level-of-detail verdict). **Shared anchoring (#268/#269):** the *pin* side is now shared the same way the box side is. `ScopeAnchors::for_scope` holds one scope's whole anchoring vocabulary — `box_set`, `boundary_of`, `iface_pin`, `iface_owner`, `raw_port` — and `scope_graph_with` resolves every edge through its `resolve`, while `cone_with` (which picks each box itself, across many scopes) resolves the pin through `pin_in_box`, the half of `resolve` that stays true once the box is known; a `ScopeCache` gives the cone one per scope. It is keyed on the *scope*, never on a caller's reached subset, because `resolve`'s bundle-member rule anchors only when **one** bundle views the member — an arity test that answers differently against a subset. `box_set` and the full anchoring cache **apart**: the walk-up tests membership at every ancestor, while only a box that anchors a wire needs the rest, whose `boundary_of` pass is quadratic in a scope's ports × children. Three divergences fell out of the old hand-rolled copies, all fixed: a folded inverter and a `Const`/`Param` tie were drawn as **phantom boxes** (178 `Const` + 100 `Not` on one depth-3 gate-level trace) though both render *inline* — `cone_box_of` now asks `child_boxes` whether a gate is really dissolved (so a gate inside an opaque `Ff` stays inside it) and redirects a foldable `Not` to its consumer, while `is_drawn_inline` keeps both off the frontier and out of the stub path; and a bare interface bundle was emitted **edge-less**, its raw member being a `Var` that "a `Port` child of the box" could never name. A box is now committed only once a connection anchors on it, and a final pass drops anything still unwired (a pin carrying a `more` count stays — that count *is* the connectivity, reported). Guarded by `cone_with_agrees_with_the_scope_graph_on_what_is_a_box`, which re-derives each emitted box's own scope graph and demands membership — the general statement the two bugs lacked. `scope_graph`/legacy `cone` output stays **byte-identical** (verified against the golden), and the `nav` benchmark's `cone_with` is unchanged (227 boxes, ~5 ms). It also **corrects a direction bug**: `Edge.dir` is relative to `e.port`, but the legacy filter ignores which side the walk stands on, so for a net seed `cone(net, Dir::Out)` returns fan-in — `follows` flips the test on the near side. The bare `cone` therefore **does not delegate**: it is the `svxprobe graph --cone` output contract *and* the `scale-bench` fan-out baseline (`scenario.rs` `nav`, `benches/query.rs`, `bin/report.rs`) that ADR 0003's 190.8 ms finding is measured against — delegating to a capped implementation would overwrite that evidence. Reach the new one via `svxprobe graph --cone <net> --projection <process-level|gate-level>` and/or `--fanout <n>`; absent both flags the legacy path runs untouched. The shared `join_signal` (drivers × loads with `(min,max)` pin-pair dedup) backs both, so the two views cannot drift on what counts as a connection. `pin_width` is `pub` (#171) alongside `is_bare_interface`/`module_of` — the GUI's signal picker annotates rows with the same width rule (enum fallback included) the schematic annotates pins with, so the two can't disagree. `is_navigable_scope` (#184) is `pub` the same way — the single scope-root predicate: `scope_graph` resolves against it (rejecting a logic-only generate block), and `gui::is_tree_scope` *is* it, so the tree prunes exactly the blocks the schematic won't open. **Gate-level projection (#157 PR3, ADR 0005):** a `pub Projection { ProcessLevel (default), GateLevel }` selects the internal-logic granularity, taken by new `scope_graph_with`/`expand_with` entry points; the bare `scope_graph`/`expand` delegate with `ProcessLevel`, so **every existing caller and its output are unchanged** (a `_with` variant, not a new required arg — avoids churning ~40 call sites). Under `GateLevel`, `child_boxes` dissolves each combinational block (`Comb`/`Latch`/`Assign`) into its flat gate/mux children (`make_gate_box`: inputs west, one east output pin keyed `(gate,gate)`, the mux select tagged `PinRole::Sel` from `MuxPort::Sel`), wiring gate→signal and gate→gate through the existing signal-join pass (each gate self-drives its own id key so a non-root gate with no `out` edge still connects); `Ff`/`Memory`/instances stay opaque at both levels (their input-cloud decomposition is a deferrable later slice). A model with no gate primitives renders identically either way, and the harness must be run with `--gate-level` to emit them at all. **PR4 wires the plumbing:** `Projection` gains serde derives (kebab-case `"process-level"`/`"gate-level"`) and is re-exported through `gui`; `Session::scope_graph_with`/`expand_with` thread it (the bare `Session::scope_graph`/`expand` delegate with `ProcessLevel`), and the `scope_graph`/`expand_node` Tauri commands take an optional `projection` param (default `ProcessLevel`). **PR5 wires the frontend** (`app/src`): a Settings "Gate-level schematic" toggle (`prefs.ts` `loadGateLevel`/`saveGateLevel`, default off) makes `main.ts`'s `setScope`/`showInSchematic` pass `"gate-level"`; `elk.ts` `muxChild`/`gateChild` size the primitives (mux select on the south wall) and `main.ts` `renderMux`/`renderGate` draw the IEEE distinctive glyphs (flat-back-D AND, curved OR, notched XOR, N-variant output bubbles, Not/Buf triangles, datapath op boxes, wires meeting the shape directly with west input leads to the actual back/arch) — so gate-level is now user-reachable, not just plumbed. **Inverter folding (#157):** `is_foldable_not` collapses a single-fanout `~operand` (a non-root `Not` read by exactly one gate) — `child_boxes` drops the `Not` box and `make_gate_box`/the signal-join rewire the operand straight to the consumer's input pin, tagged `PinRole::Inv` (serialized `"inv"`) so `renderGate` draws an inversion bubble outside that pin instead of a separate inverter. A `~signal` that drives a scope signal directly (`assign y = ~a`, it has an `out` edge) or an inverter with >1 reader stays a standalone `Not`. **Constant/parameter tie values (#199):** a gate/mux/datapath operand that is a hard-coded literal (`a & 8'hFF`) or a parameter (`a & MASK`) now surfaces its value as a tie. The harness (`--gate-level`) emits a literal as a synthetic `Const` node (`kind=Const`, value on `Node.const_value`) with an operand edge, and wires a parameter operand to its real `Param` node (relaxed `_wire_input` guard) after stamping the elaborated value into that `Param`'s `const` (`_param_value` reads the symbol's `.value`, since a param `NamedValue` carries no `.constant`). The value rides **inline on the gate input pin**: `make_gate_box` sets `SchPort.constant` from the operand endpoint's `const_value`, and `renderGate`/`renderMux` draw it as a `const-label` just left of the pin's west wall (`elk.ts` reserves a west margin sized to the value), so the tie value is **traceable right at the gate** rather than a separate source box. `Const` is never a box (excluded from gates/boxes); no separate source node is synthesized for gate operands (instance-port ties keep theirs). **`'x` don't-care branches (`sel ? a : 'x`):** slang leaves `.constant` unset for an `'x` literal, so `_literal_value` reads the `SVInt` off `.value` — otherwise those branches vanished, leaving muxes with a missing input. A concatenation `{a, b, …}` or replication `{n{a}}` operand becomes a **`Concat` primitive** box (`_emit_expr` → `_emit_gate("Concat", …)`, drawn `{ }`) gathering its element expressions — so `mem_la_addr`'s `sel ? {next_pc[31:2] + …, 2'b00} : {reg_op1[31:2], 2'b00}` renders as a full 3-input mux (the nested `+` decomposes to an `Add` feeding the concat, `2'b00` ties inline). **Memory-array read operands (#206):** a gate/mux operand that reads an array element (`cpuregs[decoded_rs1]`) resolves via `_leaf_signal` to the whole array node; `_wire_input` now accepts a `Memory` endpoint (not just `Net`/`Var`/`Port`), so the branch wires to the array instead of vanishing — the index being the same fidelity simplification as a peeled bit-select. The schematic gives such a read a home: a `Memory` box **drives its own array node** in the signal-join (a synthesized east read-out `Dout` pin keyed `(mem,mem)`, added only when an in-scope gate loads the array), so the reader's wire reaches the memory glyph. This fixed the 6 `cpuregs`-read muxes that were the actual "incomplete muxes" (the #206 title said *function-call* operands, but the golden has none — the gap was memory reads). **`case` → priority-mux trees (#207):** the harness only walked *expressions*, so a `case` branch (`alu_out = alu_add_sub;`) arrived as a bare-signal RHS and emitted no gate, leaving its read signals readerless (6 of #202's 8 dangling outputs). `_gate_block` now runs a structural pre-pass (`_lower_cases`/`_emit_case_stmt`/`_emit_case_chain`) that folds each `case` into a right-leaning 2:1 `Mux` chain over the prior-assignment default (`alu_out = 'bx` → a `Const` tie on the terminal D0), reusing `_emit_mux`'s wiring — a one-hot `case (1'b1)` takes each item predicate directly as the `sel`, a general `case (expr)` synthesizes equality-`Cmp` selects (OR'd for comma-list items). The branch assignments it lowers are excluded from the flat pass via a `consumed` set keyed on **source-range offsets** (`_assign_key`), *not* pyslang wrapper `id()` — pyslang re-wraps a node per traversal, so `id()` is unstable across the two passes and its reuse is even hash-seed dependent (keying on `id()` made node output nondeterministic). **`if`/`else` → mux trees (#215):** the same pre-pass (now `_lower_stmts`/`_lower_seq`) folds an `if`/`else if`/`else` cascade into a right-leaning `Mux` chain (`_cond_chain` flattens the cascade slang nests as `ifFalse`; condition → `sel`, branch value → `d1`, rest-of-chain → `d0`), with an `else`-less `if` falling through to the prior write so a combinational `if` doesn't read as a latch. Two structural changes make it work: the pre-pass **recurses into branch bodies** (`_lower_branch`), which is what makes a `case` nested inside an `if` reachable at all (#207's walk was top-level only), and the **wire-out moved to the end of the block** keyed on each l-value's *final* value, so a nested construct feeds its enclosing `Mux` instead of driving the signal twice. An l-value's pending value is either `("assign", node, rhs)` (not yet emitted, so `_resolve_pending` marks it `consumed` only if the lowering actually uses it) or an already-emitted endpoint. On the fixture this took `Mux` 112 → 754 and the model 2079 → 3799 nodes. A knock-on in the schematic: `access_ports`' raw-access tally counted *any* edge into a bare interface's members, so the extra gate reads flipped a bundle's port side **in the process-level graph** — it now skips gate primitives (`is_gate`), making that placement projection-independent. **Remaining gap:** a genuine **function-call** operand still isn't decomposed (like the Div/Mod datapath deferral) — none occur in the committed golden. Additive: `schema_version` stays `1` (reuses `Mux` + `mux_port`, no schema change) and the flag-off output is byte-identical. (Frontend `renderSource` highlights the def's span **by line number** (#203): `SourceLoc` carries `line`+`end_line` and `source.ts` `highlightLineRange` lights `line..=end_line`, so Show-in-source lands on the right line regardless of the `def_range` byte-offset basis. This replaced an earlier byte-offset approach that assumed offsets were raw file bytes — wrong for the committed golden, whose offsets are LF-based, so deep constructs drifted up on a CRLF checkout. `lineStarts` is retained only for the source-*click* path `offsetAt`, which shares the same offset basis; a repo-wide **`.gitattributes eol=lf`** (#203) pins the RTL source and committed goldens to LF on every platform, so the working tree matches the LF `def_range` offsets — resolving the drift for both the highlight and click paths, and making golden regeneration deterministic on Windows.) |
| `gui` | `svxprobe-gui` | `Session` logic + serializable DTOs. No UI toolkit — CI-testable. **`scope_signals`/`SignalEntry` (#171)** back the waveform panes' signal picker: the signals declared directly inside a scope, deduped by canonical path and answered *through* the cross-probe (never re-derived), with `is_signal_kind` excluding `Param` (never traceable) and `Interface`/`Modport` (a modport port's children live in another scope). Unions **every** structural node at the path, unlike `hierarchy_tree`'s `find` — one path can carry several `GenBlock`s. **Elaboration (#255):** the harness spawn is one private `harness_command` (`--top`/`-f`/`-I`*/`--gate-level`/`--name-refs`/`--hls-map`+`--hls-src`*) behind two output shapes — `elaborate_to_json` (`-o -`, in-memory, what `Session::elaborate_and_load` is built on, hence no rkyv cache key) and `elaborate_to_file` (`-o <path>`, for the benchmark's `real` basis, which needs a real file for `from_path`'s `.schemview_data/`, and which streams the harness's stderr line-by-line instead of withholding it until a multi-minute elaboration ends — deadlock-free without a reader thread precisely *because* stdout isn't piped). Sharing them is what stops the app's designlist load and the benchmark model from drifting on those flags; the argv is pinned by a unit test, since `tests/session.rs` self-skips when the harness is absent — i.e. on CI. `SVXPROBE_ELABORATE` overrides the `PATH` lookup, needed because the isolated-machine runbook invokes the venv exe by absolute path so uv never re-resolves. |
| `cli` | `svxprobe` | Dev/test binary. Subcommands: `ingest`, `cache` (pre-build the #21 rkyv load cache), `wave`, `match`, `graph`, `probe`. |
| `scale-bench` | `scale-bench` | **Dev-only (#24, Phase 4).** Deterministic synthetic-model generator (`generate`/`build_design`/`synth_signals`, seeded SplitMix64) + criterion benches (`load`/`query`/`matcher`) + a `report` bin. Measures the eager load path, scoped queries, high-fanout `cone()`, and matcher at 665/100K/1M nodes. `publish = false`; 1M gated behind `SCALE_BENCH_FULL`. A **real-design basis** loads any elaborated model via `SCALE_BENCH_MODEL=<hierarchy.json>` (handles auto-derived by `derive_handles`) — e.g. `claude_verilog_test` (`soc_top`, ~7.2K nodes with `--gate-level --name-refs`) as a realism anchor. **Memory + zero-copy axes (#22/#155):** criterion can price latency but not *memory*, and #22's premise ("too large to materialize") plus #155's payoff (skip the owned copy) are both memory questions — so a `scenario` bin runs **one measured operation per process** (the only way peak RSS is attributable), reporting wall + peak/end RSS as one JSON line per run. Modes: `prepare` (materialize the JSON + time `build_cache`, the Phase-A analogue of #24's "DB build time"), `from_slice`, `cache_hit`, `access_checked`/`access_unchecked` (mmap + rkyv `access` with/without bytecheck, **no deserialize** — the #155 split), `nav` (scripted 32-scope walk + a high-fanout cone on the hottest net, measured **twice** — the uncapped legacy `cone()` and #244's capped `cone_with()` — reporting the query spread *and* the sustained working set; the synthetic fixture wires each flop as a **load** of `clk` (`Dir::In`, since `Edge.dir` is relative to `e.port`), and because the legacy filter is side-blind the synthetic bases pass `Dir::In` to it to traverse the identical star at the identical cost, while `golden`/`real` keep `Dir::Out` — an earlier `Dir::Out` fixture encoding made `cone_with` correctly return **zero** on every synthetic basis, leaving the fan-out cap unmeasured at scale), `match` (matcher vs signal count). Bases add `golden` (committed fixture) and `real` alongside the synthetic sizes; the model JSON is **copied to a temp dir** first, so `from_path`'s `.schemview_data/` never lands beside a committed fixture. RSS comes from `mem.rs` — dependency-free by design (hand-declared `K32GetProcessMemoryInfo` / `/proc/self/status`), since the benchmark must build offline from the existing lockfile. The `collect` bin drives scenarios + criterion + the `report` bin into one paste-ready `core/target/scale-bench/metrics-<stamp>.md`; a scenario that dies (1M OOM) is recorded as a row, because that failure *is* the #22 datapoint. The **nightly `scale-bench` job builds and runs it** (`--online`, for CI's cold registry cache) and uploads the metrics file as an artifact. **Orchestration in Rust (#240 tier 1):** a packaged app has no PowerShell, bash or working `python3` (the Windows `python3` shim resolves on PATH and fails on execution), so the matrix moved into the crate. `src/scenario.rs` holds what `src/bin/scenario.rs` used to (`parse_args(argv)` / `run` returning the JSON line / `main_with`), the bin is a shim over it, and the app's `--bench-scenario` arm calls the same `main_with` — three entry points, one contract. `src/collect.rs` is the driver: `run(CollectOptions)` spawns **one child per scenario** through an injected `ScenarioRunner` (the dev bin passes the sibling `scenario`; the app passes `current_exe() --bench-scenario`, since a bundle has no second executable), and `render(facts, records, notes)` is **pure**, which is what finally makes the output format testable — `tests/collect.rs` asserts section order, verbatim headers, the `**FAILED**` rows and the float-vs-integer number split that had no coverage in any language. `src/golden.rs` `include_bytes!`s the 2.1 MB fixture, replacing three identical `env!("CARGO_MANIFEST_DIR")` copies that resolved to the *build machine's* source tree: `golden` is the only basis measured against byte-identical input everywhere, hence §Findings' cross-run anchor. `src/bin/collect.rs` adds the two toolchain-bound layers (criterion via the `.ps1`'s `estimates.json` walk, mtime-filtered so a stale estimate can't leak in; the `report` bin); the packaged app has neither, so `collect::packaged_notes()` is the one place that "`criterion_ran: false` + why each layer is absent" pairing is decided, and it drives ADR 0009's mandatory single-shot banner. `build.rs` exists only to declare `SVX_BUILD_REV` `rerun-if-env-changed` — `build_provenance` reads it with `option_env!` at *compile* time and CI restores a cached `target/`, so without it a rebuilt bundle would claim a revision it was **not** built from. Cargo features `synth`/`golden`/`collect` are **all default-on** — so every existing test, bench and clippy run is unchanged — with `cargo check -p scale-bench --no-default-features` a PR gate for the lean shape; `wellen` cannot be gated out either way, since `svxprobe-matcher` depends on it. **#246 retired the three shell collectors** (`scale-bench-collect.{ps1,sh}`, `scale_bench_tables.py`) and repointed the nightly job at `collect` in one change, gated on a full-matrix parity run — separating them would have left nightly running unmaintained code or calling deleted scripts. That run also surfaced why the criterion layer must pass `--no-default-features --features synth,golden`: cargo builds a crate's *bins* for the bench profile, so a bare `cargo bench` tries to relink the running `collect.exe`, which Windows refuses (`os error 5`). Dropping the `collect` feature is the only thing that excludes it (`--benches` does not). **Designlist basis (#255):** `-f`/`-top`/`-I` (and `--model-out`) on **both** entry points are a front-end to `-model` — the parent elaborates once, writes a `hierarchy.json`, and runs the existing `real` basis against it; `collect::run`, `scenario.rs`, the record format and `render`'s measurement tables are untouched. The point is the **single argv builder**: `src/elaborate.rs` (`elaborate_basis`, gated on the `collect` feature, which now pulls in an optional `svxprobe-gui`) calls `svxprobe_gui::elaborate_to_file`, the same `harness_command` the app's own designlist load uses, so `--gate-level --name-refs` cannot drift between the model the tool loads and the model the benchmark measures — previously a sentence in the runbook and a manual step. Shared rather than duplicated per entry point because the scratch path, timing, printed hint and provenance row must be identical or two metrics files aren't comparable. Five cross-flag usage errors (`-f` with `-model`; `-f` without `-top`; `-top`/`-I`/`-model-out` without `-f`; `-f` with a `-bases` list omitting `real`) each exist because the alternative is a run that silently measures something else, and all are checked — along with the empty-`bases` bail and the `scenario`-binary lookup — *before* the multi-minute elaboration. `Elaboration::provenance()` fills a new `EnvFacts.elaborated` → `| elaborated from |` row carrying the node count the **harness itself** printed; it must equal the `real` row's `nodes`, which is what catches a *partial* model (a missing include dir elaborates most of a design, exits 0, and reports plausible numbers). A nonexistent `-top` needs no guard here: the harness exits 1 without writing the file. Runbook: `docs/benchmarking.md`. |

Tauri shell (`app/src-tauri/`, package `hdl-schemview-app`) is a thin `cdylib`/`lib`
wrapping `svxprobe-gui` + `svxprobe-schematic` + `svxprobe-wave`.

**Packaging for isolated environments (#240 tier 1, ADR 0009).** The deployment target
has no network, no toolchain and no package manager, so the bundle carries its own
runtime: `tauri.offline.conf.json` is a one-key **overlay** setting
`webviewInstallMode: fixedRuntime` for Windows (the payload path stays *out* of the base
config, which would otherwise break every contributor's `tauri build`), AppImage is the
supported **isolated-machine** Linux artifact — the `.deb` and `.rpm` (#260) are the
separate *"Linux (connected)"* tier, declaring WebKitGTK as a system dependency and
needing repo access — and macOS is ad-hoc signed (`signingIdentity: "-"`, which does
**not** clear Gatekeeper quarantine — `xattr -dr` is documented instead) and remains
**unverified end-to-end**. `src/console.rs` attaches a release build to its parent console
via hand-declared kernel32 externs (the `mem.rs` precedent, no new dependency): the
GUI-subsystem binary otherwise has no std handles, so the documented `-h`→0 / usage→2 /
bad-path→1 contract printed *nothing*. `startup::parse_launch` wraps `parse` with a
`Launch { Gui, Bench, BenchScenario }` enum — the bench arms are recognized **only as the
first argument**. `src/bench.rs` `intercept(Launch)` handles them (#240 tier 1, PR2b),
called from `run()` **before** `tauri::Builder::default()` so a headless `--bench`
constructs no Tauri state: `BenchScenario` is a verbatim hand-off to
`scale_bench::scenario::main_with` (the scenario CLI owns its own usage and exit codes),
while `Bench` mirrors the dev `collect` bin minus the two layers a bundle cannot have
(`collect::packaged_notes`), pointing `ScenarioRunner` at `current_exe()` with a
`--bench-scenario` prefix — the **self re-exec**, since a bundle carries no second
executable — and writing `metrics-<stamp>.md` to the invocation directory
(`startup::bench_out_path`). The parent stamps `SVX_BENCH_CHILD` on its children (inherited
env, so `ScenarioRunner` needed no change): a platform that strips the flag from the re-exec
would otherwise open one GUI window *per matrix row*, so a marked child that lands on the
GUI arm exits 3 instead. `--bench` also takes a **designlist** (`-f`/`-top`/`-I`, #255),
elaborating it via `scale_bench::elaborate` before any scenario runs — so a machine that
has the `elaborate/.venv` the runbook already tells it to build, but no way to hand a
filelist to the benchmark, is no longer stuck; a missing harness fails with
`harness_missing_message()` up front rather than half-way through a matrix. The app-level `bench` cargo feature is default-on; the lean
`--no-default-features` build drops `scale-bench` entirely (2.1 MB, the embedded golden) and
keeps the "compiled without the benchmark feature" refusal, which is what makes that message
truthful. Elaboration stays
out of the bundle (tier 2, gated on a PyInstaller spike): a missing harness now returns
`gui::harness_missing_message()`, which names the copy-a-`hierarchy.json` workflow rather
than only "install it". The Tauri command layer flattens errors with `fmt_err` (`{:#}`)
so an anyhow context chain reaches the status pane instead of only its outermost layer.

### Frontend (`app/`, vanilla TS + Vite 5 + Vitest, no UI framework)

| File | Role |
| --- | --- |
| `app/index.html` | CSS-grid pane layout (#98): top-left hierarchy tree, a vertical `#col-splitter` (#139, drags the `--tree-w` track), a draggable `#row-splitter`, plus two tab groups (#99): top-right `#content` (source ↔ **C/C++ source** ↔ schematic ↔ **settings** (#17), **source** active by default; the `#csrc-pane` tab + its `#csrc-file` selector stay `hidden` until `source_files` reports a non-SV file, #159) and bottom `#bottom-group` (status ↔ waveform, **status** active by default). Each `.tab-group` has a `.tabbar` header; a tab's own controls (zoom bar, marker readout/unit) ride in a per-tab `.tab-aux`. The `#status-pane` holds the log pane (#100, #94 4c): `#status-log` is a scrollable list of timestamped, level-tagged (`log-info`/`log-warn`/`log-error`) rows fed by `main.ts`'s `log()`. The `#settings-pane` (#17) is a preferences form (theme select, excluded-scopes input). The **Schematic** and **Waveform** tab buttons start `hidden` (#17); `activateTab` un-hides a tab when it's activated, so the toolbar `#show-schematic`/`#show-waveform` buttons (or an append/show-in action) reveal + focus the on-demand views. **Detach (#18 PR2):** a `⇱` `#pop-schematic`/`#pop-waveform` button rides in each on-demand tab's `.tab-aux`; the same page reloads with `?pane=schematic\|waveform&win=<label>` in a second Tauri window, where `body.detached`/`body.detached-<pane>` CSS collapses the grid to that one pane full-window (chrome, tree, tabs, pop-out hidden). **Independent schematic panes (#169):** each schematic pop-out gets a *unique* label (`schematic-1`, `schematic-2`, …) so multiple coexist, each parked on its own scope; main keeps its own schematic (no handover). **Independent waveform panes (#170):** the same for waveform (`waveform-1`, `waveform-2`, …) — main keeps its own waveform. A `#load-trace` ("Load trace…") button rides in the waveform `.tab-aux` of **every** window: in main it swaps main's own trace without re-entering the toolbar's design load; in a pop-out it swaps only that pane's trace. A `body.detached-waveform #load-trace` rule re-shows it inside a detached window (the blanket `body.detached .pop-btn` rule would otherwise hide it with the ⇱ detach button). **Per-pane signal picker (#171):** `#wave-picker` (`#wave-picker-tree` over `#wave-picker-sigs`) is a collapsible sub-column **inside `#wave-pane`**, toggled by `#wave-pick-btn` ("☰ Signals", in the waveform `.tab-aux`) or **Ctrl/⌘+B**; it needs the same `body.detached-waveform #wave-pick-btn` escape hatch as `#load-trace`. It lives *inside the pane* rather than in the window's `#hier-pane` column precisely so every pop-out inherits it with **no `body.detached` grid surgery** — `body.detached-waveform` already hands `#bottom-group` the whole window. `#wave-picker[hidden]{display:none}` must outrank its own `display:flex` (the `.tab-aux[hidden]` trick), or a "collapsed" picker still renders. Tree row CSS is `.tree`-scoped, not `#hierarchy`-scoped, so both trees are styled. |
| `app/src/main.ts` | UI logic + app state (graph, nav stack, selection, source cache, pinned waveform traces). Tabs (#99): `activateTab(panelId)` toggles `.active` within a `.tab-group` and redraws the now-visible view (schematic/waveform have 0-size containers while hidden — `refreshSchematic` re-fits a `schematicDirty` schematic, `redrawTracks` the canvas); `showInSource`/`showInSchematic`/`addToWaveform`/`jumpToScope` reveal the matching tab. Source/tree navigation: a tree row's single-click drives the schematic (`jumpToScope`), a **double-click** reveals the node in source (#164 — probes `node.path` → publishes a `["source"]` selection); a **left-click** in the source pane moves the highlight to just the clicked line (#163, `onSourceClick` — a lightweight DOM `.hl`-marker shift, not the whole-block highlight of #158), while right-click keeps the explicit schematic/waveform cross-probe menu. The tree double-click still routes through the selection bus (#18). Settings (#17): `initSettings` populates the `#settings-pane` controls from `prefs.ts` and wires them back — theme applies live (`setTheme`), excluded scopes take effect on the next load (`loadExcluded()` feeds the two load calls). Status/log (#100): `log(level, message)` appends a row to `#status-log` (via pure `formatLogEntry`/`formatTime` in `log.ts`), auto-scrolls, and on `error` brings the Status tab forward; design-load/parse progress + API errors route here. The pane is the **only** destination (#228) — the toolbar's compact `#status` echo is gone, so the toolbar carries inputs only and there is one timestamped, level-tagged record instead of two. **Detached windows (#18 PR2):** `windowMode = paneModeOf(location.search)` splits boot — `init()` runs the full app, `initDetached(pane)` boots a single-pane window (seeded from the main window's `localStorage` snapshot: `detach:<label>:scope` = a schematic pane's scope path (#169), `detach:<label>:wave` = a `WaveSnapshot` JSON `{load,waves,waveView,markers,waveUnit}` (#170), both per unique window label, with `enumMap` as `[value,name][]` pairs; `storeTrace`/`loadTrace` round-trip the Map). Each window knows its own `selfLabel` (from `?win=`; `main` otherwise). `popOut(pane)` writes the snapshot then creates a `WebviewWindow` — schematic (#169) and waveform (#170) each get a fresh unique label every call, so pop-outs are independent. **On pop-out `hideTab` (#205) closes the originating in-app tab** (hiding the tab button, falling back to Source/Status); the toolbar `#show-schematic`/`#show-waveform` button re-reveals it via `activateTab` — main keeps its own backend pane state either way (no handover; the old `markDetached`/`focusPane` mirror machinery is gone). **Per-pane waveform sessions (#170):** a waveform window's `selfLabel` *is* its backend `session_id` (`sid`; undefined elsewhere → the `main` session). `initDetached` calls `loadPaneSession(snap.load, selfLabel)` to load that pane's own design + trace, threads `sid` through `signalValues`/`traceTimescale`/`probeNode`. `loadTraceOnly()` (the `#load-trace` button → `@tauri-apps/plugin-dialog`'s `open`) calls `load_trace` (#176) for **this window's** session (`sid`, so main or one pane) on a newly picked VCD/FST/GHW *keeping the current design*, re-resolving each lane by its stored model `path` (`WaveTrace.path`) and dropping signals the new trace lacks — main then updates `state.loaded` + the toolbar `#trace` field, a pop-out re-persists its snapshot with the re-resolved lanes. Main drops a pane's session on `tauri://destroyed` via `unloadDesign(label)`. `state.loaded` (a `LoadSpec`: model JSON or designlist) captures what main loaded so a pop-out can boot the same design. `handleSelection` gates each pane on `ownsSelection({mode,self}, target, dest, [])`: source → main only; schematic/waveform → the window whose label matches `dest`, or main's own on a broadcast — so a pop-out never follows another (#169/#170). `appendResolved` re-resolves an addressed cross-probe against *this* window's trace by model path before appending (a `signal_ref` isn't portable across traces; the model node path is), and `appendWaveItem`/`waveformDestinations` build the right-click **Append to waveform ▸ [main window | waveform-1 | …]** flyout that addresses one specific pane. **Signal picker (#171):** `setupPicker` (wired from *both* `init` and `initDetached`) binds `#wave-pick-btn` + `Ctrl/⌘+B` (gated on a visible `#wave-pane`; persisted globally under `wavePickerOpen`, closed by default); `initPicker` builds the pane's tree via `createTree` on `state.top`, `showScopeSignals` lists a scope through `api.scopeSignals(scope, sid)`, and `pickSignal` is `probeNode(path, null, sid)` → the **existing** `addToWaveform` (so dedupe/enumMap/lane order/tab reveal are unchanged). `state.top` (the design top) is captured in `load()` and, in a pop-out, from `loadPaneSession`'s **return value** — previously discarded. The picker's tree deliberately does *not* `jumpToScope` (that broadcasts, and would drive main's schematic from a pop-out). After `loadTraceOnly` the design is unchanged, so the tree is **not** rebuilt (it would collapse the user's expansion state) — only the listed scope's `in_trace` flags are refetched. |
| `app/src/api.ts` | Typed wrappers over Tauri `invoke()`. |
| `app/src/types.ts` | DTO interfaces mirroring Rust serde types. |
| `app/src/elk.ts` (+ `elk.test.ts`) | `SchematicGraph` → ELK layout → SVG DOM. |
| `app/src/tree.ts` (+ `tree.test.ts`) | The hierarchy tree: pure `scopeFrames` (breadcrumb frames from a scope path) **+ `createTree({host, fetchChildren, onSelect, onActivate?})` → `{init, highlight, clear}`** (#171) — the DOM factory behind *both* the window's `#hierarchy` tree and every waveform pane's `#wave-picker` tree. `treeItems` is closed over **per instance**, so two trees coexist in one document (a module-level map let one tree's `init` orphan the other's rows and steal its highlight). `fetchChildren` is injected rather than importing `api`, so the module stays transport-free and each tree names its own session (`sid`). **The only DOM outside `main.ts`/`source.ts`** — `tree.test.ts` opts into `happy-dom` via a per-file `// @vitest-environment` docblock (as does `srcoffset.test.ts`), so every other suite keeps Vitest's faster DOM-free `node` env. |
| `app/src/wave.ts` (+ `wave.test.ts`) | Waveform geometry (time-window mapping, zoom/pan, segments, value-at-time, ruler ticks) + per-trace/ruler canvas drawing. `WaveTrace.path` (#170) carries the lane's canonical model node path so a pane can re-resolve the lane against a different trace (a `signal_ref` is trace-specific; the model path is the portable key). |
| `app/src/source.ts` (+ `source.test.ts`) | Source-pane rendering: `renderSourceInto` (line rows + `.ln` gutter, tokens from `syntax.ts` piped through `names.ts`) and `highlightLineRange` — the **line-number**-based def highlight (#203), which is why Show-in-source lands correctly regardless of the `def_range` byte-offset basis. Shared by the RTL and C/C++ panes, which is why #224 needed no `main.ts` change. |
| `app/src/csrc.ts` (+ `csrc.test.ts`) | HLS pane routing (#159): `isCLanguage` (the single list of C/C++ `language` tags — reused by `syntax.ts`'s `grammarFor`, so the pane a file renders in and the grammar it lexes with can't disagree) + `cSourceFiles` (the `SourceFile[]` subset that populates `#csrc-file`) + the `SourceLoc` → RTL-pane-or-C-pane decision. DOM-free. |
| `app/src/log.ts` (+ `log.test.ts`) | Pure helpers for the status/log pane (#100): `formatTime` (`HH:MM:SS`) + `formatLogEntry` (level + message → renderable entry). |
| `app/src/prefs.ts` (+ `prefs.test.ts`) | Settings preferences (#17): DOM-free `parseExcluded`/`formatExcluded`/`coerceExcluded` (excluded-scopes editor round-trip + default fallback) + thin localStorage wrappers (`loadExcluded`/`saveExcluded`; key `excludedScopes`). Theme stays under the existing `theme` key. |
| `app/src/schempick.ts` (+ `schempick.test.ts`) | Pure logic for the schematic pane's signal-tracing palette (#219, the `a`-key pop-up): `filterSignals` (case-insensitive name substring over a scope's `SignalEntry[]`, blank query → all) + `isTextEntryTag` (whether focus is in an input/textarea/select/contentEditable, so the bare `a` hotkey doesn't fire while typing) + `moveIndex` (clamped, no-wrap keyboard-nav index, floors at 0 for an empty list). DOM-free like `log.ts`/`prefs.ts`; `main.ts` owns the `#schem-palette` overlay wiring. |
| `app/src/bus.ts` (+ `bus.test.ts`) | The single cross-pane coordination path (#18). Right-click/tree handlers `publish` a `Selection` (a resolved `ProbeResponse` + which panes to reveal, or a scope path to drill); one `subscribe(handleSelection)` in `main.ts` drives the panes. Transport is Tauri app-global `emit`/`listen` inside the webview (so it also reaches detached windows, #18 PR2), a module-local fan-out in browser/tests — one channel, two transports, chosen by `__TAURI_INTERNALS__` presence. Pure builders `crossProbeSelection`/`scopeSelection` (both take an optional `dest` window label, #169) — plus `paneModeOf` (read `?pane=` mode) and `ownsSelection` (which window drives which pane, keyed on window id + selection `dest`, #18 PR2 / #169 / #170) — are unit-tested; the payload carries model lookups, never geometry. `ownsSelection` is now purely id/dest-keyed for the pane views: `source` → main only, while `schematic` (#169) and `waveform` (#170) share one branch — an addressed selection drives exactly the window whose `self` label equals `dest` (main is addressable as `"main"`), a broadcast drives only main's own pane, and pop-outs never follow a broadcast. |
| `app/src/syntax.ts` (+ `syntax.test.ts`) | Lexical source-pane tokenizer (#223, ADR 0008): `tokenizeLines(text, lang?)` → one `Token[]` per line (`keyword`/`type`/`number`/`string`/`comment`/`directive`/`systask`/`operator`/`plain`). A **whole-file** scan carries `/* */` block-comment state across line boundaries, so a multi-line comment highlights on every line. DOM-free like `log.ts`/`schempick.ts`. `grammarFor` keys off `SourceFile.language`: absent/null or an SV tag ⇒ the SystemVerilog grammar; a C/C++ tag ⇒ the **C/C++ grammar (#224)**; anything else ⇒ a keyword-less fallback that still lexes comments/strings/numbers (a safe default rather than mis-coloring an unknown language). The C tags come from **`csrc.ts`'s `isCLanguage`**, reused rather than re-listed, so the pane a file renders in and the grammar it highlights with can't disagree. A `Grammar` carries per-language lexer traits alongside its keyword/type sets — `directiveSigil` (`` ` `` vs `#`), `systask` (`$display`, SV only), `charLiteral` (`'a'`, C only — SV spends `'` on sized literals), `tickNumber` (`'d5`, SV only), and its own `num` matcher (SV sized literals vs C hex/binary/float/suffixes) — so **the C pane needed no `main.ts` change**: it already renders through the same `renderSourceInto`. **Scope is lexical only** — identifiers stay `plain`, because a lexer cannot tell a module from a signal without guessing; **model-driven semantic name coloring is layered on top by `names.ts` (#225)**, not by this tokenizer. **Invariant:** a line's tokens concatenate back to the original line exactly, which is what keeps the byte-offset cross-probe correct (unit-tested). |
| `app/src/names.ts` (+ `names.test.ts`) | Semantic name coloring (#225, ADR 0007): `applyNameRefs(lineTokens, refs)` overlays the model's identifier-occurrence spans (`NameRefDto`, from `api.nameRefs(file)`) onto `syntax.ts`'s lexical rows — each ref **splits only the `plain` token it lands in** into a `name-<class>` token (`name-signal`/`name-port`/`name-param`/…), so the lexer stays authoritative for keywords/comments/strings and the model for identifiers; neither overwrites the other. DOM-free like `syntax.ts`/`schempick.ts`. `main.ts` `renderSourceInto` fetches the spans once per file (**RTL pane only** — the C pane is lexical-only per ADR 0006, `CSRC_PANE.semantic=false`), caches them on the `state.source` entry, and pipes tokens through `applyNameRefs` before rendering, gated on the `semanticNames` pref (default on, `prefs.ts`, Settings toggle re-renders in place via `state.sourceView`). Colored via `--tok-name-*` theme vars + `.tok-name-*` rules (`style.css`, both themes). **Invariant:** the same line-concat property `syntax.ts` guarantees — `applyNameRefs` preserves it (unit-tested), so `srcoffset.ts` `lineColumn` and the byte-offset cross-probe stay correct. |
| `app/src/srcoffset.ts` (+ `srcoffset.test.ts`) | `lineColumn(lineDiv, node, offsetInNode)` (#223) — the caret→line-column DOM walk extracted from `main.ts`'s `sourceOffsetAt`. A rendered line used to be one text node (caret offset *was* the column); it is now a run of token nodes, so this sums the text length of the tokens before the caret's (skipping the `.ln` gutter) to recover the true column. Keeps `lineStarts[line] + column` — the offset the source cross-probe resolves through — correct. happy-dom-tested (per-file `// @vitest-environment`, like `tree.test.ts`). |
| `app/src/style.css` | Theme vars. Dark default; light via `:root[data-theme="light"]`, persisted in `localStorage`. The `--tok-*` vars (#223) theme the source-pane syntax tokens in both themes; `.tok-<cls>` rules color the spans `renderSourceInto` emits. |

Deps: `@tauri-apps/api`, `elkjs`. Schematic = SVG; waveform = canvas 2D. Right-click
a schematic box/pin/wire (or a source token) opens an action menu: **Append to
waveform** (stacks the signal as a new lane) / **Show in source**. The waveform pane
carries its own **signal picker** (#171, `Ctrl/⌘+B`) — a scope tree over that scope's
signals (`hierarchy_tree` + `scope_signals`, both on the pane's `session_id`), so a pane
picks its own lanes instead of waiting for another window to address them at it; a
signal absent from the pane's trace is **dimmed and inert**, not pruned. The
**schematic pane** carries the analogue (#171 → #219): pressing **`a`** over a live
schematic opens a lightweight `#schem-palette` search pop-up (`Esc` closes) scoped to
the schematic's *current* scope (`schematicScope()` = the top nav frame), wired once
per window (`setupSchemPalette` from `init` + the `initDetached("schematic")` branch).
It reuses `api.scopeSignals(scope, sid)` + the shared `.snode` row styling and
type-to-filter (`filterSignals`, arrow/Enter to navigate); selecting a signal
**publishes an addressed `["waveform"]` cross-probe to the main window** (the same
`crossProbeSelection(resp, ["waveform"], selfLabel, "main")` bus path as the right-click
"Append to waveform") — going through the bus rather than a local `addToWaveform` is
what lets a **detached schematic pop-out** (which has no waveform pane) still land the
trace in main. Selecting also **focuses the signal in the schematic** itself
(`focusSignalInSchematic` = `selectWire` + `scrollIntoView({block:"nearest"})`, a no-op
when already visible), so picking a signal in a large scope jumps to where it's drawn.
Live-update: both `setScope` **and** the `showInSchematic` cross-probe drill (which
bypasses `setScope`) call `refreshSchemPalette()`, guarded by a `paletteGen` token so a
slow `scopeSignals` for an old scope can't clobber a newer one. The pane
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
| `elaborate_and_load` | `filelist, top, incdirs[], trace, excluded[], srcRoot, hlsSrc[]?` | `String` (top scope) — runs `svxprobe-elaborate` (on PATH) on a `.f` designlist **with `--gate-level` and `--name-refs`** (like the committed golden), then loads. Both flags are always passed: they are additive, but the frontend switches on data already in the model (the gate-level toggle at `scope_graph` time; semantic coloring + usage-click resolution off `name_refs`), so it must already be there — else a designlist design shows combinational logic as opaque Comb/Assign blocks even with the toggle on, and its source pane renders lexically only with usage clicks falling back to the enclosing block (#225). **`hlsSrc` (#222)** is the optional list of declared C/C++ sources / search roots; when non-empty the shell also passes **`--hls-map`** plus one `--hls-src` per entry. Before #222 `--hls-map` was never passed here, so a designlist-loaded HLS design produced no `source_map` at all and could not cross-probe to its C sources; a pure-RTL designlist still skips the scan entirely. |
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
| `source_files` | — | `SourceFile[]` (#159) — every source file + `language`; the frontend reveals the **C/C++ source pane** when a non-SystemVerilog file exists (an HLS design) |
| `name_refs` | `file` (id) | `NameRefDto[]` (#225) — every identifier occurrence in a file (`{ line, col, len, cls }`), the bulk feed for the source pane's semantic coloring (one call per file, not a point probe per token). Empty for a model elaborated without `--name-refs` |
| `trace_timescale` | — | `TraceTimescale \| null` (factor + normalized unit) |
| `startup_args` | — (no `session_id`) | `StartupArgs \| null` (#136) — CLI launch args (`-f/-top/-I/-trace/-src-root`) parsed by the shell before the window opened; the frontend prefills the load form + auto-loads |

The shell parses `std::env::args()` in `run()` **before** any window (see
`svxprobe-gui::startup`): `-h`/`--help` → usage + exit 0; a usage error → stderr + exit 2;
a missing filelist/trace → stderr + exit 1; no args → normal GUI boot.

## Key data structures

**Schematic** (`core/crates/schematic/src/lib.rs`):
- `SchematicGraph { root, nodes: Vec<SchNode>, edges: Vec<SchEdge> }`
- `SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>, module: Option<String>, constant: Option<String>, modport: Option<String>, mem_depth: Option<u32>, init_source: Option<String> }` — `modport` (#106) marks an `Interface` node as a modport-qualified *port's* bundle. The frontend draws it as a **square frame pin** (#125) at the view boundary — a small teal square labeled `bus (mem_if.mem)` sharing the `_SEPARATE` frame column with the scope's own boundary `Port` pins (design-facing walls flush), every wired member wire anchored at the square (`FIXED_POS` wall-centre ports) before fanning out with its net label; members with no in-scope edge (e.g. unread `instr`/`addr`) are omitted from the pin. Interface *instances* (`modport == null`) keep the hexagon bundle box, and are now **drillable** (#97): a bare bundle with `Modport` children reports `expandable`, and `scope_graph`/`expand` on its path returns an `interface_interior` — each `Modport` view as a box (kind `Modport`), one directional pin per member (side from the member's `dir`, `path`/width from the underlying bundle signal via `Design::modport_member_nodes`), a wire for every member one view drives and another reads (`net_path` = the member signal), the interface's own ports (`clk`) as boundary frame pins wired into the views that read them, and a per-view boundary frame port (`MODPORT_FRAME_BASE + modport`, kind `Port`, `bundle`) marking each view's external face, wired to its box. The drillability predicate `is_bare_interface` is `pub` in the schematic crate and reused by `gui::is_tree_scope` (single source of truth), so the hierarchy tree lists the bundle too; double-clicking the bundle box (caret `▸`) descends into the views.
- `SchPort { id, name, side: Side, path: String, width: Option<String>, role: Option<PinRole>, bundle: bool, dangling: bool, constant: Option<String> }` — `constant` (#199) is the inline tie value drawn just outside the pin's west wall when the operand is a literal or parameter. — `path` is the pin's canonical model path (empty for synthetic const pins) so a right-click cross-probes it; `bundle` marks a whole-interface pin (#106 consumer bundle pin, #96 access ports), drawn square instead of the directional triangle; `dangling` (#118) marks a pin nothing connects to (an instance port with no model edge and no constant tie-off, or a logic-box output no in-scope box reads) — shown dimmed instead of pruned, and a dangling FF Q gets an in-box name label since no wire labels it. **A dangling gate output (#202)** is relabelled with its driven net (the gate's `out`-edge endpoint `name`/`path`) in the dangling-marking pass, and `renderGate`/`renderMux` float that name just past the east wall — so a root gate whose signal has no in-scope reader (e.g. `mem_busy`, correct #118) keeps a labelled, cross-probeable floating wire instead of an anonymous stub. (#207 closes the ALU gap: the harness now lowers `case` statements into priority-mux trees, so those consumers wire to `alu_add_sub`/`alu_shl`/`alu_shr` instead of leaving them readerless; #215 does the same for `if`/`else`, so a branch condition is drawn as a mux select instead of being dropped.) `width` like `[31:0]`, else `None` (with an enum-table width fallback, e.g. `lane_state_e` → `[1:0]`); `role` (`PinRole { Clk, Reset, Enable, Addr, Din, Dout, Write, Read, Sel, Inv }`, #59/#112/#157 — `Sel` is a mux's select input, placed on the trapezoid's south wall from the `MuxPort::Sel` edge role; `Inv` marks a folded inverter, drawn as a bubble on the pin, whose `path` stays the *un-inverted* operand so cross-probe still lands on it) tags a synthesized FF/latch pin from the model facts (`Node.type_` clock name / `Node.reset` / `Node.enable`) — the frontend's `ffRole` prefers it over its name-regex fallback — or a MEMORY glyph pin (`Addr`/`Din` west, `Dout` east; `Write`/`Read` enables reserved for #157) from the `Edge.mem_port` role. A `Memory` `SchNode` also carries `memDepth`/`initSource` (the array label + INIT tab); `elk.ts` `memoryChild` + `main.ts` `renderMemory` draw the array-stack glyph. A bare interface instance bundle carries **aggregate access ports** (#96) instead of member pins, read off the connection edges: one port per consuming modport view (named after the view, id/path = the `Modport` node, wired straight to the consumer's #106 bundle pin) plus one raw port (named after the interface type, synthetic id, path = the instance) whose direct member taps draw as **one trunk wire per consumer wall** (#117): the backend still emits one `SchEdge` per member tap (the cross-probe truth), but the frontend collapses each (raw port, consumer box, wall) group into a single ELK edge anchored at a representative member pin (`trunkGroups`/`gatherBar` in `elk.ts`) and re-fans the members via a gather bar just off that wall — stubs cross-probe per member, the trunk/bar cross-probe the bundle, and selection links both ways (bundle lights its stubs; a stub lights itself + trunk, never siblings); the interface's real `Port` children (e.g. `clk`) stay ordinary pins.
- `SchEdge { id, source, target, net: Option<String>, net_path: Option<String> }` — `net_path` is the connecting net's canonical model path (absolute, no bit-select), so a wire click cross-probes via `probe_node`; `None` for synthetic constant tie-offs.
- `Side { West, East }` — drives ELK port placement.

**Model** (`core/crates/model/src/lib.rs`):
- `NodeId` = `u32` index into `Document::nodes`.
- `NodeKind { Instance, Net, Port, Var, Param, ModuleDef, GenBlock, Ff, Comb, Latch, Assign, Interface, Modport, Memory, And, Or, Xor, Xnor, Nand, Nor, Not, Buf, Add, Sub, Mul, Cmp, Shift, Mux, Const, Concat }` — `Interface` is an interface instance or a modport-specialized interface port; `Modport` a named view of a bundle; a `GenBlock` is one *elaborated* generate branch — the harness drops uninstantiated branches (`_walk` gates on slang's `isUninstantiated`, #178) so a discarded `if`-branch neither reparents its phantom logic (its `always @(posedge clk)` would double-drive the live `else`'s target) nor collides on the shared LRM-implicit name (`genblk1`) that made one path carry several nodes. A `GenBlock` is a **navigable scope** (a tree row with its own schematic) only if its subtree holds a real design object — an `Instance` or a bare `Interface` (#184, `is_navigable_scope`, `pub` in the schematic crate and reused verbatim by `gui::is_tree_scope` so tree and schematic agree); a **logic-only** block (`comb`/`ff`/`assign` only, e.g. `core.genblk1/2/3`) is a syntactic wrapper whose contents already render dissolved into the enclosing module (`child_boxes`), so `hierarchy_tree`/`scope_graph`/`scope_signals` reject it and a cross-probe onto its path walks *up* to the parent (`showInSchematic` already retries ancestors). `g_lane[0]` is a `GenBlock` too but holds instances → stays navigable; the test is contents, not the generate keyword. `Memory` (#112) a memory array (`logic [W-1:0] ram [0:N-1]` — the unpacked-dimension `Var`, re-kinded) drawn as a MEMORY glyph. A `Memory` node carries `mem_depth` (word count) and `init_source` (`$readmemh`/`$readmemb` arg text → INIT marker; `initial` stays non-logic per ADR 0004). Its addr/din/dout pins come from typed `Edge.mem_port` (`MemPort { Addr, Din, Dout }`) edges the harness emits by classifying `ram[idx]` accesses (addr = index expr, din = written value, dout = read target). Process-granularity per ADR 0004 (amended) — the glyph maps to the array's `def_range`, so cross-probe stays a lookup. The **gate-level primitives** (`And`…`Mux`, #157/ADR 0005) are emitted only by the harness's opt-in `--gate-level` pass (PR1) and now carried by the model spine (PR2): each is a flat child of its process/assign node with a sub-expression `def_range` (the scoped ADR-0004 relaxation), associative chains collapse to one N-input gate, `~` folds onto the base gate, and a `?:` becomes a `Mux`. A datapath primitive keeps its exact operator on `Node.op` (e.g. `Cmp`→`"LessThan"`); a `Mux`'s three inputs are role-tagged on `Edge.mux_port` (`MuxPort { Sel, D0, D1 }`, alongside `MemPort`). All additive — `schema_version` stays `1`, `ingest` indexes them uniformly, and the default (flag-off) output is byte-identical. Schematic rendering is deferred to #157 PR3–5.
- `Node { id, name, path, parent, children, kind, symbol_key, def_range, inst_range, type_, dir, const_value, modport, drivers, loads, mem_depth, init_source, op, reset, enable, members }` — `mem_depth`/`init_source` are the `Memory` glyph's word count and `$readmemh`/`$readmemb` INIT text (#112); `op` keeps a datapath primitive's exact operator (e.g. `Cmp`→`"LessThan"`, and `Divide`/`Mod`/`Power` on a `Mul`-kind node), so the label is a model fact rather than a re-derivation. — `modport` records the view name on a modport-specialized interface port (e.g. `mem`). Such a port carries directional `Port` children (one per modport member, direction from slang's `ModportPort`); each pin's `path` is the *underlying member's* canonical path (the pin is a view of that signal), wired to it by an edge. Bare interface instances stay member-pin-less.
- `Design { doc, path_index, src_index, conn_index, wave_index, gen_map_index, src_map_index, name_ref_index }` — the last three are the #159/#225 additions: per-file `Lapper`s for the C→RTL and RTL→C provenance maps, and for identifier occurrences. `name_ref_index` is deliberately **not** merged into `src_index` (a usage span is finer than the declaration enclosing it, so merging would change every narrowest-covering-node result).
- `Document.enums: HashMap<String, EnumDef>` — normalized enum table keyed by canonical type string (matches `Node.type_`); `EnumDef { width, members: Vec<EnumMember{name, value}> }`. Looked up via `Design::enum_for_type` and surfaced on `WaveLink.enum_map` for FSM state-name display.
- **HLS C/C++ ↔ RTL source tracing (#159, ADR 0006):** `FileEntry.language` tags each source file (`"systemverilog"` / `"c"` / `"cpp"`; `None` ⇒ SV) and `Document.source_map: Vec<SourceMapEntry { generated: Range, source: Range }>` is the bidirectional line-region provenance map — both **additive, `schema_version` stays 1**, both emitted only by the harness's opt-in `--hls-map` pass (which regex-scans the generated RTL's `// foo.cpp:N` provenance comments), so default output is byte-identical. `Design` builds `gen_map_index`/`src_map_index` (per-file `Lapper`s, symmetric to `src_index`); `mapped_from_gen`/`mapped_from_src` are the C↔RTL lookups. `CrossProbe::from_source` redirects a C-file offset through `src_to_gen` to the generated-RTL span and resolves the **narrowest node it overlaps** (`resolve_source_range`/`nodes_in_source_range`), so a C click yields a normal node-anchored `ProbeResponse` — the C pane inherits the schematic/waveform cross-probe. `ProbeResponse.mapped_source` carries the C counterpart of the RTL anchor's span (via `mapped_from_gen`), so one probe highlights both panes. Frontend: `source_files()` → the on-demand **C/C++ source tab** (`#csrc`, reuses `renderSourceInto`); `csrc.ts` `isCLanguage`/`cSourceFiles` route each `SourceLoc` to the RTL (`#source`) or C (`#csrc`) pane by language; a C-line left-click traces to RTL, right-click cross-probes. Fixture: `fixtures/hls_min/` (incl. `hls_min.f`, #222). C is **display-only** — never parsed; the correspondence is always the tool's own provenance, never inferred. **Declared C sources (#222):** `--hls-src` (repeatable) takes a *file* — registered eagerly via `_register_declared_c_sources` so a header no comment references is still browsable, even with zero `source_map` entries — or a *directory*, used as a search root (never bulk-imported). `_resolve_c_ref` replaces the old single `join(rtl_dir, c_ref)` guess with an ordered ladder: absolute-and-exists → each declared root → a declared source whose **basename** matches *only when exactly one does* (several ⇒ warn to stderr and fall through, never silently pick) → the RTL's own directory, so declaring nothing resolves exactly as before. Rung 1 falling through on a missing path is what rescues a vendor-baked absolute path from the build machine. Registration runs **after** `_scan_hls_provenance`, whose RTL snapshot would otherwise tag the declared C files `systemverilog`.
- **Semantic name coloring / usage resolution (#225):** `Document.name_refs: Vec<NameRef { file, line, col, offset, len, class: NameClass, rel }>` records every identifier occurrence — declaration name tokens and resolved value references — each classified (`NameClass { Module, Instance, Port, Signal, Param, Type, EnumMember, Function, Interface, Modport, Genvar }`) off the symbol the elaboration resolved, never the token text. **Additive, `schema_version` stays 1**, emitted only by the harness's opt-in `--name-refs` pass (which keeps the `sourceRange` that `_value_refs` computed and discarded), so default output is byte-identical. `rel` is the symbol path **relative to the enclosing elaborated instance** (`clk`, `g_lane[0].bus.valid`), so one source span serves every instantiation; an out-of-scope symbol (a package param) is stored absolute with a leading `/` (a char SV paths never use, `NameRef::absolute_path`). Records dedup by `(file, offset)` keeping the shortest `rel` (the innermost enclosing scope). `Design` builds a per-file `name_ref_index` (`Lapper`, symmetric to `src_index` but **kept separate** so a usage span — finer than the enclosing declaration — never perturbs `src_index`'s narrowest-covering-node result); `name_ref_at`/`name_refs_in_file` are the lookups. Adding this field bumped `ingest`'s `RKYV_FORMAT_VERSION` 1→2 (the archived `Document` layout changed). Surfaced to the frontend via the `name_refs` command (`NameRefDto { line, col, len, cls }`, `NameClass::as_str` the single kebab-case source) and consumed by `names.ts` (coloring) + `xprobe::from_source` (usage → signal). **`elaborate_and_load` always passes `--name-refs`** (like `--gate-level`, #214 pattern), so a designlist-loaded design gets the spans too — without it, coloring and usage clicks silently no-op'd on that path.
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
                     #   tree.test.ts + srcoffset.test.ts opt into happy-dom per-file.
npm run tauri dev    # Tauri window + Vite HMR
npm run tauri build  # Bundle desktop app (Win/Linux/macOS)

# Python harness (from elaborate/, uv-managed)
uv sync
uv run pytest -q
uv run svxprobe-elaborate --top <top> -f <filelist.f> -o <out.json>
# Sources can also be positional, with -I/--include for include dirs — this is the form
# ci.yml's golden regeneration uses (an explicit file list, since a *.sv glob would skip
# picorv32.v and globs don't expand in PowerShell):
uv run svxprobe-elaborate --top <top> -I <incdir> <file.sv> <file2.v> -o <out.json>
# Opt-in gate-level projection (#157, ADR 0005) — decomposes process/assign
# expressions into gate/mux primitive nodes. Additive + off by default, so the
# default output above stays byte-identical:
uv run svxprobe-elaborate --top <top> -f <filelist.f> --gate-level -o <out.json>
# Opt-in HLS C/C++ ↔ RTL provenance map (#159, ADR 0006) — scans the generated RTL's
# line-annotated comments (e.g. `// foo.cpp:42`) for the originating C source and emits a
# `source_map`. Additive + off by default (no `language`, no `source_map` ⇒ byte-identical).
# `--hls-comment-re REGEX` (named groups `file`+`line`) overrides the comment pattern:
uv run svxprobe-elaborate --top <top> -f <filelist.f> --hls-map -o <out.json>
# Declare where the C/C++ sources live (#222) instead of relying on the provenance
# comment's path resolving next to the generated RTL. A FILE is registered even if no
# comment references it (an unmapped header stays browsable); a DIRECTORY is a search
# root used when resolving a comment's C path. Repeatable; only used with --hls-map:
uv run svxprobe-elaborate --top <top> -f <filelist.f> --hls-map \
    --hls-src src/foo.cpp --hls-src src/ -o <out.json>
# Opt-in identifier-occurrence spans (#225) — every declaration name token and every
# resolved value reference, so the source pane can color identifiers by kind and a click
# on a *usage* resolves to the signal it names (the model otherwise carries declaration
# spans only). Additive + off by default (no `name_refs` key ⇒ byte-identical):
uv run svxprobe-elaborate --top <top> -f <filelist.f> --name-refs -o <out.json>

# Scalability benchmark (#22 / #155) — see docs/benchmarking.md for the full runbook.
# One command; writes core/target/scale-bench/metrics-<timestamp>.md, the paste-ready
# deliverable. Everything is --offline, so it runs on an isolated machine. This is
# also what nightly CI runs (with --online, since a cold registry cache makes the
# default --offline fail) and the code the packaged app's `--bench` shares:
cargo build --release -p scale-bench --offline
./target/release/collect [--full] [--model <hierarchy.json>] [--bases "golden 665"] \
    [--skip-criterion] [--skip-report] [--online] [--out <path>]
# The `real` basis from a designlist instead of a pre-elaborated model (#255) — the
# collector elaborates it through the app's *own* argv builder, so --gate-level and
# --name-refs cannot be forgotten. Mutually exclusive with --model; the elaborated
# JSON's path is printed so a later run can reuse it (or --model-out to keep it):
./target/release/collect [...] --filelist <design.f> --top <name> [--include <dir>]... \
    [--model-out <path>]
# The same matrix from the packaged app (#240 tier 1, PR2b) — no cargo, no second
# executable: it re-execs itself per scenario and writes metrics-<stamp>.md into the
# invocation directory. No criterion/report layers there, so the file carries the
# single-shot banner. `--no-default-features` drops the bench code and refuses instead:
hdl-schemview --bench [--full] [-model <hierarchy.json>] [-bases "665 100K"] [-out <file>]
hdl-schemview --bench [--full] -f <design.f> -top <name> [-I <dir>]... [-model-out <path>]
# When the harness is not on PATH (the isolated machine invokes the venv exe by
# absolute path so uv never re-resolves), both entry points honour:
#   SVXPROBE_ELABORATE=<path to svxprobe-elaborate>
# Individual pieces (from core/): one measured operation per process, so peak RSS is
# attributable — `prepare` first, then any measured mode:
./target/release/scenario --basis <665|100K|1M|golden|real> \
    --mode <prepare|from_slice|cache_hit|access_checked|access_unchecked|nav|match>
# The lean feature shape a packaged build may ask for (a PR gate in ci.yml):
cargo check -p scale-bench --no-default-features
```

Fixtures: `fixtures/picorv32_soc/` (committed golden + VCD/FST). PR-gate tests run
against them — **no Verilator regeneration needed** for normal work. The tiny
`fixtures/hls_min/` (#159) is a synthetic HLS design (`foo.cpp` + generated-style `foo.sv`
with `// foo.cpp:N` provenance comments + `--hls-map` golden) exercising C↔RTL tracing. See
`docs/fixtures.md` for the two-tier policy and pinned tool versions.

**CI:** `ci.yml` — Rust (fmt, clippy, test, match gate on FST+VCD) + Python (`ruff`,
pytest, schema validation, an **RTL `always_ff` driver lint** — VCS rejects a variable
written by `always_ff` that has any other procedural driver, IEEE 1800 §9.2.2.4, while
slang and Verilator accept it — and **golden reproducibility**, which re-elaborates
`fixtures/picorv32_soc/golden/hierarchy.json` with **`--gate-level --name-refs`** and
diffs it), Ubuntu, on push/PR. `app.yml` — **three** jobs, when `app/` or `core/crates/`
change: `build` (`npm test` + `npm run build` + `cargo build` on Ubuntu + Windows, the
fast PR signal) and **`bundle`** (#240 — a real `tauri build` on Ubuntu + Windows +
**macOS**, uploading each artifact; `cargo build` never exercises the bundler, so
NSIS/AppImage/`.app` breakage would otherwise surface only at release). The Linux leg
builds `appimage,deb,rpm` — **`.rpm` since #260**, which needs no `rpmbuild` (Tauri 2
writes the archive through the pure-Rust `rpm` crate). **Both** Linux packages are
smoke-tested by `.github/scripts/linux-package-smoke.sh <deb|rpm>` (#261), which
installs each in a **clean container** — Ubuntu 24.04 to match the build host's glibc,
Fedora unpinned so a package rename surfaces here — and asserts four things: the
package declares WebKitGTK, it installs, the launcher runs headlessly (`--bench`), and
its `.desktop`/icons land where a launcher finds them. A container is required because
the runner itself already has WebKitGTK from the build deps, so a local install would
pass whatever `depends` declares — and unset, Tauri emits an RPM with *no* dependencies
at all, which installs and then cannot launch. The launcher is read back out of the
payload rather than assumed: Tauri installs the **crate** binary (`hdl-schemview-app`),
not one named after `productName`, so every doc that says `hdl-schemview` names a
command that does not exist on a package install (#275). `bundle` skips pull requests, since macOS bills at 10× minutes on
a private repo — so a packaging change is proven by dispatching the workflow on its
branch instead, and `workflow_dispatch`
takes a **`bundle_os`** input (#260) narrowing the matrix to one OS (~5 billed minutes
instead of ~49); it applies to dispatch only, so pushes and tags always build all
three. It needs the
`WEBVIEW2_CAB_URL` repo variable for the Windows fixed-runtime payload (set
2026-08-01; since `release` is `needs: [build, bundle]`, an unset variable fails the
Windows leg and publishes nothing rather than a partial release). Extracting that
`.cab` **must call `expand.exe` by full path**: the step runs under Git Bash, where a
bare `expand` resolves to GNU coreutils' tabs-to-spaces filter and dies on `-F:`,
which is what broke every Windows bundle up to 2026-08-01. The third,
**`release`** (#248), fires only on a `v*` **tag** push — the trigger added alongside
`branches: [main]`, which the `paths:` filter does not suppress because GitHub does not
evaluate path filters for tag pushes. It `needs: [build, bundle]` (so a red test suite
or one failed OS leg blocks it — no partial releases), downloads every bundle artifact,
stages just the installers flat (`.exe`/`.AppImage`/`.deb`/`.rpm`/`.dmg` — the macOS
`.app` is a directory of thousands of files), writes `SHA256SUMS`, and creates a **draft** release
for a human to check and publish (macOS stays unverified end-to-end, ADR 0009). Its
`contents: write` is **job-level**, so the PR-running jobs keep the workflow's default
read token; `bundle`'s first step on a tag asserts the tag and all three manifests
(`tauri.conf.json`, `app/package.json`, `app/src-tauri/Cargo.toml`, all `0.1.0`) carry
one version — a tag/`tauri.conf.json` mismatch *breaks* the release, since the bundle
*filenames* come from that config, while the other two are cosmetic; both are reported
in one run and both fail. Runbook: `docs/releasing.md`.
`nightly.yml` — **three** jobs: `repro-tier1` (Verilator trace regeneration),
`stress-tier2` (Ibex, `continue-on-error`), and `scale-bench` (the scalability
collector, `continue-on-error`, uploads a `scale-bench-metrics` artifact).

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
  landed first: the `scale-bench` crate (#24) measures the eager path at 665/100K/1M.
  (An earlier note here claimed `scope_graph`/`expand`'s full-edge scan blew up ~300×
  by 100K — that predates the scope-graph optimization and is **stale**: measured
  2026-07-26, `scope_graph` runs 6.6 µs at 665 → 17.3 µs at 100K → 7.2 µs p50 at 1M,
  i.e. it does not scale with node count at all. What *does* drive it is **edge density
  per scope** — the real 7.2K-node design costs 203 µs p50 / 1.5 ms p95, ~28× the 100K
  synthetic at 1/140th the size — and `cone()` under fan-out, 191 ms at 59K loads.)
  The **rkyv load cache (#21)** then landed (ADR 0003 Phase A / Option A): `ingest`
  caches the parsed `Document` in `.schemview_data/` and mmaps it on repeat launches,
  ~3.8× faster load at 100K (570 → 150 ms, `cache_hit` vs `from_slice`). The **wave_index
  cache (#153)** followed, persisting the matcher's output so a warm launch also skips the
  ~5–10s matcher pass — the dominant per-launch cost once the parse is cached. The two
  open Phase-4 decisions — **#22** (redb/SQLite demand-loading) and **#155** (zero-copy
  rkyv read-back) — are both gated on measurement, and the harness now covers the memory
  axes they actually turn on: `docs/benchmarking.md` is the runbook, one command
  producing a paste-ready metrics file. **First full run (2026-07-26) — read it before
  scoping either issue:** 1M nodes load fully in ~1.1 GB, so #22's "too large to
  materialize" premise is **not met at 1M** and the measured bottleneck is `cone()` under
  fan-out (190.8 ms at 59K loads) — an algorithmic fix, per ADR 0003's third outcome. For
  #155 the deserialize gap is real but partial: of a 1,414 ms warm load at 1M, ~294 ms is
  bytecheck validation a zero-copy path still needs and much of the rest is **index build**,
  not `deserialize`. See `docs/benchmarking.md` §Findings. See also `docs/ROADMAP.md`.

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
