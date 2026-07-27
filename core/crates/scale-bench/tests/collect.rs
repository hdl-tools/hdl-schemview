//! The metrics file's format (#240).
//!
//! This is the coverage the shell collectors never had: the PowerShell and bash
//! renderers had already drifted apart (different failure rows, different
//! criterion sections, one re-serializing the raw records), and nothing would have
//! caught it. `render` is pure precisely so these can be asserted without
//! spawning a scenario.
//!
//! The number formatting below was the parity contract with the retired
//! `core/scripts/scale_bench_tables.py` (#246) and is now the contract outright —
//! floats to two decimals with thousands separators, integers ungrouped, absent
//! values as `-`. Metrics files are compared across runs, so it must not drift.

use scale_bench::collect::{self, CriterionRow, EnvFacts, Notes, Record};

fn facts() -> EnvFacts {
    EnvFacts {
        generated: "2026-07-26 10:54:48 UTC".into(),
        generator: "scale-bench collect".into(),
        cpu: "Test CPU".into(),
        threads: "8".into(),
        ram_total: "15.9 GB".into(),
        ram_available: "8.0 GB".into(),
        os: "windows x86_64".into(),
        rustc: "rustc 1.94.0".into(),
        cargo: "cargo 1.94.0".into(),
        git: "main @ abc1234 (clean)".into(),
        build: "0.0.0 @ deadbee".into(),
        bases: vec!["golden".into(), "665".into()],
        model: "none".into(),
    }
}

/// A scenario line exactly as the scenario emits it: counts bare, measurements at
/// four decimals, unavailable probes as `null`.
fn record(json: &str) -> Record {
    Record::parse(json).expect("valid scenario line")
}

fn load_row(basis: &str, mode: &str, wall_ms: &str) -> Record {
    record(&format!(
        r#"{{"nodes":100001,"edges":120000,"json_mb":57.1234,"wall_ms":{wall_ms},"peak_rss_mb":512.5000,"end_rss_mb":480.2500,"basis":"{basis}","mode":"{mode}"}}"#
    ))
}

fn rendered(records: &[Record], notes: &Notes) -> String {
    collect::render(&facts(), records, notes)
}

#[test]
fn sections_appear_in_the_documented_order() {
    let text = rendered(&[], &Notes::default());
    let expected = [
        "# scale-bench metrics - #22 / #155",
        "## Environment",
        "## Load + memory (one process per row; peak RSS is that process's high-water mark)",
        "### Derived: where warm-load time goes (#155)",
        "## Scripted navigation (working set + query spread) (#22)",
        "## Matcher vs trace signal count (#22)",
        "## Criterion (statistical, 665 + 100K)",
        "## Single-shot report bin",
        "## Raw scenario records",
    ];
    let mut cursor = 0;
    for heading in expected {
        let at = text[cursor..]
            .find(heading)
            .unwrap_or_else(|| panic!("missing or out-of-order section: {heading}"));
        cursor += at + heading.len();
    }
}

#[test]
fn column_headers_match_the_python_renderer_verbatim() {
    let text = rendered(&[], &Notes::default());
    for header in [
        "| basis | mode | nodes | edges | json MB | cache MB | wall ms | peak RSS MB | end RSS MB | note |",
        "|---|---|--:|--:|--:|--:|--:|--:|--:|---|",
        "| basis | from_slice ms | cache_hit ms | access_checked ms | access_unchecked ms | validate ms | deserialize+index ms | cache_hit peak RSS MB | access peak RSS MB |",
        "| basis | load ms | RSS after load MB | peak RSS MB | scope_graph p50/p95 us | expand p50/p95 us | path p95 us | source p95 us | cone ms | cone nodes | hot fanout |",
        "| basis | signals | wall ms | peak RSS MB | hit % | note |",
        "|---|--:|--:|--:|--:|---|",
    ] {
        assert!(text.contains(header), "missing header:\n{header}");
    }
}

