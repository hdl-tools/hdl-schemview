# hdl-schemview — Execution Roadmap

**Status:** Phases 0–3e shipped (all three views linked in a desktop app); Phase 4
(scalability hardening) active; Phases 5–6 open.
**Audience:** An executing agent or engineer with no access to the design
conversation. This document is self-contained.
**How to read it:** Sections 1–4 are context you must internalize before touching
code. Section 5 is the phased plan (gated, testable). Section 6 is what *not* to
do. Sections 7–9 are the risk register, stack, and handoff notes.

---

## 1. North star (the problem)

Build an interactive tool that links three views of a digital design and keeps
them in sync:

- **Source** — the SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — a generated, navigable diagram of the elaborated design.
- **Waveform** — simulation traces (VCD/FST/GHW, optionally FSDB via a user reader).

The value is **accurate cross-probing**: click a signal in any view, the other
two jump to the corresponding object. Commercial equivalents are Synopsys Verdi
and Cadence Indago. We are **not** matching their breadth; we are building a
focused, open, RTL-level tool by composing existing components.

### How the four goals map to the phases

| Goal | Delivered by |
|---|---|
| Full SystemVerilog elaboration | Phase 0 (harness) + Phase 1 (Node model spine) |
| Accurate source/schematic/waveform cross-probing | Phase 1 (matcher, **go/no-go**) → Phase 2 (source↔wave) → Phase 3 (schematic) → Phase 3e (usage-level source resolution, C↔RTL) |
| Scalable visualization | Phase 3 (schematic) + Phase 3e (selectable projections) + Phase 4 (scalability hardening) |
| VCD & FST + user-brought plugins | Phase 1 (wellen ingest) + Phase 5 (reader & translator plugin surfaces) |

Phase 6 makes the whole thing installable and embeddable.

---

## 2. The one principle that governs the whole design

**The elaborated hierarchy is the single source of truth. Source, schematic, and
waveform are three *projections* of it.**

- Source and waveform get bidirectional maps **to** the elaborated model.
- The schematic **is** the elaborated model rendered — no separate identity space.

Get this right and cross-probing is lookups, not heuristics. Every phase below
serves this principle.

---

## 3. Reuse manifest (build vs. adopt)

Adopting these is mandatory, not optional. The single biggest failure mode is an
agent trying to build something already solved. When in doubt, **adopt**.

| Concern | Adopt | Current status (verified Jul 2026) | Do NOT build |
|---|---|---|---|
| SV parse + elaboration | **slang** (`MikePopoloski/slang`) — complete IEEE 1800-2023 through elaboration; library-first; ships **Python bindings (`pyslang`) in-tree** | The standalone `pyslang` repo was **archived Jan 2025**; bindings consolidated into `slang` and still published to PyPI as `pyslang`. Target the consolidated source. | A SystemVerilog frontend. This is decades of work. |
| Waveform read (VCD/FST/GHW) | **wellen** (`ekiwi/wellen`) — pure-Rust, lazy per-signal loading | Confirmed VCD/FST/GHW. FST is truly lazy; VCD/GHW are parsed once into a compressed in-memory store (FST-like). `pywellen` wrapper exists; used beyond Surfer (e.g. VaporView). | A VCD/FST parser |
| Schematic layout | **ELK** (Eclipse Layout Kernel) layered/Sugiyama. Reference: `netlistsvg`, `d3-hwschematic` | Two routes: `elkjs` (mature JS/WASM) **or** `elk-rs` (`openedges/elk-rs`, pure-Rust drop-in, NAPI + WASM). Pick during Phase 3 — see §8. | A graph layout engine |
| Waveform rendering (optional) | **Surfer** — embed via `<iframe>` + WCP, or reuse its rendering | Active; CAV 2025 paper. Reuses wellen internally. | A waveform renderer from scratch (only if embedding proves too constraining) |
| Waveform control channel | **WCP** (Waveform Control Protocol) / **CDSP** (CXXRTL Debug Server Protocol) | WCP is JSON, LSP-inspired, over stdio/sockets/web; Surfer is first implementor, GTKWave has a WCP branch. | A bespoke control protocol |
| FSDB read (later) | **nFFR** (Synopsys, *user-supplied* — see §4.2) | Proprietary, non-redistributable. Enters only as a user-built IPC reader. | An FSDB reader. Format is proprietary and undocumented. |
| Source editor | Monaco / CodeMirror; or an LSP client (a slang-based SV language server exists) | — | A code editor |

