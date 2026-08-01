//! The scenario matrix driver and metrics renderer (#240).
//!
//! The matrix used to live in `core/scripts/scale-bench-collect.ps1`, `.sh` and
//! `scale_bench_tables.py`; it is here instead because of packaging. On the
//! machine this benchmark exists to measure there is no PowerShell, no bash and
//! no working `python3` (the Windows `python3` shim resolves on PATH and fails on
//! execution — it broke the bash collector once already). Folding the logic in
//! here also collapsed the ps1/sh duplication that had already produced two
//! different failure-row shapes and two different criterion sections; those
//! scripts were retired in #246 once this had a full-matrix parity run.
//!
//! Three properties are load-bearing and must survive any edit:
//!
//! * **One measured operation per process.** Peak RSS is only attributable to a
//!   process that did exactly one thing, so [`run`] spawns a child per scenario
//!   rather than looping in-process. [`ScenarioRunner`] says *how* to spawn one,
//!   which is what lets the packaged app re-exec itself while the dev bin runs
//!   its sibling `scenario` binary.
//! * **A failure is a datapoint.** A scenario that exits non-zero — or is killed
//!   outright, which is the expected 1M outcome on a small machine — becomes a
//!   `**FAILED**` row and the run continues. That row *is* the #22 answer for
//!   that size; losing it would be worse than losing the run.
//! * **[`render`] is pure.** `(facts, records, notes) -> String`, so the output
//!   format is testable without spawning anything. Nothing in any language
//!   tested it before.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::scenario;

/// Signal counts the matcher axis sweeps, in order.
pub const MATCH_SIGNALS: [usize; 3] = [1_000, 10_000, 100_000];

/// One scenario invocation in the matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub mode: &'static str,
    pub signals: Option<usize>,
}

impl Step {
    fn mode(mode: &'static str) -> Self {
        Step {
            mode,
            signals: None,
        }
    }
}

/// The steps a basis runs *after* its load modes, gated on whether `prepare`
/// succeeded.
///
/// `nav` and `match` both read what `prepare` materialized, so without it they
/// can only produce a wall of identical "run prepare first" errors — the scripts
/// skip them for exactly that reason, and this function is where that decision
/// lives so it can be tested directly.
pub fn gated_steps(prepared: bool) -> Vec<Step> {
    if !prepared {
        return Vec::new();
    }
    let mut steps = vec![Step::mode("nav")];
    steps.extend(MATCH_SIGNALS.iter().map(|&n| Step {
        mode: "match",
        signals: Some(n),
    }));
    steps
}

/// The bases to run, in order, for the enabled feature set.
///
/// **`1M` is last on purpose**: it is the basis that may be killed by the OS, and
/// every smaller basis must already be recorded when that happens.
pub fn default_bases(full: bool, model: bool) -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    if cfg!(feature = "golden") {
        bases.push("golden".into());
    }
    if cfg!(feature = "synth") {
        bases.push("665".into());
        bases.push("100K".into());
    }
    if model {
        bases.push("real".into());
    }
    if full && cfg!(feature = "synth") {
        bases.push("1M".into());
    }
    bases
}

// --- records ----------------------------------------------------------------

/// One scenario result: the child's own JSON line plus its parsed fields.
///
/// `raw` is kept verbatim so the "Raw scenario records" section reproduces
/// exactly what the scenario emitted — re-serializing would reorder keys and
/// invent an `ok` field the scenario never wrote.
#[derive(Debug, Clone)]
pub struct Record {
    pub raw: String,
    fields: Map<String, Value>,
}

impl Record {
    /// Parse a scenario's JSON line. Returns `None` if it is not a JSON object,
    /// which the driver treats as "the scenario produced no usable output".
    pub fn parse(line: &str) -> Option<Self> {
        match serde_json::from_str::<Value>(line.trim()) {
            Ok(Value::Object(fields)) => Some(Record {
                raw: line.trim().to_string(),
                fields,
            }),
            _ => None,
        }
    }