#[test]
fn counts_stay_ungrouped_while_measurements_get_two_decimals() {
    // The whole reason `cell` keys off the JSON number type: formatting both
    // alike would print `nodes` as `100,001.00`.
    let text = rendered(
        &[load_row("100K", "from_slice", "1432.0000")],
        &Notes::default(),
    );
    let row = text
        .lines()
        .find(|l| l.starts_with("| 100K | from_slice |"))
        .expect("the load row is rendered");
    assert_eq!(
        row,
        "| 100K | from_slice | 100001 | 120000 | 57.12 | - | 1,432.00 | 512.50 | 480.25 |  |"
    );
}

#[test]
fn thousands_matches_powershell_n2_and_python_comma_2f() {
    assert_eq!(collect::thousands(1432.0), "1,432.00");
    assert_eq!(collect::thousands(0.0), "0.00");
    assert_eq!(collect::thousands(999.994), "999.99");
    assert_eq!(collect::thousands(1_234_567.891), "1,234,567.89");
    // Exactly three digits must not gain a leading separator.
    assert_eq!(collect::thousands(100.0), "100.00");
    // A negative derived delta keeps the sign outside the grouping.
    assert_eq!(collect::thousands(-1234.5), "-1,234.50");
    // A negative delta smaller than half a hundredth still reads as negative —
    // Python's `{:,.2f}` does the same, and hiding the sign would misreport the
    // direction of a near-tie.
    assert_eq!(collect::thousands(-0.004), "-0.00");
    // Exact negative zero is the one deliberate divergence: it cannot arise from
    // `a - b` on equal operands (IEEE gives +0.0), and "-0.00" for a true zero
    // would only look like a bug.
    assert_eq!(collect::thousands(-0.0), "0.00");
}

#[test]
fn derived_columns_are_the_two_documented_subtractions() {
    let records = [
        load_row("100K", "from_slice", "570.0000"),
        load_row("100K", "cache_hit", "150.0000"),
        load_row("100K", "access_checked", "40.0000"),
        load_row("100K", "access_unchecked", "10.0000"),
    ];
    let text = rendered(&records, &Notes::default());
    let row = text
        .lines()
        .find(|l| l.starts_with("| 100K | 570.00 |"))
        .expect("the derived row is rendered");
    // validate = access_checked - access_unchecked; deserialize+index =
    // cache_hit - access_checked.
    assert!(row.contains("| 30.00 | 110.00 |"), "unexpected row: {row}");
}

#[test]
fn a_negative_delta_can_happen_and_renders_signed() {
    // On a noisy run the unchecked access can measure slower than the checked
    // one. That is a real observation, not something to clamp away.
    let records = [
        load_row("665", "access_checked", "10.0000"),
        load_row("665", "access_unchecked", "12.5000"),
    ];
    let text = rendered(&records, &Notes::default());
    assert!(
        text.lines().any(|l| l.contains("| -2.50 |")),
        "expected a negative validate delta:\n{text}"
    );
}

#[test]
fn a_missing_row_leaves_a_dash_rather_than_dropping_a_column() {
    let text = rendered(
        &[load_row("665", "from_slice", "12.0000")],
        &Notes::default(),
    );
    let row = text
        .lines()
        .find(|l| l.starts_with("| 665 | 12.00 |"))
        .expect("the derived row is rendered");
    assert_eq!(row.matches('|').count(), 10, "arity changed: {row}");
    // cache_hit / access_* were never run for this basis.
    assert!(row.contains("| - | - | - | - | - | - |"), "row: {row}");
}

#[test]
fn a_failed_load_row_carries_the_reason_and_the_exit_code() {
    let rec = Record::failure("1M", "cache_hit", 137, "killed", 4200.0);
    assert!(!rec.ok());
    assert_eq!(rec.basis(), "1M");
    let text = rendered(std::slice::from_ref(&rec), &Notes::default());
    assert!(
        text.contains(
            "| 1M | cache_hit | - | - | - | - | 4,200.00 | - | - | **FAILED** - killed |"
        ),
        "unexpected load row:\n{text}"
    );
    // The exit code rides in the raw record, which is the primary data.
    assert!(rec.raw.contains("\"exit_code\":137"), "raw: {}", rec.raw);
}

