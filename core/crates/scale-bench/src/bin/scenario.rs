//! One measured scenario per process, for the #22 / #155 scalability report.
//!
//! Criterion (`cargo bench`) stays the statistical source of truth for *latency*
//! at sizes it can afford to repeat. This bin covers what criterion cannot:
//!
//! * **Peak RSS** — attributable only if the process does exactly one thing, so
//!   each invocation runs a single mode at a single basis and exits. This is the
//!   axis #22 turns on ("too large to materialize") and #155's real payoff.
//! * **The 1M point** — one shot instead of ~100 criterion samples.
//! * **`access` vs `deserialize`** — `access_checked`/`access_unchecked` price
//!   the rkyv steps a true zero-copy read-back (#155) would remove or keep.
//! * **Scripted navigation** — sustained working set + per-op latency spread
//!   over many scopes, not one op on one scope (#24's "working-set RSS under a
//!   scripted navigation").
//!
//! ```text
//! scenario --basis <665|100K|1M|golden|real> --mode <mode> [--signals N]
//!          [--nav-scopes N] [--nav-iters N]
//! ```
//!
//! Modes: `prepare` (materialize the JSON + build the rkyv cache — run first),
//! `from_slice` (cold: parse + validate + index), `cache_hit` (warm: mmap +
//! access + deserialize + index), `access_checked`, `access_unchecked`, `nav`,
//! `match`.
//!
//! Emits one JSON object on stdout (the driver script concatenates them);
//! progress and errors go to stderr. `real` reads `SCALE_BENCH_MODEL`.

use std::path::PathBuf;
use std::time::Instant;

use scale_bench::{
    derive_handles, generate, mem, signals_from_design, to_json, RealHandles, SynthConfig,
};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::{Design, Dir, NodeId, NodeKind};

const CONE_DEPTH: usize = 6;

// --- argument parsing -------------------------------------------------------

struct Args {
    basis: String,
    mode: String,
    signals: Option<usize>,
    nav_scopes: usize,
    nav_iters: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut basis = None;
    let mut mode = None;
    let mut signals = None;
    let mut nav_scopes = 32usize;
    let mut nav_iters = 3usize;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--basis" => basis = Some(it.next().ok_or("--basis needs a value")?),
            "--mode" => mode = Some(it.next().ok_or("--mode needs a value")?),
            "--signals" => {
                signals = Some(
                    it.next()
                        .ok_or("--signals needs a value")?
                        .parse()
                        .map_err(|e| format!("--signals: {e}"))?,
                )
            }
            "--nav-scopes" => {
                nav_scopes = it
                    .next()
                    .ok_or("--nav-scopes needs a value")?
                    .parse()
                    .map_err(|e| format!("--nav-scopes: {e}"))?
            }
            "--nav-iters" => {
                nav_iters = it
                    .next()
                    .ok_or("--nav-iters needs a value")?
                    .parse()
                    .map_err(|e| format!("--nav-iters: {e}"))?
            }
            "-h" | "--help" => return Err("help requested".into()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    Ok(Args {
        basis: basis.ok_or("--basis is required")?,
        mode: mode.ok_or("--mode is required")?,
        signals,
        nav_scopes: nav_scopes.max(1),
        nav_iters: nav_iters.max(1),
    })
}

// --- result record ----------------------------------------------------------

/// A flat key/value record printed as one JSON object. Flat on purpose: the
/// driver script pastes these into a markdown table without walking a tree.
#[derive(Default)]
struct Record {
    fields: Vec<(String, String)>,
}

impl Record {
    fn str(&mut self, key: &str, value: &str) -> &mut Self {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        self.fields.push((key.into(), format!("\"{escaped}\"")));
        self
    }

    fn num(&mut self, key: &str, value: f64) -> &mut Self {
        // JSON has no NaN/Infinity: emit null rather than a document the driver
        // cannot parse.
        let rendered = if value.is_finite() {
            format!("{value:.4}")
        } else {
            "null".into()
        };
        self.fields.push((key.into(), rendered));
        self
    }

    fn int(&mut self, key: &str, value: u64) -> &mut Self {
        self.fields.push((key.into(), value.to_string()));
        self
    }

    fn opt_mb(&mut self, key: &str, bytes: Option<u64>) -> &mut Self {
        match bytes {
            Some(b) => self.num(key, mem::mb(b)),
            None => {
                self.fields.push((key.into(), "null".into()));
                self
            }
        }
    }

