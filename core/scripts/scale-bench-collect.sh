#!/usr/bin/env bash
# Collect the Phase-4 scalability metrics for #22 (redb/SQLite decision) and #155
# (zero-copy rkyv read-back) into one paste-ready markdown file.
#
# POSIX sibling of scale-bench-collect.ps1 - same three layers, same output shape:
#   1. Scenarios  - one process per measurement, so peak RSS is attributable.
#   2. Criterion  - statistical latency at 665/100K.
#   3. Report bin - the single-shot markdown snapshot.
#
# Everything runs --offline, so it works on an isolated machine provided the
# crates are already in the local cargo registry cache.
#
# The derived tables are rendered by the sibling scale_bench_tables.py. python3
# is optional: without it the raw JSONL records and criterion output are still
# written, which is enough to evaluate - just less readable.
#
# Usage: scale-bench-collect.sh [--full] [--model PATH] [--bases "a b c"]
#                              [--skip-criterion] [--skip-report] [--online] [--out PATH]

set -uo pipefail

FULL=0
MODEL=""
SKIP_CRITERION=0
SKIP_REPORT=0
OUT=""
BASES_OVERRIDE=""
# --offline by default (the isolated-machine case). CI has network and may have a
# cold registry cache, where --offline would fail the build outright: --online
# drops it from every cargo invocation.
OFFLINE="--offline"

while [ $# -gt 0 ]; do
    case "$1" in
        --full) FULL=1; shift ;;
        --model) MODEL="${2:?--model needs a path}"; shift 2 ;;
        --bases) BASES_OVERRIDE="${2:?--bases needs a list}"; shift 2 ;;
        --online) OFFLINE=""; shift ;;
        --skip-criterion) SKIP_CRITERION=1; shift ;;
        --skip-report) SKIP_REPORT=1; shift ;;
        --out) OUT="${2:?--out needs a path}"; shift 2 ;;
        -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
        *) echo "unknown argument '$1'" >&2; exit 2 ;;
    esac
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
core_dir=$(dirname "$script_dir")
repo_dir=$(dirname "$core_dir")
stamp=$(date +%Y%m%d-%H%M%S)

if [ -z "$OUT" ]; then
    mkdir -p "$core_dir/target/scale-bench"
    OUT="$core_dir/target/scale-bench/metrics-$stamp.md"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
records="$work/records.jsonl"
: > "$records"

# --- environment -----------------------------------------------------------

echo "Collecting environment facts..."
cpu_name="unknown"
if [ -r /proc/cpuinfo ]; then
    cpu_name=$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -1)
elif command -v sysctl >/dev/null 2>&1; then
    cpu_name=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)
fi
if command -v nproc >/dev/null 2>&1; then cpu_threads=$(nproc); else cpu_threads="?"; fi
ram_total="?"
ram_free="?"
if [ -r /proc/meminfo ]; then
    ram_total=$(awk '/^MemTotal:/ {printf "%.1f GB", $2/1048576}' /proc/meminfo)
    ram_free=$(awk '/^MemAvailable:/ {printf "%.1f GB", $2/1048576}' /proc/meminfo)
fi
# Some /proc emulations (git-bash) omit MemAvailable; never leave a blank cell.
[ -n "$ram_total" ] || ram_total="?"
[ -n "$ram_free" ] || ram_free="?"
os_desc=$(uname -sr 2>/dev/null || echo unknown)
rustc_v=$(rustc -V 2>/dev/null || echo "not found")
cargo_v=$(cargo -V 2>/dev/null || echo "not found")
git_branch=$(git -C "$repo_dir" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "?")
git_commit=$(git -C "$repo_dir" rev-parse --short HEAD 2>/dev/null || echo "?")
if [ -n "$(git -C "$repo_dir" status --porcelain 2>/dev/null)" ]; then
    git_dirty="DIRTY ($(git -C "$repo_dir" status --porcelain | wc -l | tr -d ' ') changed files)"
else
    git_dirty="clean"
fi

# --- build -----------------------------------------------------------------

echo "Building scale-bench (release, offline)..."
if ! (cd "$core_dir" && cargo build --release -p scale-bench $OFFLINE) >"$work/build.log" 2>&1; then
    tail -30 "$work/build.log" >&2
    echo "cargo build failed. On an isolated machine this usually means a crate is missing from the local registry cache." >&2
    exit 1
fi
scenario_bin="$core_dir/target/release/scenario"
[ -x "$scenario_bin" ] || scenario_bin="$core_dir/target/release/scenario.exe"
if [ ! -x "$scenario_bin" ]; then
    echo "scenario binary not found under core/target/release" >&2
    exit 1
fi

# --- layer 1: scenarios ----------------------------------------------------

# 1M runs LAST so the smaller bases are already recorded if it swaps or dies.
bases="golden 665 100K"
if [ -n "$MODEL" ]; then
    [ -f "$MODEL" ] || { echo "--model '$MODEL' not found" >&2; exit 1; }
    SCALE_BENCH_MODEL=$(cd "$(dirname "$MODEL")" && pwd)/$(basename "$MODEL")
    export SCALE_BENCH_MODEL
    bases="$bases real"
fi
[ "$FULL" -eq 1 ] && bases="$bases 1M"
[ -n "$BASES_OVERRIDE" ] && bases="$BASES_OVERRIDE"

# Escape a string for embedding in a JSON string literal.
json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr -d '\r\n'
}

