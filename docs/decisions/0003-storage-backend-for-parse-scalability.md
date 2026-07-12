# ADR 0003 — Storage backend for parse/load scalability

- **Status:** Phase A **implemented** (#21, Option A / Document-only cache); Phase B deferred
- **Date:** 2026-06-26
- **Deciders:** project maintainers
- **Relates to:** `core/crates/{model,ingest,matcher}`, ROADMAP Phase 0–2 (model/matcher), Phase 5 (load path)

## Context

Today `ingest` loads an elaborated design by reading **one monolithic `hierarchy.json`**,
running a full `serde_json::from_slice` into a flat `Vec<Node>`, an O(n) referential-integrity
pass, and then **eagerly rebuilding three in-memory indices** — `path_index`
(`HashMap<String, Vec<NodeId>>`), `src_index` (a per-file `Lapper` interval tree), and
`conn_index` (`HashMap<NodeId, Vec<u32>>`) — *from scratch on every launch*. There is **no
on-disk database or cache**. On top of that, the matcher (`matcher::run_match`, wave-signal →
node by canonical path) is O(signals × path_len) and **also re-runs every launch**.

At the committed fixture (`picorv32_soc`, 665 nodes / 413 KB) this is invisible (~15–25 ms).
But the concern is real once designs grow past a few hundred modules:

| Nodes | JSON | Parse + eager index build | Matcher | Per-launch total |
| --- | --- | --- | --- | --- |
| 665 (fixture) | 413 KB | ~15 ms | ~50–100 ms | ~0.1–0.15 s |
| ~6K (target SoC) | ~3.7 MB | ~80–150 ms | ~0.1–0.4 s | ~0.2–0.5 s |
| 100K | ~62 MB | **~3–5 s** | **~5–10 s** | **~8–15 s** |
| 1M | ~620 MB | tens of s, multi-GB RSS | minutes | infeasible on a desktop |

Realistic basis used for sizing: `claude_verilog_test` — a 56-module, ~80–100-elaborated-instance,
~5–8K-model-node mid-size SoC that grows only modestly through Phase 6. So the *current* pain is
forward-looking, but the architectural smell is concrete: **the tool redoes parse + index-build +
match from scratch on every launch and persists nothing.**

This ADR records a structured evaluation: five advocate analyses, each defending one storage
backend against the real code, judged and ranked by **risk / effort / query power**.

## Decision

**Phase A (recommended now): add an rkyv zero-copy archive as a derived on-disk cache.**
Keep `hierarchy.json` as the golden, schema-validated source of truth. Beside it, maintain an
rkyv archive of the `Document` (plus the prebuilt indices and, ideally, the resolved
`wave_index`). On load, `mmap` the archive and read `Archived<Node>` views with **no
deserialization pass**; fall back to parsing the JSON and rewriting the archive on a version
mismatch or cache miss.

**Phase B (deferred): escalate to a real embedded database only when justified** — `redb`
(pure-Rust KV, minimal disruption) or `SQLite` (relational query power) — if true demand-loading
at 100K+ nodes or incremental re-elaboration becomes a hard requirement. Kùzu and columnar
Arrow/Parquet are documented here but not pursued.

## Options evaluated

1. **rkyv zero-copy archive + mmap cache** — serialize `Document` (+ indices + `wave_index`) to
   an rkyv archive; mmap and read archived views with zero copy. JSON stays golden; archive is a
   derived cache.
2. **Pure-Rust embedded KV (`redb`)** — the three HashMaps become three on-disk B-tree tables;
   demand-driven point/range lookups; keep every existing Rust algorithm verbatim.
3. **Embedded SQLite (`rusqlite`)** — relational tables + B-tree/R-Tree indices; demand SQL;
   recursive CTEs for `cone()`; persistent matcher result.
4. **Embedded graph DB (Kùzu)** — property graph; traversals as Cypher
   `MATCH (n)-[:DRIVES*1..d]->(m)`. In-repo precedent: `claude_verilog_test` already ships a Kùzu
   graph of its HDL via hdl-kgraph.
5. **Columnar Arrow/Parquet** — node table as columns; mmap + vectorized hash-join for the
   matcher; clean pyarrow → Rust seam.

## Ranking (risk / effort / query power)

Risk and effort: **lower is better**. Query power: **higher is better**.

| Rank | Option | Risk | Effort | Query power | Verdict |
| --- | --- | --- | --- | --- | --- |
| **1** | **rkyv mmap cache** | **Very low** — derived cache; pure Rust; `memmap2` already in the lockfile (via wellen); brittleness neutralized by the existing `schema_version` gate | **Very low** — `#[derive]` + a cache gate; keeps all algorithms; no Python/TS/toolchain change | Low — zero-copy reads only (same query model as today) | **Best ROI.** Collapses the per-launch parse + index-build; archiving `wave_index` also skips the matcher on repeat launches. |
| **2** | **redb KV** | Low — pure Rust, clean for the Win/Linux Tauri matrix; younger DB but used only as a cache | Low–med — rewrite the four accessor bodies; harness/transcode writes the db | Med — point + range lookups, demand-paging, incremental updates | Best escalation if lazy partial loads or incremental re-elaboration are needed. |
| **3** | **SQLite** | Med — bundled C build (Windows CI yak-shaving); engine itself rock-solid | Med — SQL schema + accessor rewrite; recursive CTE for `cone()` | High — SQL, R-Tree source index, joins, ad-hoc queries | The "real database" pick if relational query power and maturity outweigh minimal deps. |
| **4** | **Kùzu graph DB** | High — C++ dependency, youngest Rust bindings, binary weight | Med–high — Cypher rewrite + graph bulk-load harness + FFI | Very high (graph) — native variable-length traversals, impact-of-change | Best conceptual fit + precedent, but premature for a desktop app at current scale. |
| **5** | **Arrow/Parquet columnar** | Med–high — heavy deps (arrow + parquet + datafusion); compile/binary cost | High — columnar schema, vectorized join, random-access index, awkward traversals | High (batch) / low (point + traversal) | Uniquely attacks the matcher *join*, but over-engineered now — and caching the result moots the recurrence regardless of engine. |

## Forces / rationale

- **Persistence is the win; the engine is secondary.** Every fixable cost here — re-parse,
  re-index, re-match — is *redone work* that any durable cache eliminates. So the right first
  move is the cheapest persistent cache that keeps the existing algorithms intact.
- **The model is already shaped for rkyv.** Every cross-reference (`parent`, `children`,
  `drivers`, `loads`, `Edge::port/endpoint`, `Range::file`) is a `u32` index into a flat `Vec`,
  not a pointer — which is exactly rkyv's on-disk layout. The port is derives + a cache gate, not
  a rearchitecture.
- **The fragile boundary stays untouched.** The serde DTOs in `gui`/`schematic` are the wire
  format to `app/src/types.ts`. rkyv (and redb/SQLite) sit *behind* those DTOs; the frontend
  cannot tell whether a `SchNode` came from a parsed `Vec` or an mmap'd archive. **Zero
  `types.ts` changes.**
- **Schema brittleness is a non-issue for a cache.** rkyv archives are brittle across struct
  changes, but the archive is a *derived cache guarded by the existing `schema_version` field
  (`ingest::validate` already gates on it)*. On any mismatch the code reparses the canonical JSON
  and rewrites the cache — "schema evolution" reduces to "discard a cache file."
- **The matcher is computation, not I/O.** Its O(signals × path_len) cost is normalization work;
  mmap can't speed it directly. But *persisting the resolved `wave_index`* makes it a one-time
  cost — and isolates it as the one remaining bottleneck to attack later.
- **Why defer the heavier DBs.** SQLite's C build adds Windows-CI friction; Kùzu adds a C++
  dependency and immature Rust bindings; Arrow/Parquet add heavy deps and are weak at the
  pointer-chasing traversals (`cone`/`expand`) that define the product. Their wins (relational
  queries, native graph traversal, vectorized joins) are real but not yet justified at this scale.

## 1M+ node analysis — Phase B is empirical

Does Phase B (redb/SQLite) still hold if the elaborated model reaches **1M+ nodes**? The honest
split is: *the storage layer holds; the system behavior is the open question.*

**Provable analytically (true for any embedded DB):**

- **O(1) open** — independent of node count; no full materialization.
- **Scoped queries stay bounded** — `scope_graph`/`expand`/`probe_node`/`probe_source` touch only
  the rows for one scope/path/offset, so 1M total nodes don't change the cost of inspecting one
  module. This is the property Phase A's full-materialization *lacks*, and it is why Phase B's
  demand-loading *architecture* is the correct shape for 1M+.
- **Indexed matcher join is O(signals·log n), not O(signals·n)** — SQLite/redb handle 1M-row
  indexed joins routinely; the join itself does not fall over.

**NOT provable on paper (design-distribution-dependent, not node-count-dependent):**

- **`cone()`/impact cost is driven by net fan-out, not node count** — a global clock/reset net
  touching 100K loads explodes the traversal frontier regardless of storage engine. Invisible to
  complexity analysis.
- **Matcher wall-time and memory scale with trace signal count** — the matcher iterates the entire
  wave hierarchy; a 1M-signal VCD/FST is multi-second-to-minute even indexed.
- **SQLite query-planner / recursive-CTE behavior vs redb's hand-rolled disk-backed BFS** depends
  on real adjacency distributions and the query mix; which wins is not decidable a priori.
- **DB build/ingest time and working-set RSS under real navigation** are empirical.

**Verdict.** Phase B holds *directionally* at 1M+ (it is the only approach that decouples
per-interaction cost from total design size), **but the engine choice — redb vs SQLite — and the
confirmation that it actually holds must be settled by a benchmark on a real ≥1M-node design, not
by another paper debate.** The re-evaluation must be free to reach a *third* outcome: at 1M+ the
fix is **algorithmic** (viewport-scoped/streaming loading, level-of-detail, fan-out caps) layered
on top of the DB, not a storage swap alone.

**Re-evaluation basis is undecided.** `claude_verilog_test` maxes out at ~5–8K model nodes, so it
**cannot** be the 1M-node basis. Two candidates, to be chosen at re-eval time: (a) a **synthetic
scale-up generator** — parameterized fan-out, hierarchy depth, and signal count, deterministic in
CI; and (b) a **large external open SoC** (e.g. an OpenTitan/multicore-scale design) elaborated
through the pyslang harness. The benchmark output is the input to the Phase-B engine decision.

## Consequences

- **Phase A** touches only `core/crates/model` (rkyv derives on `Document`/`Node`/`Edge`/index
  structs) and `core/crates/ingest` (cache gate + JSON→rkyv transcode + `rkyv_format_version`
  const). The Python harness, the TS wire format, and the 1.94 toolchain are all unchanged. The
  archive is a build/cache artifact, not committed.
  - **As built (#21):** rkyv 0.8 derives sit alongside serde on the `model` wire types;
    `ingest::from_path` gates on a `<model dir>/.schemview_data/<file>.rkyv` archive with a
    self-describing header (`RKYV_FORMAT_VERSION` + `schema_version` + source `len`/`mtime_ns`
    staleness key). A hit **mmaps + validates (bytecheck) + deserializes** into an owned
    `Document` (**Option A** — not zero-copy `&Archived`; that stays a future item for when a
    design outgrows RAM, at which point it converges with Phase B). The `path_index`/
    `src_index`/`conn_index` are still rebuilt on load. Any miss/stale/corrupt archive falls
    back to the JSON and rewrites the cache (best-effort). `svxprobe cache <model.json>`
    pre-builds the archive for cache-less/ephemeral deploys. Measured at 100K synthetic nodes:
    cold `from_slice` ~603 ms → warm `cache_hit` ~210 ms (~2.9×).
  - **wave_index cache follow-up (#153, as built):** the matcher's resolved `wave_index`
    (trace signal ↔ NodeId) is now persisted too, closing the "persisting them is a follow-up"
    gap above. `ingest::{try_load_wave_index, write_wave_index}` cache the flat `(NodeId,
    var_ref)` pairs in a sibling `.schemview_data/<model>.<trace>.waveidx.rkyv` archive, keyed
    on **both** files' `len`/`mtime_ns` + a `MatchOptions` hash + `WAVE_INDEX_FORMAT_VERSION`
    (a changed model can renumber node ids, so the key must bust on either file). `CrossProbe::
    build_cached` consults it and skips `run_match` on a hit — turning the matcher's ~O(signals
    × path_len) pass (the dominant per-launch cost once the parse is cached) into a one-time
    cost. The `svxprobe match` gate still runs the live matcher (it needs the `MatchReport`).
- **A load-time benchmark** at 665 / ~6K / synthetic 100K nodes should be added as a CI perf
  guard so regressions are caught.
- **Phase B remains open and is gated on a benchmark.** If a future workload needs to browse a
  100K+/1M+-node design without ever fully materializing it, the redb-vs-SQLite choice is decided
  by the scalability benchmark (see "1M+ node analysis" above), not by this ADR alone. This ADR is
  the baseline; the benchmark is the trigger.
- This decision is **separate from the FSDB/plugin boundary (ADR 0002)** and the scope decision
  (ADR 0001); it concerns only how the elaborated model is stored and loaded.

## Loop-back

If Phase A lands and load time at the target scale is still dominated by the matcher (not parse +
index), the fix is algorithmic — memoize/normalize in `matcher`, or persist `wave_index` per
trace — not a different storage engine. Only a demonstrated need for *partial* loading of a
design too large to hold resident should trigger Phase B — and at 1M+ nodes that trigger runs
through the scalability benchmark, which may itself conclude that demand-scoped/streaming loading
(an algorithmic change) is required alongside, or instead of, the DB swap.
