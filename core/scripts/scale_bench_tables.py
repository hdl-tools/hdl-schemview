#!/usr/bin/env python3
"""Render the scenario JSONL from scale-bench-collect.sh as markdown tables.

Called by the bash collector only, and only when python3 exists — the raw
records it reads are appended to the report either way, so this is a
readability layer, never the source of truth. Output matches the PowerShell
collector's tables so the two platforms produce comparable reports.

Usage: scale_bench_tables.py <records.jsonl>
"""

import json
import sys

LOAD_MODES = ("prepare", "from_slice", "cache_hit", "access_checked", "access_unchecked")


def load(path):
    rows = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line.startswith("{"):
                rows.append(json.loads(line))
    return rows


def cell(rec, key, fallback="-"):
    """A field as a table cell: floats to 2dp, missing/None as a dash."""
    if rec is None:
        return fallback
    value = rec.get(key)
    if value is None:
        return fallback
    if isinstance(value, float):
        return f"{value:,.2f}"
    return str(value)


def find(rows, basis, mode):
    for r in rows:
        if r.get("basis") == basis and r.get("mode") == mode and r.get("ok", True):
            return r
    return None


def bases_in_order(rows):
    seen = []
    for r in rows:
        b = r.get("basis")
        if b not in seen:
            seen.append(b)
    return seen


def delta(a, b):
    """a - b in ms when both are present, else a dash."""
    if a is None or b is None:
        return "-"
    return f"{a.get('wall_ms', 0) - b.get('wall_ms', 0):,.2f}"


def emit(rows):
    out = []
    add = out.append

    add("## Load + memory (one process per row; peak RSS is that process's high-water mark)")
    add("")
    add(
        "| basis | mode | nodes | edges | json MB | cache MB | wall ms "
        "| peak RSS MB | end RSS MB | note |"
    )
    add("|---|---|--:|--:|--:|--:|--:|--:|--:|---|")
    for r in rows:
        if r.get("mode") not in LOAD_MODES:
            continue
        note = "" if r.get("ok", True) else f"**FAILED** - {r.get('error', '')}"
        add(
            f"| {r.get('basis')} | {r.get('mode')} | {cell(r, 'nodes')} | {cell(r, 'edges')} "
            f"| {cell(r, 'json_mb')} | {cell(r, 'cache_mb')} | {cell(r, 'wall_ms')} "
            f"| {cell(r, 'peak_rss_mb')} | {cell(r, 'end_rss_mb')} | {note} |"
        )
    add("")

    add("### Derived: where warm-load time goes (#155)")
    add("")
    add(
        "`cache_hit` = mmap + validating access + **deserialize** + index build. "
        "`access_checked` stops after validation; `access_unchecked` skips validation too. "
        "A true zero-copy read-back removes the deserialize step but still needs the indices."
    )
    add("")
    add(
        "| basis | from_slice ms | cache_hit ms | access_checked ms | access_unchecked ms "
        "| validate ms | deserialize+index ms | cache_hit peak RSS MB | access peak RSS MB |"
    )
    add("|---|--:|--:|--:|--:|--:|--:|--:|--:|")
    for basis in bases_in_order(rows):
        fs = find(rows, basis, "from_slice")
        ch = find(rows, basis, "cache_hit")
        ac = find(rows, basis, "access_checked")
        au = find(rows, basis, "access_unchecked")
        add(
            f"| {basis} | {cell(fs, 'wall_ms')} | {cell(ch, 'wall_ms')} | {cell(ac, 'wall_ms')} "
            f"| {cell(au, 'wall_ms')} | {delta(ac, au)} | {delta(ch, ac)} "
            f"| {cell(ch, 'peak_rss_mb')} | {cell(ac, 'peak_rss_mb')} |"
        )
    add("")

    add("## Scripted navigation (working set + query spread) (#22)")
    add("")
    add(
        "| basis | load ms | RSS after load MB | peak RSS MB | scope_graph p50/p95 us "
        "| expand p50/p95 us | path p95 us | source p95 us | cone ms | cone nodes | hot fanout |"
    )
    add("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
    for r in rows:
        if r.get("mode") != "nav":
            continue
        if not r.get("ok", True):
            add(f"| {r.get('basis')} | **FAILED** - {r.get('error', '')} | | | | | | | | | |")
            continue
        sg = f"{cell(r, 'scope_graph_p50_us')} / {cell(r, 'scope_graph_p95_us')}"
        ex = f"{cell(r, 'expand_p50_us')} / {cell(r, 'expand_p95_us')}"
        add(
            f"| {r.get('basis')} | {cell(r, 'load_ms')} | {cell(r, 'rss_after_load_mb')} "
            f"| {cell(r, 'peak_rss_mb')} | {sg} | {ex} | {cell(r, 'nodes_at_path_p95_us')} "
            f"| {cell(r, 'nodes_at_source_p95_us')} | {cell(r, 'cone_ms')} "
            f"| {cell(r, 'cone_nodes')} | {cell(r, 'hot_fanout')} |"
        )
    add("")

    add("## Matcher vs trace signal count (#22)")
    add("")
    add("| basis | signals | wall ms | peak RSS MB | hit % | note |")
    add("|---|--:|--:|--:|--:|---|")
    for r in rows:
        if r.get("mode") != "match":
            continue
        note = "" if r.get("ok", True) else f"**FAILED** - {r.get('error', '')}"
        add(
            f"| {r.get('basis')} | {cell(r, 'signals')} | {cell(r, 'wall_ms')} "
            f"| {cell(r, 'peak_rss_mb')} | {cell(r, 'hit_rate_pct')} | {note} |"
        )
    add("")
    return "\n".join(out)


def main():
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    print(emit(load(sys.argv[1])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