---

## 4. Two strategic forks

Both forks are **re-examined** as Architecture Decision Records and **locked to
their defaults** for v1. Read the ADRs before deviating.

> **All ADRs** live in [`decisions/`](decisions). 0001/0002 are the two forks below;
> the rest record decisions made during the build: 0003 storage backend for parse
> scalability, 0004 internal-logic granularity, 0005 optional gate-level projection,
> 0006 HLS C↔RTL source tracing, 0007 model-driven semantic name coloring, 0008
> lexical source highlighting. The §8 stack sub-decisions (elkjs, custom waveform
> canvas) are closed inline there rather than as separate ADRs.

### 4.1 Scope: RTL-level vs. netlist-level → **RTL-level (locked)**
Source = the hierarchy, so cross-probe is direct and a weekend-feasible spike on
slang validates it. Netlist-level (post-synthesis) requires traversing synthesis
name-mapping and is deferred as a post-v1 extension.
**Full rationale:** [`decisions/0001-scope-rtl-vs-netlist.md`](decisions/0001-scope-rtl-vs-netlist.md).

### 4.2 Distribution: open-portable vs. FSDB-native → **open-portable core (locked)**
Target VCD/FST via wellen so the tool is standalone and redistributable. FSDB
support enters only as a **user-supplied, IPC-isolated reader plugin**, because
reading FSDB requires linking Synopsys's nFFR libraries, which cannot be
redistributed. You cannot have "open + standalone" *and* "reads vendor traces
natively" in one binary — the plugin boundary is how you get both.
**Full rationale:** [`decisions/0002-distribution-open-vs-fsdb.md`](decisions/0002-distribution-open-vs-fsdb.md).

---

## 5. Phased plan

Phases are gated. Each lists **Goal / Tasks / Deliverables / Exit gate (testable)
/ Depends on / Loop-back / Primary risk / Size (S/M/L)**. If an exit gate fails,
return to the indicated loop-back point rather than proceeding.

### Phase 0 — Setup & integration substrate · Size: S

- **Goal:** Decide the host language and elaboration interface; stand up the repo;
  freeze the reference fixture that every later gate tests against.