    fn emit(&self) {
        let body = self
            .fields
            .iter()
            .map(|(k, v)| format!("\"{k}\":{v}"))
            .collect::<Vec<_>>()
            .join(",");
        println!("{{{body}}}");
    }
}

/// Every record carries the run's identity plus its memory high-water mark, so a
/// row is self-describing even when pasted out of context.
fn finish(mut rec: Record, args: &Args, wall_s: f64) {
    rec.num("wall_ms", wall_s * 1e3);
    rec.opt_mb("peak_rss_mb", mem::peak_rss_bytes());
    rec.opt_mb("end_rss_mb", mem::current_rss_bytes());
    rec.str("basis", &args.basis);
    rec.str("mode", &args.mode);
    rec.emit();
}

// --- basis materialization --------------------------------------------------

/// Per-basis scratch dir. The model JSON is **copied** here rather than read in
/// place, because `from_path` writes `.schemview_data/` beside the model — a
/// committed fixture (or the user's own design dir) must not grow a cache dir as
/// a side effect of benchmarking.
fn workdir(basis: &str) -> PathBuf {
    std::env::temp_dir()
        .join("scale_bench_scenarios")
        .join(basis)
}

fn model_path(basis: &str) -> PathBuf {
    workdir(basis).join("hierarchy.json")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc/golden/hierarchy.json")
}

fn synth_config(basis: &str) -> Option<SynthConfig> {
    match basis {
        "665" => Some(SynthConfig::baseline()),
        "100K" => Some(SynthConfig::mid()),
        "1M" => Some(SynthConfig::large()),
        _ => None,
    }
}

/// Produce the model JSON bytes for a basis: generated for a synthetic size,
/// read from disk for `golden` / `real`.
fn source_bytes(basis: &str) -> Result<Vec<u8>, String> {
    if let Some(cfg) = synth_config(basis) {
        eprintln!(
            "generating synthetic basis {basis} (~{} nodes)",
            cfg.node_count
        );
        return Ok(to_json(&generate(&cfg).doc));
    }
    let src = match basis {
        "golden" => golden_path(),
        "real" => std::env::var_os("SCALE_BENCH_MODEL")
            .map(PathBuf::from)
            .ok_or("basis 'real' needs SCALE_BENCH_MODEL=<elaborated hierarchy.json>")?,
        other => return Err(format!("unknown basis '{other}'")),
    };
    std::fs::read(&src).map_err(|e| format!("reading {}: {e}", src.display()))
}

/// `prepare`: materialize the JSON and build the rkyv cache, timing the cache
/// build — the Phase-A analogue of #24's "DB build / ingest time" axis, and the
/// cost any Phase-B store would have to beat.
fn run_prepare(args: &Args) -> Result<(), String> {
    let dir = workdir(&args.basis);
    let model = model_path(&args.basis);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    // Drop a stale cache first: `build_cache` would rewrite it anyway, but a
    // leftover archive from an older RKYV_FORMAT_VERSION makes the timing lie.
    let _ = std::fs::remove_dir_all(dir.join(".schemview_data"));

    let bytes = source_bytes(&args.basis)?;
    std::fs::write(&model, &bytes).map_err(|e| format!("writing {}: {e}", model.display()))?;
    // Release the JSON before `build_cache` re-reads it: at 1M this buffer is
    // ~600 MB, and holding it across the cache build would stack two copies of
    // the model in RAM for no reason.
    let json_mb = bytes.len() as f64 / 1e6;
    drop(bytes);

    let t = Instant::now();
    let cache = svxprobe_ingest::build_cache(&model).map_err(|e| format!("build_cache: {e:#}"))?;
    let wall = t.elapsed().as_secs_f64();

    // Counts off the archive rather than a second full parse — same numbers, no
    // extra `Document`.
    let (nodes, edges) = svxprobe_ingest::access_cache_checked(&model)
        .ok_or("cache built but not readable back — version or staleness mismatch")?;
    let cache_bytes = std::fs::metadata(&cache).map(|m| m.len()).unwrap_or(0);

    let mut rec = Record::default();
    rec.int("nodes", nodes as u64)
        .int("edges", edges as u64)
        .num("json_mb", json_mb)
        .num("cache_mb", cache_bytes as f64 / 1e6)
        .str("model", &model.display().to_string());
    finish(rec, args, wall);
    Ok(())
}

/// Load the model JSON a measured mode operates on; `prepare` must have run.
fn prepared_bytes(basis: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let model = model_path(basis);
    let bytes = std::fs::read(&model).map_err(|e| {
        format!(
            "reading {} ({e}) — run `--basis {basis} --mode prepare` first",
            model.display()
        )
    })?;
    Ok((model, bytes))
}

// --- measured modes ---------------------------------------------------------

/// Cold path: JSON parse + referential-integrity validation + index build.
fn run_from_slice(args: &Args) -> Result<(), String> {
    let (_, bytes) = prepared_bytes(&args.basis)?;
    let t = Instant::now();
    let design = svxprobe_ingest::from_slice(&bytes).map_err(|e| format!("from_slice: {e:#}"))?;
    let wall = t.elapsed().as_secs_f64();

    let mut rec = Record::default();
    rec.int("nodes", design.nodes().len() as u64)
        .int("edges", design.edges().len() as u64)
        .num("json_mb", bytes.len() as f64 / 1e6);
    finish(rec, args, wall);
    // Keep the design alive across the RSS read above; dropping it earlier would
    // still show the peak (it is monotonic) but not the end-of-run resident set.
    drop(design);
    Ok(())
}

/// Warm path (#21): mmap + validating access + deserialize + index build.
fn run_cache_hit(args: &Args) -> Result<(), String> {
    let (model, bytes) = prepared_bytes(&args.basis)?;
    // A miss here would silently measure the JSON path instead; refuse rather
    // than report a mislabelled number.
    if svxprobe_ingest::access_cache_checked(&model).is_none() {
        return Err(format!(
            "no fresh rkyv cache beside {} — run `--mode prepare` first",
            model.display()
        ));
    }
    let t = Instant::now();
    let design = svxprobe_ingest::from_path(&model).map_err(|e| format!("from_path: {e:#}"))?;
    let wall = t.elapsed().as_secs_f64();

    let mut rec = Record::default();
    rec.int("nodes", design.nodes().len() as u64)
        .int("edges", design.edges().len() as u64)
        .num("json_mb", bytes.len() as f64 / 1e6);
    finish(rec, args, wall);
    drop(design);
    Ok(())
}

/// #155 probe: mmap + bytecheck validation + header check, no deserialize.
fn run_access(args: &Args, checked: bool) -> Result<(), String> {
    let (model, _) = prepared_bytes(&args.basis)?;
    let t = Instant::now();
    let counts = if checked {
        svxprobe_ingest::access_cache_checked(&model)
    } else {
        // SAFETY: `prepare` wrote this archive with the running build moments
        // ago, in a scratch dir this process owns, and nothing else touches it.
        unsafe { svxprobe_ingest::access_cache_unchecked(&model) }
    };
    let wall = t.elapsed().as_secs_f64();
    let (nodes, edges) = counts.ok_or_else(|| {
        format!(
            "no fresh rkyv cache beside {} — run `--mode prepare` first",
            model.display()
        )
    })?;

    let mut rec = Record::default();
    rec.int("nodes", nodes as u64).int("edges", edges as u64);
    finish(rec, args, wall);
    Ok(())
}

/// Instance nodes spread evenly across the hierarchy — a stand-in for a user
/// clicking through the tree, rather than hammering one hot scope.
fn nav_targets(design: &Design, count: usize) -> Vec<(String, NodeId)> {
    let instances: Vec<(String, NodeId)> = design
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Instance)
        .map(|n| (n.path.clone(), n.id))
        .collect();
    if instances.is_empty() {
        return Vec::new();
    }
    let stride = (instances.len() / count).max(1);
    instances.into_iter().step_by(stride).take(count).collect()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn summarize(rec: &mut Record, name: &str, mut samples: Vec<f64>) {
    if samples.is_empty() {
        rec.num(&format!("{name}_mean_us"), f64::NAN);
        return;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let max = *samples.last().unwrap_or(&f64::NAN);
    rec.num(&format!("{name}_mean_us"), mean)
        .num(&format!("{name}_p50_us"), percentile(&samples, 0.50))
        .num(&format!("{name}_p95_us"), percentile(&samples, 0.95))
        .num(&format!("{name}_max_us"), max);
}

/// Scripted navigation: load once, then walk many scopes the way the GUI would
/// (tree click → schematic → drill → source probe), plus one high-fan-out cone.
/// Reports the latency spread *and* the sustained working set — the pair #22
/// needs, since a fast query that only works because everything is resident is
/// exactly what Phase B would change.
fn run_nav(args: &Args) -> Result<(), String> {
    let (model, _) = prepared_bytes(&args.basis)?;
    let t_load = Instant::now();
    let design = svxprobe_ingest::from_path(&model).map_err(|e| format!("from_path: {e:#}"))?;
    let load_ms = t_load.elapsed().as_secs_f64() * 1e3;
    let rss_after_load = mem::current_rss_bytes();

    let targets = nav_targets(&design, args.nav_scopes);
    if targets.is_empty() {
        return Err("no Instance nodes to navigate".into());
    }
    let handles: RealHandles = derive_handles(&design);

    let mut scope_us = Vec::new();
    let mut expand_us = Vec::new();
    let mut path_us = Vec::new();
    let mut src_us = Vec::new();

    let t = Instant::now();
    for _ in 0..args.nav_iters {
        for (path, id) in &targets {
            let t0 = Instant::now();
            std::hint::black_box(svxprobe_schematic::scope_graph(&design, path));
            scope_us.push(t0.elapsed().as_secs_f64() * 1e6);

            let t0 = Instant::now();
            std::hint::black_box(svxprobe_schematic::expand(&design, *id));
            expand_us.push(t0.elapsed().as_secs_f64() * 1e6);

            let t0 = Instant::now();
            std::hint::black_box(design.nodes_at_path(path));
            path_us.push(t0.elapsed().as_secs_f64() * 1e6);

            let (file, off) = handles.probe_source;
            let t0 = Instant::now();
            std::hint::black_box(design.nodes_at_source(file, off));
            src_us.push(t0.elapsed().as_secs_f64() * 1e6);
        }
    }
    let t_cone = Instant::now();
    let cone = svxprobe_schematic::cone(&design, handles.hot_net, Dir::Out, CONE_DEPTH);
    let cone_ms = t_cone.elapsed().as_secs_f64() * 1e3;
    let wall = t.elapsed().as_secs_f64();

    let mut rec = Record::default();
    rec.int("nodes", design.nodes().len() as u64)
        .int("edges", design.edges().len() as u64)
        .num("load_ms", load_ms)
        .opt_mb("rss_after_load_mb", rss_after_load)
        .int("nav_scopes", targets.len() as u64)
        .int("nav_iters", args.nav_iters as u64)
        .num("cone_ms", cone_ms)
        .int("cone_nodes", cone.nodes.len() as u64)
        .int("hot_fanout", handles.hot_fanout as u64);
    summarize(&mut rec, "scope_graph", scope_us);
    summarize(&mut rec, "expand", expand_us);
    summarize(&mut rec, "nodes_at_path", path_us);
    summarize(&mut rec, "nodes_at_source", src_us);
    finish(rec, args, wall);
    drop(design);
    Ok(())
}

/// Matcher wall-time + peak RSS vs **trace signal count** (#24's axis). Signals
/// are synthesized from the design's own paths, so the hit rate is ~100% and the
/// number reflects the join, not a miss-handling path.
fn run_match_mode(args: &Args) -> Result<(), String> {
    let (model, _) = prepared_bytes(&args.basis)?;
    let mut design = svxprobe_ingest::from_path(&model).map_err(|e| format!("from_path: {e:#}"))?;
    let want = args.signals.unwrap_or(20_000);
    let signals = signals_from_design(&design, want);
    let opts = MatchOptions {
        excluded_scopes: vec![],
        anchor: Some(vec![design.doc.design.clone()]),
    };

    let t = Instant::now();
    let report = run_match(&mut design, &signals, &opts);
    let wall = t.elapsed().as_secs_f64();

    let matched = report.matched_direct + report.matched_alias;
    let hit_rate = if report.denominator == 0 {
        f64::NAN
    } else {
        100.0 * matched as f64 / report.denominator as f64
    };
    let mut rec = Record::default();
    rec.int("nodes", design.nodes().len() as u64)
        .int("signals_requested", want as u64)
        .int("signals", signals.len() as u64)
        .int("matched", matched as u64)
        .int("unmatched", report.unmatched as u64)
        .num("hit_rate_pct", hit_rate);
    finish(rec, args, wall);
    Ok(())
}

// --- entry point ------------------------------------------------------------

const USAGE: &str = "usage: scenario --basis <665|100K|1M|golden|real> \
--mode <prepare|from_slice|cache_hit|access_checked|access_unchecked|nav|match> \
[--signals N] [--nav-scopes N] [--nav-iters N]";

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    eprintln!("scenario: basis={} mode={}", args.basis, args.mode);

    let result = match args.mode.as_str() {
        "prepare" => run_prepare(&args),
        "from_slice" => run_from_slice(&args),
        "cache_hit" => run_cache_hit(&args),
        "access_checked" => run_access(&args, true),
        "access_unchecked" => run_access(&args, false),
        "nav" => run_nav(&args),
        "match" => run_match_mode(&args),
        other => Err(format!("unknown mode '{other}'\n{USAGE}")),
    };

    if let Err(e) = result {
        // Stderr + a non-zero exit, so the driver records the failure as a row
        // instead of silently dropping the scenario. A 1M OOM *is* a result.
        eprintln!("scenario failed: {e}");
        std::process::exit(1);
    }
}
