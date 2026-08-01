//! Single-shot scalability report: runs each axis once per size and prints a
//! markdown table to stdout, to seed the #24 report and the #22 (redb-vs-SQLite)
//! decision. Criterion (`cargo bench`) remains the statistical source of truth;
//! this is a paste-ready snapshot.
//!
//! `cargo run -p scale-bench --release -- report [--full]`
//! Default prints a real-fixture 665 row + a synthetic 100K row. `--full` adds
//! the 1M synthetic row (slow; opt-in only). Writes nothing to disk.

use std::path::PathBuf;
use std::time::Instant;

use scale_bench::{
    derive_handles, generate, signals_from_design, synth_signals, to_json, SynthConfig,
};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::{Design, Dir, Document};

const CONE_DEPTH: usize = 6;

/// Seconds-per-iteration for `f`, averaged over `iters`.
fn timed<F: FnMut()>(iters: u32, mut f: F) -> f64 {
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_secs_f64() / iters as f64
}

fn header() {
    println!("| basis | nodes | edges | json MB | from_slice ms | parse ms | index ms | scope_graph µs | expand µs | path µs | src µs | cone ms | clk fanout | match ms |");
    println!("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
}

fn golden_row() {
    // Embedded at compile time (#240) rather than read from a path built out of
    // CARGO_MANIFEST_DIR, so the same accessor serves a packaged build.
    let Some(bytes) = scale_bench::golden::golden_bytes() else {
        println!("| 665 (golden) | _fixture not found_ |");
        return;
    };
    let from_slice_ms = timed(20, || {
        std::hint::black_box(svxprobe_ingest::from_slice(&bytes).unwrap());
    }) * 1e3;
    let design = svxprobe_ingest::from_slice(&bytes).unwrap();
    let mb = bytes.len() as f64 / 1e6;
    println!(
        "| 665 (golden) | {} | {} | {:.2} | {:.3} | - | - | - | - | - | - | - | - | - |",
        design.nodes().len(),
        design.edges().len(),
        mb,
        from_slice_ms,
    );
}

fn synth_row(basis: &str, cfg: &SynthConfig) {
    let synth = generate(cfg);
    let json = to_json(&synth.doc);
    let nodes = synth.doc.nodes.len();
    let edges = synth.doc.edges.len();
    let mb = json.len() as f64 / 1e6;

    let from_slice_ms = timed(3, || {
        std::hint::black_box(svxprobe_ingest::from_slice(&json).unwrap());
    }) * 1e3;
    let parse_ms = timed(3, || {
        let d: svxprobe_model::Document = serde_json::from_slice(&json).unwrap();
        std::hint::black_box(d);
    }) * 1e3;
    let index_ms = timed(3, || {
        std::hint::black_box(Design::from_document(synth.doc.clone()));
    }) * 1e3;

    let design = Design::from_document(synth.doc.clone());
    let sg_us = timed(20, || {
        std::hint::black_box(svxprobe_schematic::scope_graph(&design, &synth.mid_scope));
    }) * 1e6;
    let ex_us = timed(20, || {
        std::hint::black_box(svxprobe_schematic::expand(&design, synth.mid_instance));
    }) * 1e6;
    let path_us = timed(1000, || {
        std::hint::black_box(design.nodes_at_path(&synth.probe_path));
    }) * 1e6;
    let (file, off) = synth.probe_source;
    let src_us = timed(1000, || {
        std::hint::black_box(design.nodes_at_source(file, off));
    }) * 1e6;
    let cone_ms = timed(5, || {
        // `Dir::In` traverses the clock star: the generator wires each flop as a
        // load of `clk`, and the legacy filter is side-blind.
        std::hint::black_box(svxprobe_schematic::cone(
            &design,
            synth.clock_net,
            Dir::In,
            CONE_DEPTH,
        ));
    }) * 1e3;

    let opts = MatchOptions {
        excluded_scopes: vec![],
        anchor: Some(vec!["top".to_string()]),
    };
    let signals = synth_signals(&synth, &design, cfg.signal_count);
    let match_ms = timed(3, || {
        let mut d = Design::from_document(synth.doc.clone());
        std::hint::black_box(run_match(&mut d, &signals, &opts));
    }) * 1e3;

    println!(
        "| {basis} | {nodes} | {edges} | {mb:.2} | {from_slice_ms:.3} | {parse_ms:.3} | {index_ms:.3} | {sg_us:.1} | {ex_us:.1} | {path_us:.3} | {src_us:.3} | {cone_ms:.3} | {} | {match_ms:.3} |",
        synth.clock_fanout,
    );
}

/// A real elaborated design (a second, realism-anchor basis alongside the
/// synthetic sizes). Loads the model JSON at `SCALE_BENCH_MODEL`; handles are
/// derived from the design itself. Intended for `claude_verilog_test`:
///   uv run svxprobe-elaborate --top <top> -f <files.f> -o model.json
///   SCALE_BENCH_MODEL=model.json cargo run -p scale-bench --release -- report
fn external_row() {
    let Some(var) = std::env::var_os("SCALE_BENCH_MODEL") else {
        eprintln!("(real-design row skipped — set SCALE_BENCH_MODEL=<elaborated hierarchy.json>)");
        return;
    };
    let path = PathBuf::from(var);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("SCALE_BENCH_MODEL '{}' unreadable: {e}", path.display());
            return;
        }
    };
    let design = match svxprobe_ingest::from_slice(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SCALE_BENCH_MODEL failed to load: {e:#}");
            return;
        }
    };
    let h = derive_handles(&design);
    let nodes = design.nodes().len();
    let edges = design.edges().len();
    let mb = bytes.len() as f64 / 1e6;

    let from_slice_ms = timed(5, || {
        std::hint::black_box(svxprobe_ingest::from_slice(&bytes).unwrap());
    }) * 1e3;
    let parse_ms = timed(5, || {
        let d: Document = serde_json::from_slice(&bytes).unwrap();
        std::hint::black_box(d);
    }) * 1e3;
    let doc: Document = serde_json::from_slice(&bytes).unwrap();
    let index_ms = timed(5, || {
        std::hint::black_box(Design::from_document(doc.clone()));
    }) * 1e3;
    let sg_us = timed(20, || {
        std::hint::black_box(svxprobe_schematic::scope_graph(&design, &h.mid_scope));
    }) * 1e6;
    let ex_us = timed(20, || {
        std::hint::black_box(svxprobe_schematic::expand(&design, h.mid_instance));
    }) * 1e6;
    let path_us = timed(1000, || {
        std::hint::black_box(design.nodes_at_path(&h.probe_path));
    }) * 1e6;
    let (file, off) = h.probe_source;
    let src_us = timed(1000, || {
        std::hint::black_box(design.nodes_at_source(file, off));
    }) * 1e6;
    let cone_ms = timed(5, || {
        std::hint::black_box(svxprobe_schematic::cone(
            &design,
            h.hot_net,
            Dir::Out,
            CONE_DEPTH,
        ));
    }) * 1e3;

    let opts = MatchOptions {
        excluded_scopes: vec![],
        anchor: Some(vec![design.doc.design.clone()]),
    };
    let signals = signals_from_design(&design, nodes.clamp(1, 20_000));
    let match_ms = timed(3, || {
        let mut d = Design::from_document(doc.clone());
        std::hint::black_box(run_match(&mut d, &signals, &opts));
    }) * 1e3;

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("real")
        .to_string();
    println!(
        "| {name} (real) | {nodes} | {edges} | {mb:.2} | {from_slice_ms:.3} | {parse_ms:.3} | {index_ms:.3} | {sg_us:.1} | {ex_us:.1} | {path_us:.3} | {src_us:.3} | {cone_ms:.3} | {} | {match_ms:.3} |",
        h.hot_fanout,
    );
}

fn main() {
    let full = std::env::args().any(|a| a == "--full");
    eprintln!("scale-bench report (single-shot; use `cargo bench` for statistics)");
    header();
    golden_row();
    external_row();
    synth_row("665 (synth)", &SynthConfig::baseline());
    synth_row("100K", &SynthConfig::mid());
    if full {
        synth_row("1M", &SynthConfig::large());
    } else {
        eprintln!("(1M row skipped — pass --full to include it)");
    }
}