- **Tasks:**
  1. **Polyglot substrate.** Rust core (owns data model, matcher, event bus,
     wellen, layout) + **slang accessed out-of-process**. Start with a `pyslang`
     script that walks the elaborated design and serializes the Node model (§Phase
     1) to JSON/MessagePack; the Rust core ingests it. Escalate to C++ FFI only if
     serialization throughput becomes a bottleneck.
     - Across the process boundary, the live slang symbol handle becomes a
       **stable symbol key + captured provenance** (source ranges, canonical
       path), **not** a pointer.
  2. **Repo skeleton:**
     ```
     core/                 Rust workspace: data model, indices, matcher, event bus
     elaborate/            pyslang harness: elaborate → serialize Node model
     app/                  Tauri 2 desktop app (added in Phase 3c)
     fixtures/             frozen reference fixture (see below)
     docs/                 this roadmap + ADRs
     .github/workflows/    CI
     ```
  3. **CI:** matrix building the Rust core, running the pyslang harness against a
     pinned slang/pyslang version, and a Verilator step that regenerates traces to
     confirm the fixture is reproducible.
  4. **Build the reference fixture (do this first; reuse through Phase 4).**
     - **Design:** a small but non-trivial RISC-V core exercising the hard cases —
       **generate blocks, parameterized instances, packages, and interfaces** (a
       flat trivial core won't stress the matcher). PicoRV32 is a reasonable
       default; if it lacks generate/param coverage, prefer a small parameterized
       in-order core or add a parameterized wrapper.
     - **Trace:** simulate with **Verilator**, dump **FST** (native fast path)
       **plus a VCD** of the same run, so the matcher hits both formats from day
       one.
     - **Pin the gate threshold:** Phase 1 target = **≥ 95% of design-scope
       signals matched, every miss attributable to a named normalization-rule gap
       (zero mystery misses).** Testbench-only and simulator-internal scopes are
       excluded from the denominator; commit that exclusion list with the fixture.
     - **Freeze it:** check design sources, generated FST + VCD, the golden
       expected hierarchy, and the excluded-scope list into `fixtures/`.
- **Deliverables:** repo skeleton, CI, the pyslang→serialized-model harness stub,
  and the frozen reference fixture (sources + FST + VCD + golden hierarchy +
  excluded-scope list + the pinned 95% / zero-mystery threshold recorded in the
  README).
- **Exit gate:** `pyslang` elaborates the fixture and emits *something* the Rust
  core can deserialize; the fixture (both FST and VCD) is committed and loads via
  wellen.
- **Depends on:** §4 decisions recorded (done — ADRs 0001/0002).
- **Loop-back:** none (foundational).
- **Risk:** polyglot friction (mitigated by the serialized boundary, no FFI in
  v1); **and choosing too simple a fixture** — without generate/param/interface
  constructs the Phase 1 gate passes trivially and gives false confidence. The
  fixture must contain the constructs the matcher exists to handle.

### Phase 1 — Foundation + feasibility spike (GO/NO-GO) · Size: M

> **Status: GATE PASSED.** The matcher (`core/crates/matcher`) clears the gate on
> the frozen tier-1 fixture — **100% of design-scope signals matched, 0 unmatched,
> 0 mystery, on both FST and VCD** — with the DUT anchor auto-detected. Enforced in
> CI via `svxprobe match`. GO.

- **Goal:** Build the elaboration spine and data model, ingest a real waveform,
  and **prove the cross-probe matcher works** before investing in UI.
- **Tasks:**
  1. **Node model** (the spine):
     ```
     Node { id, kind(Instance|Net|Port|Var|ModuleDef|GenBlock),
            path(canonical), parent, children[],
            symbol_key, def_range, inst_range?, type, drivers[], loads[] }
     src_index:  per-file IntervalTree<SourceRange -> [NodeId]>   // reverse map is one-to-many
     path_index: HashMap<CanonicalPath -> NodeId>
     wave_index: BiMap<NodeId <-> WaveSignalRef>                  // populated at trace load
     ```
     The spine absorbs generate/param expansion via `symbol_key`.
  2. **Canonical-path matcher:** normalize every waveform scope path (testbench
     wrapper prefixes, `genblk` aliasing, escaped identifiers, array notation)
     through **one grammar**, then match against `path_index`:
     exact → ordered rule set → **unmatched signals surface in a visible list
     (never silent).**
  3. **Hit-rate report:** matched/unmatched lists with each miss tagged by the
     rule gap that explains it.
- **Deliverables:** elaborate the fixture → populated Node model + three indices;
  load its FST via wellen; run the matcher; emit the hit-rate report against both
  FST and VCD.
- **Exit gate (THE decision number):** matcher hit-rate on the frozen fixture
  meets the Phase 0 threshold — **≥ 95% of design-scope signals (excluded scopes
  per the fixture list), every miss attributable to a named rule gap, zero mystery
  misses** — against **both** the FST and the VCD.
- **Depends on:** Phase 0.
- **Loop-back:** if the gate fails and residual misses can't be explained or
  ruled, **return to §4.1 (scope)** and reconsider before building any UI. This is
  where the project is validated or killed cheaply.
- **Risk (HIGHEST in the whole project):** cross-probe accuracy across
  generate/param expansion and simulator naming. This phase exists to prove the
  claim on real data.

### Phase 2 — Cross-probe core: source ↔ waveform · Size: M

> **Status: DONE (headless).** `core/crates/xprobe` links source ↔ waveform
> through one `Selection` channel: bidirectional resolution, generate ambiguity
> disambiguated by context with a picker, and loud `NotInTrace`. Driven by
> `svxprobe probe`; tested on both FST and VCD. A GUI is deferred to later phases.

- **Goal:** Two of three views, fully linked. No schematic yet.
- **Tasks:**
  1. **Event bus:** one `Selection { nodes[], anchor }` event routed between
     views; hold current hierarchy `context` as the disambiguator.
  2. **Wire both directions:** source click → interval-tree lookup → Selection;
     waveform click → `WaveSignalRef` → NodeId → Selection. On receipt, source
     scrolls to `def_range`/`inst_range`; waveform resolves via `wave_index` (or
     shows "not in trace").
  3. **Ambiguity:** source click inside a generate loop returns N candidates →
     resolve against active `context`, offer the rest in a picker (this is what
     Verdi does).
- **Exit gate:** bidirectional cross-probe works on the fixture core; one-to-many
  resolved via picker; trace misses are loud.
- **Depends on:** Phase 1 (passed gate).
- **Loop-back:** matcher gaps exposed here → Phase 1 normalizer.
- **Risk:** UI state/event consistency. Keep the bus as the single channel.

### Phase 3 — Schematic view (third projection) · Size: L

> **Status: DONE (3 stacked PRs).** 3a — connectivity (port-connection `edges`
> in the model) ✅. 3b — the headless `schematic` crate (scope graph, on-demand
> `expand`, fan-in/out `cone`; `svxprobe graph` CLI) ✅. 3c — the **Tauri desktop
> app** (`app/`): three linked panes (schematic via elkjs, source, waveform
> canvas) over `svxprobe-gui`; click a box → source + waveform follow; expand;
> picker; cone ✅. Standalone (native webview; no Chromium/Playwright at runtime).

- **Goal:** Generate a navigable schematic from the elaborated model and link it
  into the bus.
- **Tasks:**
  1. **Subgraph extractor:** one scope, or a fan-in/fan-out cone of a selected net.
  2. **Layout:** feed the subgraph to **ELK** (`elkjs` or `elk-rs` — decide here,
     see §8) → render.
  3. **Incremental expansion:** fetch a node's children and re-layout
     **incrementally**, never globally.
  4. **Identity is free:** a box is an Instance NodeId, a wire a Net NodeId, an
     endpoint a Port NodeId. No schematic-specific ID space — the payoff of drawing
     from elaborated RTL rather than a synthesized netlist.
- **Exit gate:** render a scope; expand instances on demand; click any box/wire and
  have source + waveform follow; cone extraction (fan-in/out of a selected net)
  works.
- **Depends on:** Phase 2.
- **Loop-back:** if incremental re-layout proves unreadable/unstable, revisit the
  ELK route choice in §8 before scaling.
- **Risk:** layout readability and incremental re-layout. ELK handles the
  algorithm; the work is the on-demand / level-of-detail policy. This is the
  largest single build — budget accordingly.

#### Phase 3d — Internal-logic schematic (drill into leaf modules) · Size: L

> **Status: DONE.** Epic #35. Drilling into a **leaf** RTL module
> (no child instances — e.g. `picorv32`) now shows its internal logic as a
> schematic at **process/statement granularity**: each `always` / `always_ff` /
> `always_comb` / continuous `assign` is **one** logic box (`Ff`/`Comb`), wired
> to the signals it reads and writes. Granularity is fixed by **ADR 0004**
> (process-level, *not* gate/operator-level).

- **Part-issues:** #30 quick nav fix (leaf instances drillable) ✅ · #31 harness
  emits process-level logic nodes (`Comb` + broaden `FF`) + golden + `NodeKind::Comb` ✅ ·
  #32 per-`(box,signal)` pin allocator ✅ · #33 schematic extractor (leaf
  logic-graph + signal-join wiring) ✅ · #34 frontend renders combinational boxes ✅.
