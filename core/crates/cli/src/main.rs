//! `svxprobe` — the cross-probe spike binary.
//!
//! Subcommands:
//!   * `ingest <model.json>`        — deserialize the pyslang Node model, summarize.
//!   * `wave <trace>`               — open a VCD/FST trace, summarize its hierarchy.
//!   * `match <model.json> <trace>` — run the canonical-path matcher and print the
//!     hit-rate report; exits non-zero if the Phase 1 gate fails.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::{Design, NodeKind};
use svxprobe_xprobe::{CrossProbe, Resolution, WaveTarget};

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
    /// Pre-build the rkyv load cache (#21) beside a model JSON, without loading it.
    Cache { model: String },
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
    /// Extract a schematic graph for a scope (or a net's fan-in/out cone).
    Graph {
        model: String,
        /// Scope canonical path (e.g. picorv32_soc.g_lane[0]).
        scope: String,
        /// Instead of the scope graph, extract the cone of this net path.
        #[arg(long)]
        cone: Option<String>,
        /// Cone direction.
        #[arg(long, default_value = "inout")]
        dir: String,
        /// Cone depth.
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Use the rebuilt cone extractor (#244) at this projection
        /// (process-level | gate-level). Selecting either this or --fanout
        /// switches away from the legacy extractor, whose output is kept as a
        /// stable contract.
        #[arg(long)]
        projection: Option<String>,
        /// Max boxes reached through one signal per hop; implies the rebuilt
        /// extractor. Truncation is reported, never silent.
        #[arg(long)]
        fanout: Option<usize>,
        /// Emit the graph as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cross-probe: resolve one view's selection and show the other two.
    Probe {
        model: String,
        trace: String,
        #[arg(long)]
        excluded: Option<String>,
        /// A waveform signal's full (trace) name.
        #[arg(long, group = "target")]
        signal: Option<String>,
        /// A node's canonical path (e.g. picorv32_soc.g_lane[0].bus.valid).
        #[arg(long, group = "target")]
        node: Option<String>,
        /// A source position FILE:LINE[:COL] (FILE matched against the model's files).
        #[arg(long, group = "target")]
        source: Option<String>,
        /// A source position FILE:OFFSET (byte offset; FILE = path or numeric id).
        #[arg(long, group = "target")]
        offset: Option<String>,
        /// Active hierarchy scope (canonical path) to disambiguate a source click.
        #[arg(long)]
        context: Option<String>,
        /// Root directory for resolving source files read by --source.
        #[arg(long, default_value = ".")]
        src_root: String,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Ingest { model } => ingest(&model),
        Cmd::Cache { model } => cache_cmd(&model),
        Cmd::Wave { trace } => wave(&trace),
        Cmd::Match {
            model,
            trace,
            threshold,
            excluded,
            anchor,
            show_unmatched,
        } => match_cmd(&model, &trace, threshold, excluded, anchor, show_unmatched),
        Cmd::Graph {
            model,
            scope,
            cone,
            projection,
            fanout,
            dir,
            depth,
            json,
        } => graph_cmd(GraphArgs {
            model,
            scope,
            cone,
            dir,
            depth,
            projection,
            fanout,
            json,
        }),
        Cmd::Probe {
            model,
            trace,
            excluded,
            signal,
            node,
            source,
            offset,
            context,
            src_root,
        } => probe_cmd(ProbeArgs {
            model,
            trace,
            excluded,
            signal,
            node,
            source,
            offset,
            context,
            src_root,
        }),
    }
}

fn cache_cmd(path: &str) -> Result<()> {
    let cache = svxprobe_ingest::build_cache(path)?;
    pln!("wrote cache: {}", cache.display());
    Ok(())
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

/// Arguments for `graph`, grouped like [`ProbeArgs`] so the subcommand can grow
/// flags without tripping the argument-count lint.
struct GraphArgs {
    model: String,
    scope: String,
    cone: Option<String>,
    dir: String,
    depth: usize,
    projection: Option<String>,
    fanout: Option<usize>,
    json: bool,
}

fn graph_cmd(args: GraphArgs) -> Result<()> {
    use svxprobe_model::Dir;
    let GraphArgs {
        model,
        scope,
        cone,
        dir,
        depth,
        projection,
        fanout,
        json,
    } = args;
    let (model, scope, dir) = (model.as_str(), scope.as_str(), dir.as_str());
    let design = svxprobe_ingest::from_path(model)?;

    let graph = if let Some(net) = cone {
        let start = *design
            .nodes_at_path(&net)
            .first()
            .with_context(|| format!("net path not found: {net}"))?;
        let d = match dir {
            "in" => Dir::In,
            "out" => Dir::Out,
            _ => Dir::Inout,
        };
        // Either new flag opts into the rebuilt extractor (#244). Absent both,
        // the legacy one runs untouched: it is this command's output contract
        // and the scale-bench fan-out baseline ADR 0003 is measured against.
        if projection.is_some() || fanout.is_some() {
            let proj = match projection.as_deref() {
                Some("gate-level") => svxprobe_schematic::Projection::GateLevel,
                Some("process-level") | None => svxprobe_schematic::Projection::ProcessLevel,
                Some(other) => {
                    anyhow::bail!("unknown projection {other:?} (process-level | gate-level)")
                }
            };
            let mut limits = svxprobe_schematic::ConeLimits::depth(depth);
            if let Some(f) = fanout {
                limits.fanout = f;
            }
            svxprobe_schematic::cone_with(&design, start, d, limits, proj)
        } else {
            svxprobe_schematic::cone(&design, start, d, depth)
        }
    } else {
        svxprobe_schematic::scope_graph(&design, scope)
            .with_context(|| format!("scope not found: {scope}"))?
    };

    if json {
        pln!("{}", serde_json::to_string_pretty(&graph)?);
        return Ok(());
    }

    pln!("scope:  {}", graph.root);
    pln!("boxes:  {}", graph.nodes.len());
    for n in &graph.nodes {
        pln!(
            "  [{}] {} ({:?}, {} ports){}",
            n.id,
            n.label,
            n.kind,
            n.ports.len(),
            if n.expandable { " +" } else { "" },
        );
    }
    pln!("wires:  {}", graph.edges.len());
    for e in &graph.edges {
        let p = |id| {
            design
                .node(id)
                .map(|n: &svxprobe_model::Node| n.path.as_str())
                .unwrap_or("?")
        };
        pln!("  {} -> {}", p(e.source), p(e.target));
    }
    Ok(())
}

struct ProbeArgs {
    model: String,
    trace: String,
    excluded: Option<String>,
    signal: Option<String>,
    node: Option<String>,
    source: Option<String>,
    offset: Option<String>,
    context: Option<String>,
    src_root: String,
}

fn probe_cmd(a: ProbeArgs) -> Result<()> {
    let design = svxprobe_ingest::from_path(&a.model)?;
    let wave = svxprobe_wave::LoadedWave::open(&a.trace)?;
    let opts = MatchOptions {
        excluded_scopes: match &a.excluded {
            Some(p) => read_excluded(p)?,
            None => Vec::new(),
        },
        anchor: None,
    };
    let mut cp = CrossProbe::build(design, &wave, &opts);

    if let Some(ctx) = &a.context {
        match cp.design().nodes_at_path(ctx).first().copied() {
            Some(id) => cp.set_context(id),
            None => pln!("warning: context path not found: {ctx}"),
        }
    }

    let resolution = if let Some(s) = &a.signal {
        cp.from_signal(s)
    } else if let Some(p) = &a.node {
        cp.from_node_path(p)
    } else if let Some(spec) = &a.source {
        let (file, off) = resolve_source(cp.design(), spec, &a.src_root)?;
        cp.from_source(file, off)
    } else if let Some(spec) = &a.offset {
        let (file, off) = parse_offset(cp.design(), spec)?;
        cp.from_source(file, off)
    } else {
        anyhow::bail!("give one of --signal / --node / --source / --offset");
    };

    match resolution {
        Some(r) => {
            print_resolution(&cp, &r);
            Ok(())
        }
        None => {
            pln!("no cross-probe target found");
            std::process::exit(1);
        }
    }
}

fn print_resolution(cp: &CrossProbe, r: &Resolution) {
    let d = cp.design();
    let anchor = &r.selection;
    let node = d.node(anchor.anchor);
    pln!(
        "selection: {}  [{:?}]",
        node.map(|n| n.path.as_str()).unwrap_or("?"),
        node.map(|n| n.kind),
    );

    // Source projection.
    let src = cp.to_source(anchor);
    match src.def.or(src.inst) {
        Some(rng) => {
            let file = d
                .doc
                .files
                .iter()
                .find(|f| f.id == rng.file)
                .map(|f| f.path.as_str())
                .unwrap_or("?");
            pln!(
                "  source:  {}:{}:{}  (offset {})",
                file,
                rng.start.line,
                rng.start.col,
                rng.start.offset,
            );
        }
        None => pln!("  source:  (no range)"),
    }

    // Waveform projection.
    match cp.to_wave(anchor) {
        WaveTarget::Linked { full_name, .. } => pln!("  wave:    {full_name}"),
        WaveTarget::NotInTrace => pln!("  wave:    (not in trace)"),
    }

    // Picker.
    if !r.alternatives.is_empty() {
        pln!("alternatives (picker):");
        for &alt in &r.alternatives {
            pln!(
                "  - {}",
                d.node(alt).map(|n| n.path.as_str()).unwrap_or("?")
            );
        }
    }
}

/// Resolve a `FILE:LINE[:COL]` spec to (file id, byte offset), reading the file
/// from `src_root` to convert line:col.
fn resolve_source(design: &Design, spec: &str, src_root: &str) -> Result<(u32, usize)> {
    let parts: Vec<&str> = spec.split(':').collect();
    anyhow::ensure!(parts.len() >= 2, "expected FILE:LINE[:COL], got {spec}");
    let (id, path) = file_entry(design, parts[0])?;
    let line: usize = parts[1].parse().context("LINE not a number")?;
    let col: usize = parts.get(2).map(|c| c.parse()).transpose()?.unwrap_or(1);

    let full = Path::new(src_root).join(&path);
    let text = std::fs::read_to_string(&full)
        .with_context(|| format!("reading source {} (try --src-root)", full.display()))?;
    let mut offset = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            return Ok((id, offset + col.saturating_sub(1)));
        }
        offset += l.len();
    }
    anyhow::bail!("line {line} past end of {}", full.display())
}

/// Parse a `FILE:OFFSET` spec (FILE = path or numeric id).
fn parse_offset(design: &Design, spec: &str) -> Result<(u32, usize)> {
    let (file, off) = spec.rsplit_once(':').context("expected FILE:OFFSET")?;
    let off: usize = off.parse().context("OFFSET not a number")?;
    let id = match file.parse::<u32>() {
        Ok(id) => id,
        Err(_) => file_entry(design, file)?.0,
    };
    Ok((id, off))
}

/// Find a file in the model by exact path, basename, or suffix.
fn file_entry(design: &Design, name: &str) -> Result<(u32, String)> {
    design
        .doc
        .files
        .iter()
        .find(|f| {
            f.path == name
                || f.path.ends_with(&format!("/{name}"))
                || Path::new(&f.path)
                    .file_name()
                    .map(|b| b == name)
                    .unwrap_or(false)
        })
        .map(|f| (f.id, f.path.clone()))
        .with_context(|| format!("no source file matching '{name}' in the model"))
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
