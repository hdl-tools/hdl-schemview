//! `svxprobe` — the cross-probe spike binary.
//!
//! Subcommands:
//!   * `ingest <model.json>`        — deserialize the pyslang Node model, summarize.
//!   * `wave <trace>`               — open a VCD/FST trace, summarize its hierarchy.
//!   * `match <model.json> <trace>` — run the canonical-path matcher and print the
//!     hit-rate report; exits non-zero if the Phase 1 gate fails.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::Result;
use clap::{Parser, Subcommand};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::NodeKind;

/// Write a line to stdout, ignoring a broken pipe (e.g. when piped to `head`).
macro_rules! pln {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

#[derive(Parser)]
#[command(name = "svxprobe", about = "hdl-schemview cross-probe spike")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest a pyslang Node-model JSON file and summarize it.
    Ingest { model: String },
    /// Open a waveform (VCD/FST/GHW) and summarize its hierarchy.
    Wave { trace: String },
    /// Match a trace's signals to the elaborated design and report the hit-rate.
    Match {
        model: String,
        trace: String,
        /// Gate threshold (fraction of design-scope signals that must match).
        #[arg(long, default_value_t = 0.95)]
        threshold: f64,
        /// File of excluded scope names (one per line; `#`/`//` comments ok).
        #[arg(long)]
        excluded: Option<String>,
        /// Force the DUT anchor (dot-separated) instead of auto-detecting.
        #[arg(long)]
        anchor: Option<String>,
        /// List unmatched signals (with the candidate path that was tried).
        #[arg(long)]
        show_unmatched: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Ingest { model } => ingest(&model),
        Cmd::Wave { trace } => wave(&trace),
        Cmd::Match {
            model,
            trace,
            threshold,
            excluded,
            anchor,
            show_unmatched,
        } => match_cmd(&model, &trace, threshold, excluded, anchor, show_unmatched),
    }
}

fn ingest(path: &str) -> Result<()> {
    let design = svxprobe_ingest::from_path(path)?;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for n in design.nodes() {
        *by_kind.entry(format!("{:?}", n.kind)).or_default() += 1;
    }
    pln!("design:       {}", design.doc.design);
    pln!("files:        {}", design.doc.files.len());
    pln!("nodes:        {}", design.nodes().len());
    pln!("unique paths: {}", design.path_count());
    for (k, v) in &by_kind {
        pln!("  {k:<10} {v}");
    }
    if let Some(top) = design.nodes().iter().find(|n| n.kind == NodeKind::Instance) {
        if let Some(r) = top.def_range {
            let hits = design.nodes_at_source(r.file, r.start.offset as usize);
            pln!("src_index @ top def: {} node(s)", hits.len());
        }
    }
    Ok(())
}

fn wave(path: &str) -> Result<()> {
    let w = svxprobe_wave::LoadedWave::open(path)?;
    let s = w.summary();
    pln!("trace:  {path}");
    pln!("format: {}", s.file_format);
    pln!("scopes: {}", s.scopes);
    pln!("vars:   {}", s.vars);
    for name in w.var_full_names().iter().take(8) {
        pln!("  {name}");
    }
    Ok(())
}

fn match_cmd(
    model: &str,
    trace: &str,
    threshold: f64,
    excluded: Option<String>,
    anchor: Option<String>,
    show_unmatched: bool,
) -> Result<()> {
    let mut design = svxprobe_ingest::from_path(model)?;
    let wave = svxprobe_wave::LoadedWave::open(trace)?;
    let signals = wave.signals();

    let opts = MatchOptions {
        excluded_scopes: match &excluded {
            Some(p) => read_excluded(p)?,
            None => Vec::new(),
        },
        anchor: anchor.map(|a| a.split('.').map(str::to_string).collect()),
    };

    let report = run_match(&mut design, &signals, &opts);

    pln!("trace:       {trace}");
    pln!("anchor:      {}", report.anchor.join("."));
    pln!("signals:     {}", report.total_signals);
    pln!(
        "denominator: {} (design-scope, non-parameter)",
        report.denominator
    );
    pln!(
        "matched:     {} ({:.2}%)  [direct {} + interface-alias {}]",
        report.matched(),
        report.hit_rate() * 100.0,
        report.matched_direct,
        report.matched_alias,
    );
    pln!("unmatched:   {}", report.unmatched);
    pln!(
        "excluded:    {} scope + {} parameter",
        report.excluded_scope,
        report.excluded_parameter,
    );
    pln!("mystery:     {}", report.mystery);
    pln!("wave_index:  {} signals linked", design.wave_index.len());
    if !report.rule_counts.is_empty() {
        pln!("rules applied:");
        for (rule, count) in &report.rule_counts {
            pln!("  {rule:?}: {count}");
        }
    }
    if show_unmatched {
        for u in &report.unmatched_samples {
            pln!("  UNMATCHED {} -> {}", u.full_name, u.candidate);
        }
    }
    for m in &report.mystery_samples {
        pln!("  MYSTERY {m}");
    }

    let pass = report.passes_gate(threshold);
    pln!(
        "gate (>= {:.0}% & 0 mystery): {}",
        threshold * 100.0,
        if pass { "PASS" } else { "FAIL" },
    );
    if !pass {
        std::process::exit(1);
    }
    Ok(())
}

/// Parse an excluded-scopes file: one scope name per line; `#`/`//` comments and
/// blank lines ignored.
fn read_excluded(path: &str) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(|l| {
            l.split('#')
                .next()
                .unwrap_or("")
                .split("//")
                .next()
                .unwrap_or("")
                .trim()
        })
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}