    /// A synthesized row for a scenario that failed or never reported.
    ///
    /// Carries `exit_code` and `wall_ms` — the `.ps1` shape. `docs/benchmarking.md`
    /// promises the exit code ("records the row with its exit code"), which the
    /// `.sh` failure row silently dropped.
    pub fn failure(basis: &str, mode: &str, exit_code: i32, error: &str, wall_ms: f64) -> Self {
        let mut fields = Map::new();
        fields.insert("basis".into(), Value::String(basis.into()));
        fields.insert("mode".into(), Value::String(mode.into()));
        fields.insert("ok".into(), Value::Bool(false));
        fields.insert("exit_code".into(), Value::from(exit_code));
        fields.insert("error".into(), Value::String(error.into()));
        fields.insert(
            "wall_ms".into(),
            serde_json::Number::from_f64(round4(wall_ms))
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        let raw = Value::Object(fields.clone()).to_string();
        Record { raw, fields }
    }

    pub fn basis(&self) -> &str {
        self.fields
            .get("basis")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    pub fn mode(&self) -> &str {
        self.fields
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// Success unless explicitly marked otherwise — a scenario's own line has no
    /// `ok` field, matching the renderers' `r.get("ok", True)`.
    pub fn ok(&self) -> bool {
        self.fields
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn error(&self) -> &str {
        self.fields
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn wall_ms(&self) -> f64 {
        self.fields
            .get("wall_ms")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    }

    /// A field as a table cell.
    ///
    /// The float-vs-integer split is the parity contract, not a formatting
    /// preference: the scenario emits every measurement through its `num` helper
    /// (always 4 decimals, so it reads back as a JSON float) and every count
    /// through `int` (bare digits). Floats get 2 decimals and thousands
    /// separators; integers pass through ungrouped. Formatting both alike would
    /// print `nodes` as `100,001.00`.
    fn cell(&self, key: &str) -> String {
        match self.fields.get(key) {
            None | Some(Value::Null) => "-".into(),
            Some(Value::Number(n)) => match n.as_f64() {
                // `is_f64` is false for a bare integer literal, which is exactly
                // the distinction the scenario encoded.
                Some(v) if n.is_f64() => thousands(v),
                _ => n.to_string(),
            },
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }
    }
}

/// A cell for a possibly-absent record, so a missing row renders as `-` rather
/// than dropping a column.
fn cell_of(rec: Option<&Record>, key: &str) -> String {
    rec.map(|r| r.cell(key)).unwrap_or_else(|| "-".into())
}

/// `a - b` in ms when both rows are present, else a dash.
fn delta(a: Option<&Record>, b: Option<&Record>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => thousands(a.wall_ms() - b.wall_ms()),
        _ => "-".into(),
    }
}

fn find<'a>(records: &'a [Record], basis: &str, mode: &str) -> Option<&'a Record> {
    records
        .iter()
        .find(|r| r.basis() == basis && r.mode() == mode && r.ok())
}

fn bases_in_order(records: &[Record]) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for r in records {
        if !seen.contains(&r.basis()) {
            seen.push(r.basis());
        }
    }
    seen
}

/// Two decimals with `,` thousands separators and `.` as the decimal point —
/// what PowerShell's `{0:N2}` and Python's `{:,.2f}` produce, hardcoded rather
/// than locale-dependent so a run in another locale stays comparable to the
/// numbers already published in `docs/benchmarking.md` §Findings.
///
/// The sign leads: the derived `validate` / `deserialize+index` columns are
/// subtractions and can legitimately come out negative on a noisy run.
pub fn thousands(value: f64) -> String {
    let negative = value.is_sign_negative() && value != 0.0;
    let formatted = format!("{:.2}", value.abs());
    let (int_part, frac) = formatted
        .split_once('.')
        .unwrap_or((formatted.as_str(), "00"));
    let mut grouped = String::new();
    for (i, ch) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{}{grouped}.{frac}", if negative { "-" } else { "" })
}

fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

// --- running the matrix -----------------------------------------------------

/// How to run one scenario as its own process.
///
/// The dev `collect` bin points this at the sibling `scenario` binary; the
/// packaged app points it at [`std::env::current_exe`] with a
/// `--bench-scenario` prefix, because a bundle contains no second executable.
/// Either way the measured code is the same code.
#[derive(Debug, Clone)]
pub struct ScenarioRunner {
    pub exe: PathBuf,
    pub prefix_args: Vec<String>,
}

impl ScenarioRunner {
    pub fn new(exe: impl Into<PathBuf>, prefix_args: Vec<String>) -> Self {
        ScenarioRunner {
            exe: exe.into(),
            prefix_args,
        }
    }
}

pub struct CollectOptions<'a> {
    pub runner: &'a ScenarioRunner,
    /// Bases in run order — see [`default_bases`] for why `1M` goes last.
    pub bases: Vec<String>,
    /// An elaborated `hierarchy.json` for the `real` basis.
    pub model: Option<PathBuf>,
}