# Run one scenario; append its JSON record, or a failure record. A crash (OOM at
# 1M) is data: it is recorded, never swallowed.
run_scenario() {
    local basis="$1" mode="$2"
    shift 2
    echo "  scenario $basis/$mode $*"
    if "$scenario_bin" --basis "$basis" --mode "$mode" "$@" \
        >"$work/out.json" 2>"$work/err.txt"; then
        local line
        line=$(grep -m1 '^{' "$work/out.json" || true)
        if [ -n "$line" ]; then
            printf '%s\n' "$line" >> "$records"
            return 0
        fi
    fi
    local why
    why=$(grep -v '^[[:space:]]*$' "$work/err.txt" | tail -1)
    [ -n "$why" ] || why="no output - likely killed by the OS"
    echo "    FAILED: $why" >&2
    printf '{"basis":"%s","mode":"%s","ok":false,"error":"%s"}\n' \
        "$(json_escape "$basis")" "$(json_escape "$mode")" "$(json_escape "$why")" >> "$records"
    return 1
}

echo "Running scenarios (bases: $bases)..."
for basis in $bases; do
    prepared=0
    for mode in prepare from_slice cache_hit access_checked access_unchecked; do
        if run_scenario "$basis" "$mode"; then
            [ "$mode" = "prepare" ] && prepared=1
        fi
    done
    # A basis whose prepare failed cannot support nav/match; skip rather than
    # emit a wall of identical "run prepare first" errors.
    if [ "$prepared" -eq 1 ]; then
        run_scenario "$basis" nav || true
        for sig in 1000 10000 100000; do
            run_scenario "$basis" match --signals "$sig" || true
        done
    fi
done

# --- layer 2: criterion ----------------------------------------------------

criterion_note="skipped (--skip-criterion)"
if [ "$SKIP_CRITERION" -eq 0 ]; then
    echo "Running criterion (this takes several minutes)..."
    # Reduced sample size keeps the 100K groups to minutes rather than tens of
    # minutes; 1M stays out of criterion entirely (the scenarios own that point).
    (cd "$core_dir" && cargo bench -p scale-bench $OFFLINE -- \
        --warm-up-time 1 --measurement-time 3 --sample-size 20) \
        >"$work/criterion.log" 2>&1
    criterion_note="cargo bench -p scale-bench -- --warm-up-time 1 --measurement-time 3 --sample-size 20 (exit $?)"
fi

# --- layer 3: report bin ---------------------------------------------------

report_out="_skipped (--skip-report)_"
if [ "$SKIP_REPORT" -eq 0 ]; then
    echo "Running the single-shot report bin..."
    report_args=(run -p scale-bench --release --bin report)
    [ -n "$OFFLINE" ] && report_args=(run -p scale-bench --release "$OFFLINE" --bin report)
    [ "$FULL" -eq 1 ] && report_args+=(-- --full)
    if (cd "$core_dir" && cargo "${report_args[@]}") >"$work/report.txt" 2>"$work/report.err"; then
        report_out=$(grep '^|' "$work/report.txt")
    else
        report_out="_report bin failed: $(tail -1 "$work/report.err")_"
    fi
fi

# --- assemble --------------------------------------------------------------

echo "Writing $OUT"
{
    echo "# scale-bench metrics - #22 / #155"
    echo
    echo "Generated $(date '+%Y-%m-%d %H:%M:%S') by \`core/scripts/scale-bench-collect.sh\`."
    echo
    echo "## Environment"
    echo
    echo "| key | value |"
    echo "|---|---|"
    echo "| CPU | $cpu_name |"
    echo "| threads | $cpu_threads |"
    echo "| RAM total | $ram_total |"
    echo "| RAM available at start | $ram_free |"
    echo "| OS | $os_desc |"
    echo "| rustc | $rustc_v |"
    echo "| cargo | $cargo_v |"
    echo "| git | $git_branch @ $git_commit ($git_dirty) |"
    echo "| bases run | $bases |"
    echo "| real model | ${MODEL:-none} |"
    echo
} > "$OUT"

# Derived tables need a JSON parser. Without python3 the raw records below still
# carry every number - the report is less readable, not less complete.
# Probe by *running* the interpreter, not just resolving the name: Windows ships
# a `python3` shim that exists on PATH and fails on execution.
tables=0
for py in python3 python; do
    command -v "$py" >/dev/null 2>&1 || continue
    "$py" -c 'import json' >/dev/null 2>&1 || continue
    if "$py" "$script_dir/scale_bench_tables.py" "$records" >> "$OUT" 2>/dev/null; then
        tables=1
        break
    fi
done
if [ "$tables" -eq 0 ]; then
    echo "_no working python3 - derived tables skipped; see the raw records at the end._" >> "$OUT"
    echo >> "$OUT"
fi

{
    echo "## Criterion (statistical, 665 + 100K)"
    echo
    echo "_${criterion_note}_"
    echo
    if [ -f "$work/criterion.log" ]; then
        echo '```'
        grep -E '^(load|query|matcher)/|^ +time:' "$work/criterion.log" | head -80
        echo '```'
    else
        echo "_no criterion output from this run_"
    fi
    echo
    echo "## Single-shot report bin"
    echo
    echo "$report_out"
    echo
    echo "## Raw scenario records"
    echo
    echo '```json'
    cat "$records"
    echo '```'
} >> "$OUT"

echo
echo "Done. Metrics written to:"
echo "  $OUT"
echo "Paste that file's contents back into the conversation for evaluation."
