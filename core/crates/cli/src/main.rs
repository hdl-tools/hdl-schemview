//! `svxprobe` — the Phase 0 spike binary.
//!
//! Two subcommands prove the integration substrate end to end:
//!   * `ingest <model.json>` — deserialize the pyslang Node model, build indices,
//!     print a summary.
//!   * `wave <trace>`        — open a VCD/FST trace via wellen, print its
//!     hierarchy summary.
//!
//! Phase 1 grows this into the cross-probe matcher and hit-rate report.

use std::collections::BTreeMap;

use anyhow::Result;
use clap::{Parser, Subcommand};
use svxprobe_model::NodeKind;

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
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Ingest { model } => ingest(&model),
        Cmd::Wave { trace } => wave(&trace),
    }
}

fn ingest(path: &str) -> Result<()> {
    let design = svxprobe_ingest::from_path(path)?;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for n in design.nodes() {
        *by_kind.entry(format!("{:?}", n.kind)).or_default() += 1;
    }
    println!("design:       {}", design.doc.design);
    println!("files:        {}", design.doc.files.len());
    println!("nodes:        {}", design.nodes().len());
    println!("unique paths: {}", design.path_count());
    for (k, v) in &by_kind {
        println!("  {k:<10} {v}");
    }
    // Spot-check the source reverse index against the top instance.
    if let Some(top) = design.nodes().iter().find(|n| n.kind == NodeKind::Instance) {
        if let Some(r) = top.def_range {
            let hits = design.nodes_at_source(r.file, r.start.offset as usize);
            println!("src_index @ top def: {} node(s)", hits.len());
        }
    }
    Ok(())
}

fn wave(path: &str) -> Result<()> {
    let w = svxprobe_wave::LoadedWave::open(path)?;
    let s = w.summary();
    println!("trace:  {path}");
    println!("format: {}", s.file_format);
    println!("scopes: {}", s.scopes);
    println!("vars:   {}", s.vars);
    for name in w.var_full_names().iter().take(8) {
        println!("  {name}");
    }
    Ok(())
}