#[derive(Debug)]
pub struct CollectResult {
    pub records: Vec<Record>,
}

/// Run the matrix, one child process per row.
///
/// Returns `Err` only for a problem that would make every row meaningless (an
/// unusable runner). Individual scenario failures are recorded, never
/// propagated.
pub fn run(opts: &CollectOptions<'_>) -> anyhow::Result<CollectResult> {
    // Check the runner once, up front: a missing binary would otherwise be
    // reported as N identical failure rows that look like measurements.
    if !opts.runner.exe.is_file() {
        anyhow::bail!("scenario runner not found: {}", opts.runner.exe.display());
    }

    let mut records = Vec::new();
    for basis in &opts.bases {
        let mut prepared = false;
        for mode in scenario::LOAD_MODES {
            let rec = run_one(opts, basis, &Step::mode(mode));
            if mode == "prepare" && rec.ok() {
                prepared = true;
            }
            records.push(rec);
        }
        if !prepared {
            eprintln!("  {basis}: prepare failed — skipping nav/match for this basis");
        }
        for step in gated_steps(prepared) {
            records.push(run_one(opts, basis, &step));
        }
    }
    Ok(CollectResult { records })
}

fn run_one(opts: &CollectOptions<'_>, basis: &str, step: &Step) -> Record {
    let label = match step.signals {
        Some(n) => format!("{basis}/{} --signals {n}", step.mode),
        None => format!("{basis}/{}", step.mode),
    };
    eprintln!("  {label}");

    let mut cmd = Command::new(&opts.runner.exe);
    cmd.args(&opts.runner.prefix_args)
        .args(["--basis", basis, "--mode", step.mode]);
    if let Some(n) = step.signals {
        cmd.args(["--signals", &n.to_string()]);
    }
    if let Some(model) = &opts.model {
        cmd.env("SCALE_BENCH_MODEL", model);
    }

    let t = Instant::now();
    let output = cmd.output();
    let wall_ms = t.elapsed().as_secs_f64() * 1e3;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            let why = format!("could not spawn the scenario: {e}");
            eprintln!("    FAILED: {why}");
            return Record::failure(basis, step.mode, -1, &why, wall_ms);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().map(str::trim).find(|l| l.starts_with('{'));
    if output.status.success() {
        if let Some(rec) = line.and_then(Record::parse) {
            return rec;
        }
    }

    // A crash or an OOM kill lands here, which is the point: the row is the
    // result. Prefer the child's own last complaint over a synthesized one.
    let code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut why = last_nonblank(&stderr).unwrap_or_default().to_string();
    if why.is_empty() {
        why = match code {
            Some(c) => format!("no output (exit {c}) - likely killed by the OS"),
            None => "no output (killed by a signal) - likely killed by the OS".into(),
        };
    } else if code.is_none() {
        why.push_str(" (killed by a signal)");
    }
    eprintln!("    FAILED: {why}");
    Record::failure(basis, step.mode, code.unwrap_or(-1), &why, wall_ms)
}

/// The last non-blank line of a child's stderr — its most specific complaint.
///
/// Trims each line, so a `\r\n` stream does not leave a stray carriage return
/// inside a markdown table cell.
pub fn last_nonblank(text: &str) -> Option<&str> {
    // Searched from the back: `Lines` is double-ended, so walking the whole
    // stderr stream forward would be wasted work.
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

// --- environment ------------------------------------------------------------

/// Everything the metrics file's provenance depends on. Absent values are
/// `not found` rather than blank, so a packaged run reads as "this machine has
/// no toolchain" instead of "someone forgot a column".
#[derive(Debug, Clone)]
pub struct EnvFacts {
    pub generated: String,
    /// What produced the file — a packaged run must be identifiable as such.
    pub generator: String,
    pub cpu: String,
    pub threads: String,
    pub ram_total: String,
    pub ram_available: String,
    pub os: String,
    pub rustc: String,
    pub cargo: String,
    pub git: String,
    pub build: String,
    pub bases: Vec<String>,
    pub model: String,
}

/// Probe the machine. Impure by nature — [`render`] takes the result instead of
/// gathering it, so the formatting stays testable.
pub fn env_facts(bases: &[String], model: Option<&Path>) -> EnvFacts {
    EnvFacts {
        generated: format_utc(now_unix()),
        generator: String::new(),
        cpu: cpu_name(),
        threads: std::thread::available_parallelism()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| "?".into()),
        ram_total: ram_gb(|(total, _)| total),
        ram_available: ram_gb(|(_, avail)| avail),
        os: os_name(),
        rustc: probe("rustc", &["-V"]).unwrap_or_else(|| "not found".into()),
        cargo: probe("cargo", &["-V"]).unwrap_or_else(|| "not found".into()),
        git: git_revision(),
        build: build_provenance(),
        bases: bases.to_vec(),
        model: model
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".into()),
    }
}