#[test]
fn a_failed_nav_row_keeps_the_tables_arity() {
    let text = rendered(
        &[Record::failure("1M", "nav", -1, "out of memory", 900.0)],
        &Notes::default(),
    );
    let row = text
        .lines()
        .find(|l| l.contains("**FAILED** - out of memory") && l.starts_with("| 1M |"))
        .expect("the nav failure row is rendered");
    assert_eq!(row, "| 1M | **FAILED** - out of memory | | | | | | | | | |");
}

#[test]
fn a_nav_row_with_no_percentile_samples_degrades_to_dashes() {
    // `summarize` emits only `<name>_mean_us`, as null, when its sample vector is
    // empty — the record shape is not fixed-arity, so the composite p50/p95 cells
    // have to tolerate absent keys.
    let rec = record(
        r#"{"nodes":665,"edges":700,"load_ms":12.0000,"rss_after_load_mb":null,"nav_scopes":1,"nav_iters":1,"cone_ms":0.5000,"cone_nodes":3,"hot_fanout":9,"scope_graph_mean_us":null,"expand_mean_us":null,"wall_ms":1.0000,"peak_rss_mb":40.0000,"end_rss_mb":39.0000,"basis":"golden","mode":"nav"}"#,
    );
    let text = rendered(&[rec], &Notes::default());
    let row = text
        .lines()
        .find(|l| l.starts_with("| golden | 12.00 |"))
        .expect("the nav row is rendered");
    assert!(row.contains("| - / - | - / - |"), "row: {row}");
    // An unavailable RSS probe is a dash, never a fabricated zero.
    assert!(row.contains("| 12.00 | - |"), "row: {row}");
}

#[test]
fn the_single_shot_banner_appears_only_without_criterion() {
    let banner = "**Single-shot run — no criterion layer.**";
    let without = rendered(&[], &Notes::default());
    assert!(without.contains(banner), "packaged runs must say so");

    let with = rendered(
        &[],
        &Notes {
            criterion_ran: true,
            criterion_note: "cargo bench (exit 0)".into(),
            criterion_rows: vec![CriterionRow {
                id: "load/from_slice/665".into(),
                mean_ms: 1.2345,
                median_ms: 1.2000,
                std_dev_ms: 0.0500,
            }],
            ..Notes::default()
        },
    );
    assert!(
        !with.contains(banner),
        "a criterion run must not carry the single-shot caveat"
    );
    assert!(with.contains("| bench | mean ms | median ms | std dev ms |"));
    assert!(with.contains("| load/from_slice/665 | 1.234 | 1.200 | 0.050 |"));
}

#[test]
fn packaged_notes_carry_the_banner_and_explain_both_missing_layers() {
    // The packaged app's one supported Notes shape (#240 tier 1). Pinned here
    // because the banner is ADR 0009's guard against a single-shot file being
    // compared against a `cargo bench` one.
    let notes = collect::packaged_notes();
    assert!(!notes.criterion_ran);
    assert!(notes.criterion_rows.is_empty());
    assert!(notes.report_table.is_empty());

    let text = rendered(&[], &notes);
    assert!(text.contains("**Single-shot run — no criterion layer.**"));
    // Each absent layer says *why* it is absent, so the file is self-explaining
    // on a machine where nobody can go check.
    assert!(text.contains("cargo bench"), "text: {text}");
    assert!(text.contains("`report` bin"), "text: {text}");
}

#[test]
fn criterion_and_report_notes_are_italicized_where_a_table_is_absent() {
    let text = rendered(
        &[],
        &Notes {
            criterion_note: "skipped (--skip-criterion)".into(),
            report_note: Some("skipped (--skip-report)".into()),
            ..Notes::default()
        },
    );
    assert!(text.contains("_skipped (--skip-criterion)_"));
    assert!(text.contains("_no criterion estimates from this run_"));
    assert!(text.contains("_skipped (--skip-report)_"));
}

