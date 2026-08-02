//! Elaborate a designlist into the `real` basis (#255).
//!
//! The matrix has always accepted an already-elaborated `-model
//! <hierarchy.json>`; producing that JSON was a manual step in
//! `docs/benchmarking.md`, and it had to be run with `--gate-level --name-refs`
//! or the run would silently measure a *smaller* document than the tool
//! actually loads. Nothing enforced that. This module closes the gap by
//! elaborating through [`svxprobe_gui::elaborate_to_file`] — the same argv
//! builder the desktop app's own designlist load uses, so the two cannot drift.
//!
//! Deliberately shared by both entry points (the dev `collect` bin and the
//! packaged app's `--bench`): the scratch path, the timing, the printed hint
//! and the provenance row must be identical in both, or two metrics files are
//! not comparable. It lives beside [`crate::collect`] rather than inside it
//! because that module's header declares `render` pure, and this one spawns a
//! process and touches the filesystem.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Scratch directory for the elaborated model, mirroring
/// [`crate::scenario::workdir`]'s `scale_bench_scenarios`.
///
/// Separate from the scenario workdirs because `prepare` *copies* the model
/// into its own basis dir — this one holds the original, so a second run can
/// reuse it with `-model` instead of re-elaborating.
pub fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join("scale_bench_elaborate")
}

/// What a designlist elaboration produced, and what the metrics file says
/// about it.
#[derive(Debug, Clone)]
pub struct Elaboration {
    /// The written `hierarchy.json` — hand this to `CollectOptions::model`.
    pub model: PathBuf,
    pub filelist: String,
    pub top: String,
    /// Node count as the *harness* reported it, when it could be parsed.
    ///
    /// This is the automated half of the runbook's manual sanity check: the
    /// operator was told to compare it against the `real` row's `nodes` column,
    /// which is the only thing that catches a **partial** model (a missing
    /// include dir elaborates most of a design, exits 0, and reports
    /// plausible-looking numbers). Having both in one file makes that a glance.
    pub nodes: Option<u64>,
    pub wall: Duration,
}

impl Elaboration {
    /// The `| elaborated from |` row's value — one line stating where this
    /// run's `real` basis came from.
    pub fn provenance(&self) -> String {
        let nodes = match self.nodes {
            Some(n) => format!("{} nodes", with_thousands(n)),
            None => "node count not reported".into(),
        };
        format!(
            "{} (top {}), {nodes}, elaborated in {:.1} s -> {}",
            self.filelist,
            self.top,
            self.wall.as_secs_f64(),
            self.model.display()
        )
    }
}

/// Elaborate `filelist` and return where the model landed.
///
/// `model_out` keeps the JSON somewhere durable instead of [`scratch_dir`];
/// the temp path is what a `TEMP` sweep removes between sessions, and on the
/// isolated machine it may also be the volume without room for it.
pub fn elaborate_basis(
    filelist: &str,
    top: &str,
    incdirs: &[String],
    model_out: Option<&Path>,
) -> Result<Elaboration> {
    let model = match model_out {
        Some(p) => p.to_path_buf(),
        None => scratch_dir().join("hierarchy.json"),
    };
    if let Some(parent) = model.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    eprintln!("elaborating {filelist} (top {top}) -> {}", model.display());
    let started = Instant::now();
    // No `hls_src`: the benchmark measures the RTL model, and the HLS
    // provenance pass regex-scans every line of every RTL file for a mapping
    // nothing here reads.
    let log = svxprobe_gui::elaborate_to_file(filelist, top, incdirs, &[], &model)?;
    let wall = started.elapsed();

    Ok(Elaboration {
        model,
        filelist: filelist.to_string(),
        top: top.to_string(),
        nodes: harness_node_count(&log),
        wall,
    })
}

/// Pull the node count out of the harness's closing line, which reads
/// `elaborated <design>: <n> nodes, <m> files`.
///
/// Best-effort by design — a `None` costs the cross-check, not the run, so a
/// reworded harness message must never fail an otherwise good elaboration.
fn harness_node_count(log: &str) -> Option<u64> {
    let line = crate::collect::last_nonblank(log)?;
    let rest = line.strip_prefix("elaborated ")?;
    let (_, after_colon) = rest.split_once(": ")?;
    let (count, _) = after_colon.split_once(" nodes")?;
    count.trim().parse().ok()
}

/// `1203441` -> `1,203,441`. The measurement tables render counts ungrouped,
/// but this one is read by a human comparing it against a column, not diffed.
fn with_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Elaboration {
        Elaboration {
            model: PathBuf::from("/tmp/scale_bench_elaborate/hierarchy.json"),
            filelist: "design.f".into(),
            top: "soc_top".into(),
            nodes: Some(1_203_441),
            wall: Duration::from_millis(42_300),
        }
    }

    // Both entry points emit this row, so it is what makes two metrics files
    // comparable — pin the whole line, not a substring.
    #[test]
    fn provenance_names_the_filelist_top_size_and_cost() {
        assert_eq!(
            sample().provenance(),
            "design.f (top soc_top), 1,203,441 nodes, elaborated in 42.3 s \
             -> /tmp/scale_bench_elaborate/hierarchy.json"
        );
    }

    #[test]
    fn provenance_says_so_rather_than_lying_when_the_count_is_unknown() {
        let e = Elaboration {
            nodes: None,
            ..sample()
        };
        assert!(e.provenance().contains("node count not reported"));
    }

    #[test]
    fn the_node_count_comes_off_the_harness_closing_line() {
        let log = "warning: no time scale\nelaborated soc_top: 7231 nodes, 42 files";
        assert_eq!(harness_node_count(log), Some(7231));
    }

    // A reworded or missing closing line costs the cross-check, never the run.
    #[test]
    fn an_unrecognized_log_yields_no_count_rather_than_an_error() {
        for log in [
            "",
            "done",
            "elaborated soc_top",
            "elaborated soc_top: lots nodes",
        ] {
            assert_eq!(harness_node_count(log), None, "log: {log:?}");
        }
    }

    #[test]
    fn thousands_groups_from_the_right() {
        for (n, want) in [
            (0u64, "0"),
            (7, "7"),
            (999, "999"),
            (1_000, "1,000"),
            (7_231, "7,231"),
            (1_203_441, "1,203,441"),
        ] {
            assert_eq!(with_thousands(n), want);
        }
    }
}