/// `version @ rev` for the binary that produced the file.
///
/// The scripts have no equivalent because they always run in a git checkout with
/// a toolchain. A packaged binary has neither, so without this row a metrics file
/// from the target machine would carry **no** provenance at all. CI sets
/// `SVX_BUILD_REV`.
fn build_provenance() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("SVX_BUILD_REV") {
        Some(rev) if !rev.is_empty() => format!("{version} @ {rev}"),
        _ => format!("{version} (no SVX_BUILD_REV at build time)"),
    }
}

fn ram_gb(pick: impl Fn((u64, u64)) -> u64) -> String {
    match crate::mem::ram_bytes() {
        Some(pair) => format!("{:.1} GB", pick(pair) as f64 / 1e9),
        None => "?".into(),
    }
}

/// CPU model, best effort.
///
/// On Windows this is `PROCESSOR_IDENTIFIER` — coarser than the `.ps1`'s WMI
/// `Name` (a family string rather than a marketing name), and a deliberate
/// trade: the alternative is a WMI or registry round trip for a row that only
/// labels the run.
fn cpu_name() -> String {
    #[cfg(target_os = "linux")]
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(name) = info
            .lines()
            .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')))
            .map(|(_, v)| v.trim().to_string())
        {
            return name;
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(name) = probe("sysctl", &["-n", "machdep.cpu.brand_string"]) {
        return name;
    }
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into())
}

/// OS identity, best effort — coarser than the `.ps1`'s WMI caption or the
/// `.sh`'s `uname -sr`, for the same reason as [`cpu_name`].
fn os_name() -> String {
    let base = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    #[cfg(target_os = "linux")]
    if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        return format!("{base} {}", release.trim());
    }
    base
}

fn git_revision() -> String {
    let (Some(branch), Some(sha)) = (
        probe("git", &["rev-parse", "--abbrev-ref", "HEAD"]),
        probe("git", &["rev-parse", "--short", "HEAD"]),
    ) else {
        return "not found".into();
    };
    let dirty = match probe("git", &["status", "--porcelain"]) {
        Some(s) if !s.is_empty() => format!("DIRTY ({} changed files)", s.lines().count()),
        _ => "clean".into(),
    };
    format!("{branch} @ {sha} ({dirty})")
}