- **Governing principle:** logic boxes carry their model `NodeId` + `def_range`,
  so cross-probe stays a lookup — no heuristics, no string-matching.
- **Out of scope:** gate/operator-level netlist decomposition; `initial`/`final`
  blocks; tasks/functions.
  - **Superseded in part by ADR 0005 (Phase 3e).** Gate/operator decomposition is no
    longer out of scope outright — it exists as an **opt-in projection** behind the
    harness's `--gate-level` flag and a Settings toggle. Process-level remains the
    default and the granularity ADR 0004 fixes; the gate view is a second projection
    of the same model, never a replacement. `initial`/`final` and tasks/functions
    stay out of scope.

#### Phase 3e — Projections & source fidelity · Size: L

> **Status: DONE.** The wave after 3d, delivered as independent slices rather than
> one epic. Everything here is a *projection* of the same elaborated model, per §2 —
> no new identity space, and every addition to the harness is **opt-in and additive**
> (`schema_version` stays `1`; with the flag off the emitted JSON is byte-identical).

- **Gate-level schematic projection** (#157 → #199, #206, #207; **ADR 0005**) — a
  `Projection { ProcessLevel | GateLevel }` selects internal-logic granularity.
  Under `GateLevel` a combinational block dissolves into flat gate/mux primitives
  drawn as IEEE distinctive glyphs, with inverter folding, inline constant/parameter
  tie values, `Concat` primitives, memory-array read operands, and `case` statements
  lowered to priority-mux trees. Harness side is `--gate-level`; frontend side is a
  Settings toggle. **Open follow-up: #215** (`if`/`else` statement lowering — the
  `case` sibling; not implemented).