#[test]
fn raw_records_are_reproduced_verbatim_and_in_order() {
    // Verbatim, not re-serialized: the scenario's own line is the primary datum,
    // and re-encoding it would reorder keys and invent an `ok` field.
    let first = load_row("golden", "prepare", "5.0000");
    let second = load_row("665", "from_slice", "6.0000");
    let text = rendered(&[first.clone(), second.clone()], &Notes::default());
    let raw = text.split("```json\n").nth(1).expect("a raw records block");
    let lines: Vec<&str> = raw.lines().take(2).collect();
    assert_eq!(lines, vec![first.raw.as_str(), second.raw.as_str()]);
}

#[test]
fn the_environment_table_names_the_build_even_with_no_toolchain() {
    // The row the scripts have no equivalent for: on the target machine rustc,
    // cargo and git are all absent, so this is the only provenance left.
    let text = rendered(&[], &Notes::default());
    assert!(text.contains("| build | 0.0.0 @ deadbee |"));
    assert!(text.contains("| bases run | golden, 665 |"));
    assert!(text.contains("Generated 2026-07-26 10:54:48 UTC by `scale-bench collect`."));
}

#[test]
fn the_file_is_lf_with_no_bom_and_ends_in_a_newline() {
    let text = rendered(&[load_row("665", "prepare", "1.0000")], &Notes::default());
    assert!(!text.contains('\r'), "CRLF leaked into the metrics file");
    assert!(!text.starts_with('\u{feff}'), "a BOM leaked in");
    assert!(text.ends_with('\n'));
}

#[test]
fn nav_and_match_are_skipped_when_prepare_failed() {
    // Without `prepare` they can only emit identical "run prepare first" errors,
    // so the driver skips them rather than filling the tables with noise.
    assert!(collect::gated_steps(false).is_empty());
    let steps = collect::gated_steps(true);
    assert_eq!(steps.len(), 1 + collect::MATCH_SIGNALS.len());
    assert_eq!(steps[0].mode, "nav");
    assert_eq!(steps[0].signals, None);
    let signals: Vec<Option<usize>> = steps[1..].iter().map(|s| s.signals).collect();
    assert_eq!(signals, vec![Some(1_000), Some(10_000), Some(100_000)]);
    assert!(steps[1..].iter().all(|s| s.mode == "match"));
}

#[test]
fn one_m_is_always_the_last_basis() {
    // It is the basis that may be killed by the OS; every smaller one must
    // already be recorded when that happens.
    let bases = collect::default_bases(true, true);
    assert_eq!(bases.last().map(String::as_str), Some("1M"));
    assert!(bases.contains(&"real".to_string()));
    // Without --full there is no 1M row at all.
    assert!(!collect::default_bases(false, false).contains(&"1M".to_string()));
}

#[test]
fn last_nonblank_survives_crlf_and_trailing_newlines() {
    // A child's stderr is what a failure row quotes; a stray carriage return
    // would break the markdown cell it lands in.
    assert_eq!(
        collect::last_nonblank("first\r\nsecond failed\r\n\r\n"),
        Some("second failed")
    );
    assert_eq!(collect::last_nonblank("   \n\n"), None);
    assert_eq!(collect::last_nonblank(""), None);
}

#[test]
fn a_non_object_line_is_not_a_record() {
    // The driver's success test is "exit 0 and a `{`-prefixed line"; anything else
    // has to become a failure row rather than a half-parsed one.
    assert!(Record::parse("not json").is_none());
    assert!(Record::parse("[1,2]").is_none());
    assert!(Record::parse("").is_none());
    assert!(Record::parse(r#"  {"basis":"665","mode":"nav"}  "#).is_some());
}

#[test]
fn timestamps_render_as_the_documented_shapes() {
    // 2026-07-26T10:54:48Z.
    let secs = 1_785_063_288;
    assert_eq!(collect::format_utc(secs), "2026-07-26 10:54:48 UTC");
    assert_eq!(collect::stamp_utc(secs), "20260726-105448");
    // The epoch itself, as a boundary on the civil-date maths.
    assert_eq!(collect::format_utc(0), "1970-01-01 00:00:00 UTC");
    // A leap day, which the March-based year shift exists to get right.
    assert_eq!(
        collect::format_utc(1_709_208_000),
        "2024-02-29 12:00:00 UTC"
    );
}