/// Run a short command and take its output, or `None` if it is not usable.
///
/// Probes by *running* the tool rather than resolving its name — the lesson of
/// the Windows `python3` shim, which exists on PATH and fails on execution.
fn probe(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

// --- time -------------------------------------------------------------------

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `(year, month, day, hour, minute, second)` in UTC for a Unix timestamp.
///
/// UTC, not local time: the alternative is a per-OS timezone call for a label,
/// and a *mislabelled* local time would be worse than an honest UTC one when
/// comparing two runs from different machines. The scripts print unlabelled local
/// time, so this line differing between them is expected.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days-to-civil (Howard Hinnant's algorithm): shift to a March-based year so
    // the leap day lands at the end and no month-length table is needed.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        m,
        d,
        (rem / 3_600) as u32,
        ((rem % 3_600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// `2026-07-26 10:54:48 UTC` — the `Generated` line.
pub fn format_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// `20260726-105448` — the default metrics filename stamp.
pub fn stamp_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

// --- rendering --------------------------------------------------------------

/// One harvested criterion estimate.
#[derive(Debug, Clone)]
pub struct CriterionRow {
    pub id: String,
    pub mean_ms: f64,
    pub median_ms: f64,
    pub std_dev_ms: f64,
}

/// The two sections [`run`] cannot produce itself: criterion needs `cargo bench`
/// and the report bin needs a second process, so both are the caller's to supply.
#[derive(Debug, Clone, Default)]
pub struct Notes {
    /// The italicized note under `## Criterion`.
    pub criterion_note: String,
    pub criterion_rows: Vec<CriterionRow>,
    /// Whether the criterion layer ran at all — drives the single-shot banner.
    pub criterion_ran: bool,
    /// A note *instead of* the report table (skipped, or failed).
    pub report_note: Option<String>,
    /// The report bin's `|`-prefixed lines.
    pub report_table: Vec<String>,
}

/// The [`Notes`] a packaged run must use: neither extra layer can exist there.
///
/// Criterion needs `cargo bench` and a Rust toolchain; the `report` bin is a
/// second executable a bundle does not carry. `criterion_ran: false` is what
/// makes [`render`] emit ADR 0009's mandatory single-shot banner, so this is the
/// one place that pairing is decided rather than re-stated at each call site.
pub fn packaged_notes() -> Notes {
    Notes {
        criterion_note: "not run — the criterion layer needs `cargo bench` and a Rust \
                         toolchain, which a packaged build has neither of"
            .into(),
        criterion_rows: Vec::new(),
        criterion_ran: false,
        report_note: Some(
            "not run — the derived tables come from the `report` bin, a second executable \
             the bundle does not carry"
                .into(),
        ),
        report_table: Vec::new(),
    }
}

/// The whole metrics file, as a string. **Pure** — every input is a parameter.
///
/// LF line endings and no BOM, unlike the `.ps1`'s CRLF + PS 5.1 BOM: the file is
/// read by humans, by `git`, and occasionally diffed against another run.
pub fn render(env: &EnvFacts, records: &[Record], notes: &Notes) -> String {
    let mut out: Vec<String> = Vec::new();

    out.push("# scale-bench metrics - #22 / #155".into());
    out.push(String::new());
    out.push(format!(
        "Generated {} by `{}`.",
        env.generated, env.generator
    ));
    out.push(String::new());
    if !notes.criterion_ran {
        // ADR 0009 makes this mandatory: a single-shot run must never be
        // silently compared against a `cargo bench` one.
        out.push(
            "**Single-shot run — no criterion layer.** Latency figures are \
             order-of-magnitude and must not be compared against a `cargo bench` run. The \
             memory axes (peak RSS, working set) are unaffected."
                .into(),
        );
        out.push(String::new());
    }

    out.push("## Environment".into());
    out.push(String::new());
    out.push("| key | value |".into());
    out.push("|---|---|".into());
    out.push(format!("| CPU | {} |", env.cpu));
    out.push(format!("| threads | {} |", env.threads));
    out.push(format!("| RAM total | {} |", env.ram_total));
    out.push(format!(
        "| RAM available at start | {} |",
        env.ram_available
    ));
    out.push(format!("| OS | {} |", env.os));
    out.push(format!("| rustc | {} |", env.rustc));
    out.push(format!("| cargo | {} |", env.cargo));
    out.push(format!("| git | {} |", env.git));
    out.push(format!("| build | {} |", env.build));
    out.push(format!("| bases run | {} |", env.bases.join(", ")));
    out.push(format!("| real model | {} |", env.model));
    out.push(String::new());

    out.push(
        "## Load + memory (one process per row; peak RSS is that process's high-water mark)".into(),
    );
    out.push(String::new());
    out.push(
        "| basis | mode | nodes | edges | json MB | cache MB | wall ms \
         | peak RSS MB | end RSS MB | note |"
            .into(),
    );
    out.push("|---|---|--:|--:|--:|--:|--:|--:|--:|---|".into());
    for r in records
        .iter()
        .filter(|r| scenario::LOAD_MODES.contains(&r.mode()))
    {
        out.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.basis(),
            r.mode(),
            r.cell("nodes"),
            r.cell("edges"),
            r.cell("json_mb"),
            r.cell("cache_mb"),
            r.cell("wall_ms"),
            r.cell("peak_rss_mb"),
            r.cell("end_rss_mb"),
            note_for(r),
        ));
    }
    out.push(String::new());

    out.push("### Derived: where warm-load time goes (#155)".into());
    out.push(String::new());
    out.push(
        "`cache_hit` = mmap + validating access + **deserialize** + index build. \
         `access_checked` stops after validation; `access_unchecked` skips validation too. \
         A true zero-copy read-back removes the deserialize step but still needs the indices."
            .into(),
    );
    out.push(String::new());
    out.push(
        "| basis | from_slice ms | cache_hit ms | access_checked ms | access_unchecked ms \
         | validate ms | deserialize+index ms | cache_hit peak RSS MB | access peak RSS MB |"
            .into(),
    );
    out.push("|---|--:|--:|--:|--:|--:|--:|--:|--:|".into());
    for basis in bases_in_order(records) {
        let fs = find(records, basis, "from_slice");
        let ch = find(records, basis, "cache_hit");
        let ac = find(records, basis, "access_checked");
        let au = find(records, basis, "access_unchecked");
        out.push(format!(
            "| {basis} | {} | {} | {} | {} | {} | {} | {} | {} |",
            cell_of(fs, "wall_ms"),
            cell_of(ch, "wall_ms"),
            cell_of(ac, "wall_ms"),
            cell_of(au, "wall_ms"),
            delta(ac, au),
            delta(ch, ac),
            cell_of(ch, "peak_rss_mb"),
            cell_of(ac, "peak_rss_mb"),
        ));
    }
    out.push(String::new());

    out.push("## Scripted navigation (working set + query spread) (#22)".into());
    out.push(String::new());
    // `cone` is the uncapped legacy baseline (ADR 0003's fan-out finding);
    // `cone_with` is #244's capped rebuild on the same seed, so the cliff and the
    // level-of-detail answer to it read off one row.
    out.push(
        "| basis | load ms | RSS after load MB | peak RSS MB | scope_graph p50/p95 us \
         | expand p50/p95 us | path p95 us | source p95 us | cone ms | cone nodes \
         | cone_with ms | cone_with boxes | truncated | hot fanout |"
            .into(),
    );
    out.push("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|".into());
    for r in records.iter().filter(|r| r.mode() == "nav") {
        if !r.ok() {
            // The error takes the `load ms` column and the rest stay empty, so
            // the row keeps the table's arity.
            out.push(format!(
                "| {} | **FAILED** - {} | | | | | | | | | | | | |",
                r.basis(),
                r.error()
            ));
            continue;
        }
        out.push(format!(
            "| {} | {} | {} | {} | {} / {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.basis(),
            r.cell("load_ms"),
            r.cell("rss_after_load_mb"),
            r.cell("peak_rss_mb"),
            r.cell("scope_graph_p50_us"),
            r.cell("scope_graph_p95_us"),
            r.cell("expand_p50_us"),
            r.cell("expand_p95_us"),
            r.cell("nodes_at_path_p95_us"),
            r.cell("nodes_at_source_p95_us"),
            r.cell("cone_ms"),
            r.cell("cone_nodes"),
            r.cell("cone_with_ms"),
            r.cell("cone_with_nodes"),
            r.cell("cone_with_truncated"),
            r.cell("hot_fanout"),
        ));
    }
    out.push(String::new());

    out.push("## Matcher vs trace signal count (#22)".into());
    out.push(String::new());
    out.push("| basis | signals | wall ms | peak RSS MB | hit % | note |".into());
    out.push("|---|--:|--:|--:|--:|---|".into());
    for r in records.iter().filter(|r| r.mode() == "match") {
        out.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            r.basis(),
            r.cell("signals"),
            r.cell("wall_ms"),
            r.cell("peak_rss_mb"),
            r.cell("hit_rate_pct"),
            note_for(r),
        ));
    }
    out.push(String::new());

    out.push("## Criterion (statistical, 665 + 100K)".into());
    out.push(String::new());
    out.push(format!("_{}_", notes.criterion_note));
    out.push(String::new());
    if notes.criterion_rows.is_empty() {
        out.push("_no criterion estimates from this run_".into());
    } else {
        out.push("| bench | mean ms | median ms | std dev ms |".into());
        out.push("|---|--:|--:|--:|".into());
        let mut rows: Vec<&CriterionRow> = notes.criterion_rows.iter().collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        for row in rows {
            out.push(format!(
                "| {} | {:.3} | {:.3} | {:.3} |",
                row.id, row.mean_ms, row.median_ms, row.std_dev_ms
            ));
        }
    }
    out.push(String::new());

    out.push("## Single-shot report bin".into());
    out.push(String::new());
    match &notes.report_note {
        Some(note) => out.push(format!("_{note}_")),
        None => out.extend(notes.report_table.iter().cloned()),
    }
    out.push(String::new());

    out.push("## Raw scenario records".into());
    out.push(String::new());
    out.push("```json".into());
    // Verbatim, in collection order: these lines are the primary data and the
    // tables above are a reading of them.
    out.extend(records.iter().map(|r| r.raw.clone()));
    out.push("```".into());

    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn note_for(r: &Record) -> String {
    if r.ok() {
        String::new()
    } else {
        format!("**FAILED** - {}", r.error())
    }
}