- **Memory glyph** (#112; **ADR 0004 amendment**) — an unpacked-array `Var` re-kinds
  to `Memory` and renders as a MEMORY glyph with `Addr`/`Din`/`Dout` pins from typed
  `mem_port` edges, plus depth and an INIT marker from `$readmemh`/`$readmemb`.
- **HLS C/C++ ↔ RTL source tracing** (#159, #222; **ADR 0006**) — a bidirectional
  line-region `source_map` scanned from the generated RTL's own provenance comments
  gives HLS designs a C/C++ source pane that cross-probes to schematic and waveform.
  C is **display-only and never parsed**: the correspondence is always the tool's own
  provenance, never inferred.
- **Source-pane fidelity** — lexical syntax highlighting for SystemVerilog and C/C++
  (#223, #224; **ADR 0008**) layered under **model-driven semantic name coloring**
  (#225; **ADR 0007**), where identifier spans come from the elaboration
  (`--name-refs`) rather than the lexer. The same spans make a click on a *usage*
  resolve to the signal it names, not merely to the enclosing process.
- **Independent panes** (#167 epic: #168, #169, #170, #171) — multi-session backend,
  detachable schematic and waveform windows each with their own trace, and a per-pane
  signal picker; extended to the schematic pane by the `a`-key palette (#219).
- **Waveform pane** — collapsible lane groups with drag reordering (#182, #188, #192),
  sticky ruler and top-packed lanes (#180, #181), per-signal multi-view lanes and
  sub-buses (#179).
- **Polish** — dangling logic-box and gate outputs labelled with their driven net
  (#118, #202, #216); navigable-scope predicate shared by tree and schematic (#184);
  status/log consolidated into one pane (#100, #228); `.gitattributes eol=lf` pinning
  source offsets across platforms (#203).
- **Governing principle held throughout:** every one of these is a lookup against the
  model. The gate view carries model `NodeId`s and sub-expression `def_range`s; the
  C↔RTL map is emitted provenance; name coloring is classified off the symbol the
  elaboration resolved, never off the token text.

### Phase 4 — Scalability hardening · Size: M

> **Status: ACTIVE — measurement-first.** The rule for this phase is that no storage
> or representation change lands before a measurement justifies it, so the benchmark
> was built first.
>
> - **`scale-bench` (#24)** ✅ — a dev-only crate: a deterministic synthetic-model
>   generator (665 / 100K / 1M nodes), criterion benches for load/query/matcher, and a
>   `scenario` bin that runs **one measured operation per process** so peak RSS is
>   attributable. Bases include the committed golden and any **real** elaborated design.
>   The `collect` bin drives the whole matrix into one paste-ready metrics file; the
>   nightly `scale-bench` job builds and runs it, and uploads the result. Runbook:
>   [`benchmarking.md`](benchmarking.md).
> - **rkyv load cache (#21)** ✅ — ADR 0003 Phase A. `ingest` caches the parsed
>   `Document` in `.schemview_data/` and mmaps it on repeat launches; measured **3.8×**
>   faster warm load at 100K (570 ms → 150 ms).
> - **`wave_index` cache (#153)** ✅ — persists the matcher's resolved `(NodeId,
>   var_ref)` pairs, so a warm launch also skips the matcher pass that dominated
>   per-launch cost once the parse was cached.
>
> **Measured against the exit gate** (2026-07-26, 16 GB desktop, full matrix in
> `benchmarking.md` §Findings):
>
> - *Bounded memory* — 1M nodes **fit**: 4.1 s cold load at 1,129 MB peak RSS, 1.4 s
>   warm. Nothing OOMs. This matters because it means #22's premise (a design *too
>   large to materialize*) is **not met at 1M**, which shifts weight toward ADR 0003's
>   third outcome (level-of-detail / fan-out policy) over a storage swap.
> - *Sub-second scope expansion* — holds everywhere: `scope_graph` is **flat in node
>   count** (6.6 µs at 665 → 17.3 µs at 100K → 7.2 µs p50 at 1M). What drives it is
>   **edge density per scope**, not size: a real 7.2K-node design costs 203 µs p50 /
>   1.5 ms p95, ~28× the 100K synthetic at 1/140th the node count.
> - *Sub-second signal query* — the one interactive miss is **`cone()` under fan-out**:
>   190.8 ms at a 59K-load clock. That is the concrete target for the level-of-detail
>   work, and per ADR 0003 it is an algorithmic fix, not a storage one.
>
> **Open and gated on this data:** **#22** (Phase B — redb/SQLite demand-loading) and
> **#155** (true zero-copy rkyv read-back). For #155 the split is already measured:
> `access_unchecked` 0.29 ms vs `cache_hit` 1,414 ms at 1M, but 294 ms of that gap is
> bytecheck *validation* a zero-copy path still needs, and most of the rest is
> **index build**, not `deserialize` — so the payoff is smaller than the headline gap
> suggests.

- **Goal:** Survive a real SoC, not just a core.
- **Tasks:** lazy everywhere (hierarchy, connectivity, signal data via wellen's
  per-signal loading); level-of-detail in the schematic; bounded-memory trace
  handling. Audit every "load all" path.
- **Exit gate:** open a multi-million-net design + large trace with bounded memory
  and interactive (sub-second) scope expansion and signal query.
- **Depends on:** Phase 3.
- **Loop-back:** memory blow-ups trace back to eager materialization in Phases 1–3.
- **Risk:** memory blow-up from eager materialization.

### Phase 5 — Plugin surfaces · Size: M

Two **separate** surfaces — do not conflate them (§6, ADR 0002).

- **5a. Format/reader plugins (the FSDB path).** Define a `WaveSource` trait keyed
  to the lazy model:
  ```
  trait WaveSource {
    fn probe(path) -> bool;             // extension / magic-byte sniff
    fn hierarchy() -> Hierarchy;        // scopes + vars
    fn load_signals(ids);               // pull only displayed signals
    fn query(id, time_range) -> [ValueChange];
  }
  ```
  - Built-in impl = wellen (VCD/FST/GHW).
  - User-brought readers load via **subprocess + IPC** (LSP/WCP-style JSON over
    stdio/socket). Mandatory for FSDB: the nFFR-linked code lives in a *separate
    executable the user builds against their own Verdi*, so the tool ships zero
    proprietary bits and gets crash isolation for free. WASM is unsuitable (can't
    `dlopen` a native proprietary lib).
  - The only contract the spine needs from any reader: surface hierarchical scope
    paths so the matcher maps them to NodeIds.
- **5b. Value-translator plugins.** Bit-vector → semantic value (struct / enum /
  instruction decode). Sandboxed **WASM or Python**, trait-based, like Surfer's
  translator system. Independent of 5a.
- **Deliverables:** the reader IPC protocol spec (message set + handshake /
  discovery for locating a user's reader binary); a working FSDB-via-nFFR reference
  reader (gated on a Verdi install being present); one sample translator.
- **Exit gate:** a user-supplied FSDB reader binary loads via IPC and cross-probes
  *identically* to a native FST; a sample translator decodes a struct in the
  waveform.
- **Depends on:** Phase 2 (readers), Phase 1 (translators can come earlier).
- **Loop-back:** scope-path mismatches from a user reader → Phase 1 normalizer.
- **Risk:** IPC protocol churn and licensing. Version the protocol; ship none of
  Synopsys's bits.

### Phase 6 — Packaging, integration, docs · Size: M

- **Goal:** Make it installable and embeddable.
- **Deliverables:** distribution (native binary; optional WASM build); config
  system; editor integration (VSCode extension and/or LSP wiring; WCP if embedding
  Surfer); a **bring-your-own-Verdi** guide for FSDB; user docs.
- **Exit gate:** a fresh user installs, opens a design + trace, and cross-probes
  all three views without reading source.
- **Depends on:** Phase 4 (and 5 for FSDB docs).

---

## 6. Non-goals (explicit — do not scope-creep)

- **Do not** build a SystemVerilog frontend. Use slang.
- **Do not** target Verdi/Indago feature parity, multi-vendor simulator
  robustness, or post-synthesis optimization tracing in v1.
- **Do not** render flat schematics of whole designs. Hierarchical, on-demand only.
- **Do not** ship any Synopsys/FSDB binaries or headers. The user supplies their
  own.
- **Do not** support the full SV class/UVM/constraint machinery for visualization
  — the synthesizable design subset is enough for the three views. (slang parses
  the rest regardless; you just don't project it.)
- **Do not** build value translators and format readers as one plugin system. They
  have opposite constraints (sandboxable vs. native-linking).

---

## 7. Risk register (ranked)

1. **Cross-probe matcher accuracy** — validated/killed at the Phase 1 gate.
   Highest risk; front-loaded deliberately.
2. **Polyglot integration (slang C++/Python ↔ Rust core)** — mitigated by the
   serialized-model boundary; FFI only if needed.
3. **Schematic layout + incremental re-layout at scale** — Phase 3/4; algorithm is
   solved (ELK), policy is the work.
4. **FSDB licensing coupling** — mitigated structurally by the IPC reader boundary
   (Phase 5 / ADR 0002).
5. **Scope creep toward commercial parity** — mitigated by §6.

---

## 8. Tech stack (decided defaults)

- **Core:** Rust.
- **Elaboration:** slang via `pyslang` (now in-tree), out-of-process, serialized
  model (JSON/MessagePack). FFI only if throughput demands it.
- **Waveform read:** wellen (built-in); subprocess IPC for user readers; nFFR for
  FSDB (user-supplied).
- **Schematic layout:** ELK — **`elkjs`, decided in Phase 3c.** The layout path runs
  in the webview alongside the SVG renderer (`app/src/elk.ts`), so there is no
  JS-boundary crossing to pay for: the graph is already in the frontend when it is
  laid out. `elk-rs` would have moved layout into the Rust shell only to serialize the
  result straight back out. Revisit only if layout becomes the interactive bottleneck
  — at present `cone()` extraction, not layout, is the measured cliff (Phase 4).
- **Rendering/control:** **custom canvas, decided in Phase 3c.** Surfer is embedded via
  `<iframe>` + WCP or not at all, and an embedded pane cannot participate in the
  cross-probe as a *projection of our model* — WCP would have made it a second
  identity space to reconcile, which §2 exists to prevent. The waveform pane is a
  canvas 2D renderer (`app/src/wave.ts`) over the same `wave_index` lookups as every
  other view; wellen is still the reader, so nothing was rebuilt but the drawing.
  WCP remains available as a Phase 5 *outbound* surface.
- **Editor:** **own source pane with a hand-rolled lexical highlighter, decided in
  Phase 3e** — an argued reversal of §3's "Do NOT build: a code editor", scoped to a
  *viewer*, not an editor. See **ADR 0008**: the pane's whole job is byte-offset-exact
  cross-probe anchoring, and semantic identifier coloring comes from the model
  (**ADR 0007**), not from a language service. Monaco/CodeMirror/LSP stay the answer
  if editing is ever in scope; it is not.

---

## 9. Handoff notes for the executing agent

- Treat **Phase 1's exit gate as the project's go/no-go.** Do not build UI
  (Phases 2+) until the matcher hit-rate is proven on a real trace. The cheapest
  path to knowing whether this is worth building is the `pyslang + wellen` spike
  with no rendering.
- The §4 forks are locked to defaults (ADRs 0001/0002). Record any deviation —
  Phases 3 and 5 assume RTL-level + open-portable.
- Each phase's exit gate is an automatable test. The **reference fixture is built
  in Phase 0** (small RISC-V core with generate/param/interface coverage +
  Verilator FST + VCD + golden hierarchy + excluded-scope list) and is the
  regression target through Phase 4. The Phase 1 threshold (≥ 95%, zero mystery
  misses) is pinned alongside it. Do not invent ad-hoc test files in later phases
  — extend the fixture.
  - **Amended.** The rule's target is ad-hoc files invented *instead of* exercising
    the reference design; a **purpose-built fixture for a construct the reference
    design does not contain** is legitimate, because extending `picorv32_soc` with
    unrelated constructs would erode the very property that makes it a stable gate.
    `fixtures/hls_min/` is the precedent: HLS provenance comments cannot be added to
    a hand-written RTL core without making it not-hand-written. Such a fixture must
    be tiny, committed, deterministic, and documented in
    [`fixtures.md`](fixtures.md) with the tier table saying what gates it. The
    matcher and cross-probe gates still run on tier-1 only.
- When in doubt about a build-vs-adopt call, re-read §3. The default is always
  adopt.
