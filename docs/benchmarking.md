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

```powershell
# Windows
& core/scripts/scale-bench-collect.ps1
```

```bash
# Linux / macOS / git-bash — same three layers, same output shape
bash core/scripts/scale-bench-collect.sh
```

Writes `core/target/scale-bench/metrics-<timestamp>.md` and prints the path. That file
is the deliverable — paste its contents back for evaluation.

Full run, including the 1M point and a real-design basis:

```powershell
& core/scripts/scale-bench-collect.ps1 -Full -Model core/target/scale-bench/cvt-hierarchy.json
```

Useful flags: `-SkipCriterion` (memory axes only, minutes instead of tens of minutes),
`-SkipReport`, `-Out <path>`. The bash sibling takes `--full`, `--model`,
`--skip-criterion`, `--skip-report`, `--out`, plus two it alone needs:
`--bases "golden 665"` to limit the matrix, and `--online` to drop `--offline` from the
cargo invocations (what CI uses, since a cold registry cache makes `--offline` fail).
It renders the derived tables with `scale_bench_tables.py` when a working `python3`
exists; without one the raw JSONL records still carry every number.

**In CI:** the nightly `scale-bench` job runs the bash collector with `--online` and
uploads the result as the `scale-bench-metrics` artifact. It is `continue-on-error`, so
benchmark noise never fails the nightly.

### The Rust collector (#240)

The same three layers, driven from Rust instead of a shell:

```bash
cd core
cargo build --release -p scale-bench --offline
./target/release/collect                        # golden + 665 + 100K
./target/release/collect --bases "665" --skip-criterion --skip-report
./target/release/collect --full --model <model.json> --out metrics.md
```

Flags are the bash collector's superset — `--full`, `--model`, `--bases`,
`--skip-criterion`, `--skip-report`, `--online`, `--out` — and the default output path is
unchanged. It exists because a **packaged app** (issue #240 tier 1) has no PowerShell, no
bash and no working `python3`, so the *matrix* had to move into Rust; `collect` is the
developer-side front door to the same code the app runs.

Two intentional differences from the shell collectors:

- **`golden` is embedded** in the binary rather than resolved through
  `CARGO_MANIFEST_DIR`, so it survives a build whose source tree is gone. It is the only
  basis measured against byte-identical input on every machine, which is what makes it the
  cross-run anchor §Findings compares against.
- **The Criterion section is the `.ps1`'s shape** — a `| bench | mean ms | median ms | std
  dev ms |` table parsed from `target/criterion/**/new/estimates.json`, filtered by
  modification time so a stale estimate cannot leak in. The `.sh` grepped its log into a
  bare fence instead; that shape goes away when nightly repoints to this bin.

Also new: a `build` row (`version @ SVX_BUILD_REV`) in the environment table. On the target
machine `rustc`, `cargo` and `git` are all absent, so it is the only provenance a metrics
file from there would otherwise carry. The `Generated` line is UTC and names the collector.

The `.ps1` / `.sh` / `scale_bench_tables.py` scripts **stay** for now, and nightly still
runs the `.sh`. Retiring them and repointing nightly at this bin happen **together, in one
change** (#246), gated on a full-matrix parity run — splitting them would either leave
nightly generating its artifact from unmaintained code, or delete the scripts while nightly
still calls them.

## The real-design basis (realism anchor)

The synthetic generator owns the 1M point and axis isolation; a real elaborated design
owns realistic adjacency. Producing one offline, e.g. from `claude_verilog_test`:

```bash
# 1. filelist — packages first, then the rest, absolute Windows-style paths
cd <path-to>/claude_verilog_test
{ find rtl -name "*_pkg.sv" | sort; find rtl -name "*.sv" ! -name "*_pkg.sv" | sort; } \
  | sed "s|^|$PWD/|" > <repo>/core/target/scale-bench/cvt.f

# 2. elaborate with the same flags the app passes (elaborate_and_load)
<repo>/elaborate/.venv/Scripts/svxprobe-elaborate.exe \
  --top soc_top -f <repo>/core/target/scale-bench/cvt.f \
  --gate-level --name-refs -o <repo>/core/target/scale-bench/cvt-hierarchy.json
```

`design element does not have a time scale defined` warnings are non-fatal — the harness
still elaborates. Then pass the JSON via `-Model` / `--model` (or `SCALE_BENCH_MODEL` for
the bare `report` bin and `scenario` binary).

### Doing this on an isolated machine

Both prerequisites are *network-dependent to create* and *offline to use*, so prepare
them while the machine still has a network, then verify before disconnecting:

1. **Warm the cargo registry cache** — `cd core && cargo fetch`, then prove it:
   `cargo build --release -p scale-bench --offline`. If that succeeds, the whole
   benchmark runs offline. (`cargo vendor` is the stricter alternative if the cache may
   be pruned.)
2. **Create the Python venv** — `cd elaborate && uv sync`. Elaboration itself is fully
   offline afterwards; the runbook calls
   `elaborate/.venv/Scripts/svxprobe-elaborate.exe` (or `.venv/bin/svxprobe-elaborate`)
   directly rather than `uv run`, so uv never tries to re-resolve.

Then, on the isolated machine:

3. **Copy the design in** — the RTL tree only. Nothing else is fetched.
4. **Write a filelist and elaborate** as above. Use `--gate-level --name-refs` so the
   model matches what the app actually loads; without them you benchmark a smaller
   document than the tool produces.
5. **Run the collector** with `--model <hierarchy.json>`. The model is copied into a
   temp dir first, so your design directory never grows a `.schemview_data/` cache —
   budget disk for roughly (JSON size + ~0.4× that for the rkyv archive).
6. **Sanity-check the row**: the `real` basis should report the node/edge counts the
   harness printed at elaboration. A mismatch means the collector picked up a different
   file.

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
   | `nav` | load, then walk 32 scopes × 3 iterations (`scope_graph`/`expand`/`nodes_at_path`/`nodes_at_source`) plus one high-fan-out `cone()`; reports the latency spread **and** the sustained working set |
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

## Handing the results over

Paste the whole generated markdown file. It is self-contained: environment (CPU, RAM,
free RAM at start, rustc, git commit + dirty flag), every table, and the raw JSON
records at the end. If a run was aborted, say which bases completed — partial data with
a known boundary is usable; partial data presented as complete is not.
