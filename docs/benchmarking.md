# Scalability benchmarking — the runbook for #22 and #155

> How to produce the numbers that decide **#22** (Phase B: redb/SQLite demand-loading)
> and **#155** (true zero-copy rkyv read-back), and what to hand back for evaluation.
> Runs fully **offline**.

## What each issue needs from a run

Neither issue can be settled by latency alone. #22's premise is a design *too large to
materialize*; #155's payoff is *not making the owned copy*. Both are memory questions,
so every scenario reports **peak RSS** next to wall time.

| Issue | The question | The numbers that answer it |
| --- | --- | --- |
| **#22** | Does the eager, fully-resident load path survive 1M nodes — and if not, is the fix a storage swap or an algorithmic one? | `from_slice`/`cache_hit` wall + **peak RSS** at 100K and 1M; scripted-navigation working set; `cone()` under the high-fan-out clock; matcher wall + RSS vs signal count; cache build time (the Phase-A analogue of "DB build time") |
| **#155** | Is skipping `rkyv::deserialize` worth an `effort: large`? | The split of a warm load into `access_unchecked` → `access_checked` → `cache_hit`: validation cost, deserialize cost, and the **RSS gap** between an archive-only read and an owned `Document` |

ADR 0003 (`docs/decisions/0003-storage-backend-for-parse-scalability.md`) allows a
**third outcome** for #22 — streaming / level-of-detail / fan-out caps instead of a
storage swap. The navigation and `cone()` rows are what make that case if it holds.

## Prerequisites (isolated machine)

- Rust toolchain per `core/rust-toolchain.toml`; every command below passes
  `--offline`, so nothing is fetched — but the crates (incl. `criterion`) must already
  be in the local cargo registry cache. If `cargo build --offline -p scale-bench` fails,
  the cache is incomplete and the run cannot proceed.
- **~2–3 GB of free RAM** for the optional 1M basis (measured peak is 1,549 MB, in the
  `prepare` step — the load itself peaks lower). It runs last on purpose.
- Close other heavy applications: peak-RSS and latency numbers are both sensitive to
  background load, and the report records free RAM at start so the reading can be judged.

## One command

```bash
cd core
cargo build --release -p scale-bench --offline
./target/release/collect                        # golden + 665 + 100K
```

Writes `core/target/scale-bench/metrics-<timestamp>.md` and prints the path. That file
is the deliverable — paste its contents back for evaluation.

