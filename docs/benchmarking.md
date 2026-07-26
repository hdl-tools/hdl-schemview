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
- **~6 GB of free RAM** for the optional 1M basis. It runs last on purpose.
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
   **1M stays out of criterion** — the scenario layer owns that point.

3. **Report bin** — the existing single-shot markdown snapshot
   (`cargo run -p scale-bench --release --bin report`).

## Running pieces by hand

```bash
cd core
cargo build --release -p scale-bench --offline

# a single scenario (JSON on stdout, progress on stderr)
./target/release/scenario --basis 100K --mode prepare
./target/release/scenario --basis 100K --mode cache_hit
./target/release/scenario --basis 100K --mode nav --nav-scopes 64 --nav-iters 5
./target/release/scenario --basis 100K --mode match --signals 100000
SCALE_BENCH_MODEL=<model.json> ./target/release/scenario --basis real --mode from_slice

# criterion
cargo bench -p scale-bench --offline                   # 665 + 100K
SCALE_BENCH_FULL=1 cargo bench -p scale-bench          # adds 1M (slow)
cargo bench -p scale-bench -- load/cache_hit           # filter

# single-shot report
cargo run -p scale-bench --release --bin report [-- --full]
```

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

## Handing the results over

Paste the whole generated markdown file. It is self-contained: environment (CPU, RAM,
free RAM at start, rustc, git commit + dirty flag), every table, and the raw JSON
records at the end. If a run was aborted, say which bases completed — partial data with
a known boundary is usable; partial data presented as complete is not.