Full run, including the 1M point and a real-design basis — either from a designlist,
which elaborates it for you (#255), or from a model you already have:

```bash
./target/release/collect --full --filelist design.f --top soc_top --include rtl/inc
./target/release/collect --full --model target/scale-bench/cvt-hierarchy.json
```

Useful flags: `--skip-criterion` (memory axes only, minutes instead of tens of minutes),
`--skip-report`, `--out <path>`, `--bases "golden 665"` to limit the matrix,
`--model-out <path>` to keep the elaborated JSON somewhere durable, and `--online` to
drop `--offline` from the cargo invocations (what CI uses, since a cold registry cache
makes `--offline` fail).

`collect` drives all three layers — the scenario matrix, then criterion, then the
single-shot report bin — into one paste-ready file. The *matrix* itself lives in
`scale_bench::collect`, not in the bin, because a **packaged app** (issue #240 tier 1) has
no PowerShell, no bash and no working `python3` yet must run the same matrix; `collect`
adds only the two layers that inherently need a toolchain. Three properties follow:

- **`golden` is embedded** in the binary rather than resolved through
  `CARGO_MANIFEST_DIR`, so it survives a build whose source tree is gone. It is the only
  basis measured against byte-identical input on every machine, which is what makes it the
  cross-run anchor §Findings compares against.
- **The Criterion section is a table** — `| bench | mean ms | median ms | std dev ms |`,
  parsed from `target/criterion/**/new/estimates.json` and filtered by modification time
  so a stale estimate cannot leak in. That directory keeps prior runs around, and a stale
  estimate silently mixed into the table would be worse than a missing row.
- **The criterion layer deselects the `collect` feature** — `cargo bench -p scale-bench
  --benches --no-default-features --features synth,golden`. Cargo builds a crate's *bin*
  targets for the bench profile too, so a bare `cargo bench` tries to relink `collect.exe`,
  which is the process running right now; Windows refuses to replace a locked executable
  and the layer dies with `failed to remove file collect.exe (os error 5)`. `--benches`
  does **not** help — bins are built regardless of target selection — and the `collect`
  bin's `required-features = ["collect"]` is the only thing that excludes it. Which
  benchmarks execute is unchanged.

**In CI:** the nightly `scale-bench` job builds `collect` and runs it with `--online`,
uploading the result as the `scale-bench-metrics` artifact. It is `continue-on-error`, so
benchmark noise never fails the nightly.

> **History (#246).** Until 2026-07 this matrix also existed as
> `core/scripts/scale-bench-collect.{ps1,sh}` plus `scale_bench_tables.py`. Two
> implementations of one matrix had already drifted — different failure-row fields, two
> different Criterion sections, one re-serializing the raw records and one not, CRLF+BOM
> versus LF — so the shell collectors were retired in favour of the single Rust one, whose
> output format is asserted by `core/crates/scale-bench/tests/collect.rs`. A metrics file
> produced before that change has a log-fence Criterion section and no `build` row.

Also new: a `build` row (`version @ SVX_BUILD_REV`) in the environment table. On the target
machine `rustc`, `cargo` and `git` are all absent, so it is the only provenance a metrics
file from there would otherwise carry. `app.yml` sets `SVX_BUILD_REV` from `github.sha`;
`scale-bench`'s `build.rs` declares it `rerun-if-env-changed`, because `option_env!` is read
at *compile* time and CI restores a cached `target/` — without that a rebuilt bundle would
claim the revision it was **not** built from, which is worse than the honest fallback. The
`Generated` line is UTC and names the collector.

### The packaged app's `--bench` (#240 tier 1)

On the isolated machine there is no `collect` binary, no `cargo`, and no second executable
of any kind — so the app binary *is* the collector:

```bash
hdl-schemview --bench                                   # the default matrix
hdl-schemview --bench -bases "golden 665" -out m.md
hdl-schemview --bench --full -model <hierarchy.json>
hdl-schemview --bench --full -f design.f -top soc_top -I rtl/inc
```

Long flags take one dash (EDA style) or two, interchangeably. Same matrix, same `render`,
same file. Three things differ from `collect`, all inherent:

- **The output goes to the invocation directory** as `metrics-<stamp>.md` (there is no
  `target/` to default into). `-out` overrides, resolved against that same directory.
- **The criterion and report layers are absent** — criterion needs `cargo bench` and a
  toolchain, the report bin is a second executable a bundle does not carry. Both sections
  say so instead of being silently empty, and `collect::packaged_notes` is the single place
  that pairing is decided. The file therefore always carries the ADR 0009 banner:
  *Single-shot run — no criterion layer.* Latency figures are order-of-magnitude; the
  memory axes are unaffected.
- **Each scenario runs as a child of the app re-exec'ing itself** with the hidden
  `--bench-scenario` flag, since peak RSS is only attributable to a process that did exactly
  one thing. The parent marks its children with `SVX_BENCH_CHILD`, so if a platform ever
  strips the flag from the re-exec (an AppImage `AppRun` wrapper rewriting argv is the
  realistic way) the child exits 3 with an explanation rather than opening a window per row.

A build made with `--no-default-features` carries no benchmark code at all — 2.1 MB smaller,
since the `golden` fixture is embedded — and `--bench` then refuses with
*this build was compiled without the benchmark feature*, exit 2.

## The real-design basis (realism anchor)

The synthetic generator owns the 1M point and axis isolation; a real elaborated design
owns realistic adjacency. Write a filelist, then hand it to either entry point (#255):

```bash
# filelist — packages first, then the rest, absolute Windows-style paths
cd <path-to>/claude_verilog_test
{ find rtl -name "*_pkg.sv" | sort; find rtl -name "*.sv" ! -name "*_pkg.sv" | sort; } \
  | sed "s|^|$PWD/|" > <repo>/core/target/scale-bench/cvt.f

# elaborate + measure in one step
./target/release/collect --filelist <repo>/core/target/scale-bench/cvt.f --top soc_top
hdl-schemview --bench -f <repo>/core/target/scale-bench/cvt.f -top soc_top
```

The elaboration goes through the **same argv builder** as the app's own designlist load
(`svxprobe_gui::harness_command`), so `--gate-level --name-refs` cannot be forgotten —
without them you would benchmark a smaller document than the tool actually produces, and
the numbers would still look valid. That is the whole reason this is a flag rather than a
runbook step.

The model lands in a temp scratch dir, and its path is **printed twice** — once when
elaboration starts, once in the closing summary — so a second run can skip elaboration
with `--model <that path>`. `--model-out <path>` keeps it somewhere durable instead;
prefer that if `TEMP` is swept between sessions or is on a small volume.

`design element does not have a time scale defined` warnings are non-fatal — the harness
still elaborates. A `--top` that names no module *is* fatal: the harness exits nonzero
without writing anything, so the run stops before the matrix rather than measuring a blank
model.

**Cross-check the result.** The metrics file's `| elaborated from |` row carries the node
count the harness itself reported; it must equal the `real` row's `nodes` column. A
mismatch means the matrix picked up a different file. This also catches a *partial* model
— a missing include dir elaborates most of a design, exits 0, and otherwise reports
entirely plausible numbers.

**Elaborating separately** is still supported, and is the only route for the bare `report`
and `scenario` binaries, which read `SCALE_BENCH_MODEL` and have no filelist flag:

```bash
<repo>/elaborate/.venv/Scripts/svxprobe-elaborate.exe \
  --top soc_top -f <repo>/core/target/scale-bench/cvt.f \
  --gate-level --name-refs -o <repo>/core/target/scale-bench/cvt-hierarchy.json
```

Then pass the JSON via `-model` / `--model`. Both flags are required by hand here — that
is exactly the drift `--filelist` removes.

### Doing this on an isolated machine

Both prerequisites are *network-dependent to create* and *offline to use*, so prepare
them while the machine still has a network, then verify before disconnecting:

1. **Warm the cargo registry cache** — `cd core && cargo fetch`, then prove it:
   `cargo build --release -p scale-bench --offline`. If that succeeds, the whole
   benchmark runs offline. (`cargo vendor` is the stricter alternative if the cache may
   be pruned.)
2. **Create the Python venv** — `cd elaborate && uv sync`. Elaboration itself is fully
   offline afterwards; point `SVXPROBE_ELABORATE` at
   `elaborate/.venv/Scripts/svxprobe-elaborate.exe` (or `.venv/bin/svxprobe-elaborate`)
   rather than using `uv run`, so uv never tries to re-resolve. Both entry points honour
   that variable, so the harness needs no place on `PATH`.

Then, on the isolated machine:

3. **Copy the design in** — the RTL tree only. Nothing else is fetched.
4. **Write a filelist and run the collector** with `--filelist <design.f> --top <name>`
   (`-f`/`-top` for the packaged `--bench`). Elaboration and measurement are one step,
   and the `--gate-level --name-refs` flags come from the app's own argv builder, so
   they cannot be forgotten.
5. **Budget disk.** The model is copied into a temp dir before measurement, so your design
   directory never grows a `.schemview_data/` cache — but that copy is on top of the
   elaborated JSON itself, so allow roughly 2× the JSON size plus ~0.4× for the rkyv
   archive. `--model-out <path>` puts the original on a volume you choose. (With a
   pre-elaborated `--model` the original is already yours, so it is 1× + ~0.4×.)
6. **Sanity-check the row**: the `| elaborated from |` node count must equal the `real`
   basis's `nodes` column. A mismatch means the collector picked up a different file, or
   the design only partly elaborated.

If elaboration fails on the design (unsupported constructs, missing include dirs), the
synthetic and `golden` bases still run — report which bases completed rather than
dropping the run.

## What the run actually does

Three layers, because no single tool covers all the axes:

1. **Scenarios** — `scale-bench --bin scenario`, **one measured operation per process**,
   which is the only way peak RSS is attributable. Bases: `golden` (committed
   picorv32 fixture), `665`/`100K`/`1M` (synthetic), `real`. Modes:

   | mode | measures |
   | --- | --- |
   | `prepare` | materializes the model JSON into a scratch dir and times `build_cache` — the one-time cost any Phase-B store must beat. Run first; the others need it. |
   | `from_slice` | cold path: JSON parse + referential-integrity validation + index build |
   | `cache_hit` | warm path (#21): mmap + validating access + **deserialize** + index build |
   | `access_checked` | mmap + bytecheck validation only — no deserialize (#155) |
   | `access_unchecked` | same without validation, to price validation separately (#155) |
   | `nav` | load, then walk 32 scopes × 3 iterations (`scope_graph`/`expand`/`nodes_at_path`/`nodes_at_source`) plus one high-fan-out cone on the hottest net — measured **twice**, as the uncapped legacy `cone()` (`cone_ms`/`cone_nodes`, ADR 0003's baseline) and as #244's capped `cone_with()` (`cone_with_ms`/`cone_with_nodes`/`cone_with_truncated`), so the fan-out cliff and the level-of-detail answer to it sit on one row; reports the latency spread **and** the sustained working set |
   | `match` | matcher wall + RSS at 1K / 10K / 100K signals |

   The model JSON is **copied into a temp dir** before loading, so `from_path`'s
   `.schemview_data/` cache never appears beside a committed fixture or your own design.

2. **Criterion** — `cargo bench -p scale-bench`, the statistical source of truth for
   latency at 665/100K (`load`/`query`/`matcher` groups). The collector runs it with
   `--sample-size 20 --measurement-time 3` to keep it to minutes, and only harvests
   estimates written by *that* run, so a stale result can't leak into the report.
   **The collector never runs 1M through criterion** — statistical sampling of a 4-second
   load would dominate the run time, and the scenario layer already owns that point. The
   benches themselves *can* do 1M via `SCALE_BENCH_FULL=1` (see below) for a hand-run
   investigation; the collector simply does not set it.

3. **Report bin** — the existing single-shot markdown snapshot
   (`cargo run -p scale-bench --release --bin report`).

## Running pieces by hand

```bash
cd core
cargo build --release -p scale-bench --offline

# a single scenario (JSON on stdout, progress on stderr)
./target/release/scenario --basis 100K --mode prepare
./target/release/scenario --basis 100K --mode cache_hit
# nav defaults are --nav-scopes 32 --nav-iters 3 (what the collector uses); pass
# your own only when investigating, or the row won't compare to a collected one:
./target/release/scenario --basis 100K --mode nav --nav-scopes 64 --nav-iters 5
./target/release/scenario --basis 100K --mode match --signals 100000
SCALE_BENCH_MODEL=<model.json> ./target/release/scenario --basis real --mode from_slice

# criterion
cargo bench -p scale-bench --offline                   # 665 + 100K
SCALE_BENCH_FULL=1 cargo bench -p scale-bench          # adds 1M (slow)
cargo bench -p scale-bench -- load/cache_hit           # filter

# single-shot report
cargo run -p scale-bench --release --bin report [-- --full]

# the same scenario through the packaged app's hidden child flag (#240) — the app
# binary re-execs itself this way because a bundle carries no second executable
hdl-schemview --bench-scenario --basis golden --mode cache_hit

# pre-build the #21 rkyv load cache for a model (what `prepare` times internally)
cargo run --bin svxprobe -- cache <model.json>
```

Peak/end RSS comes from `core/crates/scale-bench/src/mem.rs`, which is **dependency-free by
design** — a hand-declared `K32GetProcessMemoryInfo` on Windows, `/proc/self/status` on Linux,
plus `GlobalMemoryStatusEx` / `/proc/meminfo` for the machine RAM the environment table
reports (#240). The benchmark has to build `--offline` from the existing lockfile, so it
cannot pull in a memory-stats crate to get any of this.

The scenario logic itself lives in `scale_bench::scenario` rather than in the bin, so the
same code backs three entry points — the `scenario` binary, the `collect` driver, and the
packaged app's `--bench-scenario` arm. `cargo check -p scale-bench --no-default-features`
(a PR gate) builds it without the synthetic generator, the embedded fixture or the
collector, which is the shape a lean packaged build can ask for.

## Reading the results

- **A failure is a result.** If the 1M basis is killed by the OS, the collector records
  the row with its exit code and carries on. "1M does not fit in 16 GB" is exactly the
  datapoint #22 is gated on.
- **`access_*` vs `cache_hit` is the #155 verdict.** `cache_hit − access_checked` is
  deserialize + index build; the `load/index_build` criterion row splits that further.
  The **RSS** column matters at least as much as the time column: an archive-only read
  keeps only the mmap, while `cache_hit` also holds the owned `Document`.
- **Synthetic ≠ real for query cost.** The generator is uniform by construction, so its
  per-scope `scope_graph`/`expand` cost is far lower than a real design's at comparable
  node counts. Compare like with like: use the synthetic bases for *scaling shape*
  across sizes, and `golden`/`real` for *absolute* query latency.
- **Noise.** Wall times on a desktop under load vary by tens of percent. The criterion
  layer exists for that reason; treat single-shot scenario times as order-of-magnitude
  and read the RSS numbers as the sharper signal.

## Findings — first full run (2026-07-26)

Intel i7-9700KF, 8 threads, 15.9 GB RAM, rustc 1.96.1, bases `golden 665 100K real 1M`.
Source: `metrics-20260726-105448.md`. Numbers below are single-shot scenario measurements
unless marked criterion; treat wall times as order-of-magnitude and RSS as the sharper signal.

### The load path holds at 1M

| basis | nodes | cold `from_slice` | warm `cache_hit` | cold peak RSS | warm peak RSS |
|---|--:|--:|--:|--:|--:|
| golden | 2,079 | 11.6 ms | 3.7 ms | 9.1 MB | 9.9 MB |
| real | 7,203 | 44.6 ms | 13.4 ms | 20.1 MB | 22.8 MB |
| 100K | 100,001 | 570 ms | 150 ms | 122 MB | 153 MB |
| 1M | 1,000,001 | 4,123 ms | 1,414 ms | 1,129 MB | 1,432 MB |

**Nothing OOMs.** A 1M-node design materializes fully in ~1.1 GB. This is the headline result
for **#22**: its premise is a design *too large to materialize*, and 1M does not meet it on a
16 GB desktop. The rkyv cache (#21) is worth **3.8×** at 100K and 2.9× at 1M.

### Where warm-load time goes (#155)

| basis | `access_unchecked` | `access_checked` | `cache_hit` | ⇒ validate | ⇒ deserialize + index |
|---|--:|--:|--:|--:|--:|
| 100K | 0.28 ms | 20.1 ms | 150 ms | 19.8 ms | 130 ms |
| 1M | 0.29 ms | 294 ms | 1,414 ms | 294 ms | 1,120 ms |

A true zero-copy read-back removes **deserialize** but keeps **validation** and keeps
**index building**. Criterion splits the remainder further: at 100K, `load/index_build` is
76.8 ms of a 175 ms `load/cache_hit` — i.e. roughly *half* the post-validation cost is index
construction that zero-copy does not touch. RSS is the stronger argument: 380 MB archive-only
vs 1,432 MB owned at 1M. **Verdict: the win is real but partial**; #155 should be scoped
against the index-build cost, not against the full 1.4 s.

### Query cost tracks edge density, not node count

| basis | nodes | `scope_graph` p50 / p95 | `expand` p50 | `cone()` | cone nodes |
|---|--:|--:|--:|--:|--:|
| 665 | 666 | 6.1 / 18.4 µs | 5.9 µs | 0.3 ms | 125 |
| 100K | 100,001 | 6.5 / 47.1 µs | 5.8 µs | 26.8 ms | 10,000 |
| 1M | 1,000,001 | 7.2 / 53.8 µs | 5.6 µs | **190.8 ms** | 59,049 |
| real | 7,203 | **202.7 / 1,472 µs** | 185.4 µs | 0.04 ms | 1 |
| golden | 2,079 | **928.3 / 1,462.8 µs** | 814.9 µs | 0.05 ms | 1 |

Two things to read here:

1. **`scope_graph` does not scale with node count** — it is flat from 665 to 1M. An earlier
   roadmap claim that the full-edge scan blew up ~300× by 100K predated the scope-graph
   optimization and was stale; it would have sent the #22 decision after the wrong bottleneck.
2. **Real designs cost far more per scope than synthetic ones of any size.** The 7.2K real
   design is ~28× the 100K synthetic at 1/140th the nodes, and the 2,079-node golden is higher
   still. The generator is uniform by construction; real adjacency is not. **Never quote a
   synthetic absolute latency as a user-facing one** — use synthetic for scaling *shape* and
   `golden`/`real` for absolute numbers.

**`cone()` is the one interactive miss.** 190.8 ms on a 59K-load clock is the only operation
in the matrix that misses the sub-second-and-comfortable bar, and it is a *fan-out traversal*
cost — no storage backend makes a 59K-node cone cheap. This is the concrete target for the
level-of-detail work, and it is exactly ADR 0003's "third outcome".

### The level-of-detail answer to that cliff (#244)

Recorded as [ADR 0010](decisions/0010-schematic-trace-mode.md) — trace mode is the
consumer that turned ADR 0003's third outcome into shipped behaviour.

`cone_with` is the capped rebuild. `nav` now walks **both** on the same seed, so the two are
directly comparable within a run. Measured 2026-08-01 on a slower machine than the table above
— compare rows *within* this table, never against the 2026-07-26 absolutes:

| basis | hot fanout | legacy `cone` | boxes | `cone_with` | boxes | truncated |
|---|--:|--:|--:|--:|--:|--:|
| golden | 111 | 0.08 ms | 1 | 5.08 ms | 227 | yes |
| 100K | 10,000 | 40.6 ms | 10,000 | 0.55 ms | 90 | yes |
| 1M | 59,049 | 522.6 ms | 59,049 | **3.12 ms** | 90 | yes |

**The cliff is answered.** At 1M the same seed goes 522.6 ms → **3.12 ms**, inside the one-frame
(16 ms) budget `ConeLimits` was chosen against, with the truncation reported rather than hidden
(`SchPort.more` / `SchematicGraph.truncated`). The defaults are `depth: 4` / `fanout: 32` /
`boxes: 500` — the box budget was corrected from an extrapolated 2000 once `golden` gave a
measured **31 µs/box** (227 boxes in 7.11 ms), roughly 4× the ~8 µs the extrapolation assumed. The capped walk is flat at 90 boxes from 100K to
1M because `fanout: 32` binds at the first hop — which is the behaviour #244 PR1 asserted but,
until this run, had never measured (see the fixture note below).

**#285 changed what this row measures, not what it costs.** Making a module port one signal and
letting the walk descend through an instance box roughly doubles what a trace *draws*, because
it now reaches the logic behind a boundary instead of stopping at it. Measured before/after on
one machine (so comparable to each other, not to the table above):

| | `cone_with` | boxes | µs/box |
|---|--:|--:|--:|
| before #285 | 4.15 ms | 227 | 18.3 |
| after #285 | 14.36 ms | 460 | 31.2 |

Per-box cost lands back on the **31 µs/box** the box budget was sized against, so the extra wall
time is extra *content*, not extra cost per unit — and it stays inside the one-frame budget with
`truncated` still set. Two allocations had to go to get there: the group lookup hands out an
`Rc<[NodeId]>` rather than a fresh `Vec` (it is asked per *candidate edge* in the wall-crossing
arm, not per signal), and group membership is a linear scan of one or two ids rather than a
`HashSet` built per signal. Without those the same row measured 16.7 ms / 36 µs per box — over
the frame bar.

Two caveats worth keeping:

- **`truncated` is set on `golden` too.** #244 PR1's description reasoned that no committed
  fixture net would truncate at the defaults, counting `resetn`'s 11 *box* loads; the flag says
  otherwise at its 111 raw degree. Nothing is lost — truncation is visible by construction — but
  the "no fixture net truncates" claim does not hold.
- Legacy `cone` and `cone_with` answer *different questions* on the same seed (the legacy
  direction filter is side-blind; see below), so the box counts are not two measurements of one
  quantity. The comparison that matters is cost-to-usable-answer.

#### Fixture note: the synthetic clock edge direction

The generator used to wire every flop as a **driver** of `clk` (`b.edge(ff, clock_net,
Dir::Out)`; `Edge.dir` is relative to `e.port`, the flop). That is backwards — a flop *receives*
its clock — and it only looked right because the legacy filter ignores which end of an edge the
walk stands on. `cone_with`'s corrected filter therefore found nothing downstream and reported
**zero boxes** on every synthetic basis, which is why the cap went unmeasured at scale.

The fixture now emits `Dir::In`. Because the legacy filter is side-blind, `nav`, `benches/query.rs`
and `bin/report.rs` pass `Dir::In` for a synthetic basis to traverse the identical star at the
identical cost — `cone_nodes` is unchanged at every size (59,049 at 1M before and after), which is
the check that keeps ADR 0003's baseline intact. A `golden` or `real` model encodes directions
normally and keeps `Dir::Out` on both walks, so those rows are untouched.

### Matcher

100% hit rate at every basis and every signal count. Wall time scales with **trace signal
count**, not design size (~3 ms at 1K → ~360-580 ms at 100K across bases), and peak RSS is
dominated by the loaded design rather than the match. With the #153 `wave_index` cache this
is a first-launch cost only.

### What this means for the two open issues

- **#22** — trigger unmet at 1M. Keep open, but re-arm it on a design that actually exceeds
  RAM; the measured bottleneck is algorithmic (`cone()` fan-out), not storage.
- **#155** — worth doing, worth scoping honestly: budget against `deserialize` alone, with
  validation and index build staying put.

### Index build, attributed (2026-08-16, #238)

Measured on a different machine from the rig above, so read the **proportions**. Attributed
by ablation on `Design::from_document` — strip one input, rebuild, diff — then confirmed by
building each structure directly on identical data.

At 1M (`from_document` ≈ 1222 ms, i.e. **higher** than the ~770 ms this doc's table
extrapolated linearly from the 100K row):

| index | attributed | share |
| --- | --: | --: |
| `path_index` | 727.3 ms | 59.5% |
| `conn_index` | 449.4 ms | 36.8% |
| `src_index` | 58.5 ms | 4.8% |

**What #238 concluded, against its own premise.** The issue set out to *archive* these into
the rkyv cache. Prototyping `conn_index` first showed the win is the **layout, not the
persistence**: flattening `HashMap<NodeId, Vec<u32>>` to CSR took its build from
**251.1 ms → 14.1 ms** at 1M, while archiving the result would have saved only a further
~11 ms (build 14.1 − materialize 3.2), i.e. **0.8% of index build** — in exchange for a
`RKYV_FORMAT_VERSION` bump invalidating every cache, a bigger archive, and a staleness
guard. So the CSR shipped and the archiving did not.

The same logic is what makes `path_index` hard: at 59.5% it is the biggest term, but it is
`HashMap<String, …>`, and deserializing an archived one still allocates every key and
rehashes it. Its share also *grows* with scale (44% at 100K → 60% at 1M) as paths lengthen
with depth. Recovering it is a data-structure question (sorted key array + binary search
over one string blob), not a serialization one — and its payoff is coupled to whether #237
lands a zero-copy read model. `src_index` is not worth pursuing at 4.8%.

## Handing the results over

Paste the whole generated markdown file. It is self-contained: environment (CPU, RAM,
free RAM at start, rustc, git commit + dirty flag), every table, and the raw JSON
records at the end. If a run was aborted, say which bases completed — partial data with
a known boundary is usable; partial data presented as complete is not.
