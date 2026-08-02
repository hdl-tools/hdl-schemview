//! Collect a full metrics file — the developer/CI front door to
//! [`scale_bench::collect`] (#240).
//!
//! ```text
//! collect [--full] [--model <hierarchy.json>] [--bases "golden 665"]
//!         [--skip-criterion] [--skip-report] [--online] [--out <path>]
//! ```
//!
//! Three layers — the scenario matrix, then criterion, then the single-shot report
//! bin — assembled into one paste-ready markdown file. The *matrix* lives in
//! `scale_bench::collect::run`, so the packaged app can run it with no shell at
//! all; this bin adds only the two layers that inherently need a toolchain.
//!
//! Criterion estimates come from walking `target/criterion/**/new/estimates.json`
//! filtered by modification time: the directory keeps prior runs around, and a
//! stale estimate silently mixed into the table would be worse than a missing row.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use scale_bench::collect::{self, CollectOptions, CriterionRow, Notes, ScenarioRunner};
use scale_bench::elaborate;

const USAGE: &str = "\
usage: collect [--full] [--model <hierarchy.json>] [--bases \"golden 665\"]
               [--skip-criterion] [--skip-report] [--online] [--out <path>]
       collect [...] --filelist <design.f> --top <name> [--include <dir>]...
               [--model-out <path>]

  --full             add the 1M basis (slow; may be killed by the OS — that is a result)
  --model <path>     an elaborated hierarchy.json, measured as the `real` basis
  --bases \"a b c\"    override the basis list entirely
  --skip-criterion   skip `cargo bench` (the statistical layer)
  --skip-report      skip the single-shot report bin
  --online           allow cargo to reach the network (default is --offline)
  --out <path>       metrics file (default: <core>/target/scale-bench/metrics-<stamp>.md)

  --filelist <f>     elaborate this designlist into the `real` basis (#255),
                     instead of passing an already-elaborated --model. Uses the
                     same argv as the app's own designlist load, so the model
                     measured is the model the tool loads. Needs --top.
  --top <name>       top module name; required with --filelist
  --include <dir>    include directory for the elaboration, repeatable
  --model-out <path> keep the elaborated JSON here instead of a temp scratch
                     dir, so a later run can skip elaboration with --model

environment:
  SVXPROBE_ELABORATE  path to the elaboration harness, when it is not on PATH";

struct Args {
    full: bool,
    model: Option<PathBuf>,
    bases: Option<Vec<String>>,
    skip_criterion: bool,
    skip_report: bool,
    online: bool,
    out: Option<PathBuf>,
    filelist: Option<String>,
    top: Option<String>,
    incdirs: Vec<String>,
    model_out: Option<PathBuf>,
}

fn parse_args(argv: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        full: false,
        model: None,
        bases: None,
        skip_criterion: false,
        skip_report: false,
        online: false,
        out: None,
        filelist: None,
        top: None,
        incdirs: Vec::new(),
        model_out: None,
    };
    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        // Reject a value that is itself a flag, like `startup.rs`'s parser:
        // `--model --out x` silently took `--out` as the model path, and the
        // adjacent `--filelist`/`--top`/`--include` make that far likelier.
        let mut value = |flag: &str| -> Result<String, String> {
            match it.next() {
                Some(v) if !v.starts_with('-') => Ok(v),
                Some(v) => Err(format!("{flag} expects a value, got flag '{v}'")),
                None => Err(format!("{flag} needs a value")),
            }
        };
        match arg.as_str() {
            "--full" => args.full = true,
            "--skip-criterion" => args.skip_criterion = true,
            "--skip-report" => args.skip_report = true,
            "--online" => args.online = true,
            "--model" => args.model = Some(value("--model")?.into()),
            "--out" => args.out = Some(value("--out")?.into()),
            "--filelist" | "-f" => args.filelist = Some(value("--filelist")?),
            "--top" => args.top = Some(value("--top")?),
            "--include" | "-I" => args.incdirs.push(value("--include")?),
            "--model-out" => args.model_out = Some(value("--model-out")?.into()),
            "--bases" => {
                let raw = value("--bases")?;
                let list: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
                if list.is_empty() {
                    return Err("--bases needs at least one basis".into());
                }
                args.bases = Some(list);
            }
            "-h" | "--help" => return Err("help requested".into()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    check_designlist(&args)?;
    Ok(args)
}

/// The designlist flags' cross-flag rules (#255), mirroring
/// `svxprobe_gui::startup`'s for the packaged `--bench`. Each exists because
/// the alternative is a run that silently measures something other than what
/// was asked for.
fn check_designlist(args: &Args) -> Result<(), String> {
    if args.filelist.is_some() && args.model.is_some() {
        return Err("--filelist and --model are mutually exclusive \
                    (--filelist elaborates one for you)"
            .into());
    }
    match (&args.filelist, &args.top) {
        (Some(_), None) => return Err("--filelist requires --top <name>".into()),
        (None, Some(_)) => return Err("--top requires --filelist <design.f>".into()),
        _ => {}
    }
    if args.filelist.is_none() {
        if !args.incdirs.is_empty() {
            return Err("--include requires --filelist <design.f>".into());
        }
        if args.model_out.is_some() {
            return Err("--model-out requires --filelist <design.f>".into());
        }
    } else if let Some(bases) = &args.bases {
        // An explicit --bases replaces the default list, so one without `real`
        // would elaborate the design and then never measure it.
        if !bases.iter().any(|b| b == "real") {
            return Err(
                "--filelist elaborates the `real` basis, but --bases does not include it".into(),
            );
        }
    }
    Ok(())
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) if e == "help requested" => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if let Err(e) = run(&args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let target_dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            anyhow::anyhow!("cannot locate the target directory from {}", exe.display())
        })?
        .to_path_buf();
    let core_dir = target_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot locate the core directory"))?
        .to_path_buf();

    // Everything cheap that can fail is checked *before* the elaboration:
    // discovering a missing `scenario` binary after a multi-minute elaboration
    // would throw the expensive step away.
    let will_have_model = args.model.is_some() || args.filelist.is_some();
    let bases = args
        .bases
        .clone()
        .unwrap_or_else(|| collect::default_bases(args.full, will_have_model));
    if bases.is_empty() {
        anyhow::bail!(
            "no bases to run — this build has neither the `golden` nor the `synth` feature, \
             and no --model was given"
        );
    }

    let scenario_exe = sibling(&exe, "scenario").ok_or_else(|| {
        anyhow::anyhow!(
            "no `scenario` binary beside {} — build it with `cargo build --release -p scale-bench`",
            exe.display()
        )
    })?;

    let elaborated = match &args.filelist {
        Some(filelist) => Some(elaborate::elaborate_basis(
            filelist,
            args.top.as_deref().unwrap_or_default(),
            &args.incdirs,
            args.model_out.as_deref(),
        )?),
        None => None,
    };
    let model = match elaborated
        .as_ref()
        .map(|e| e.model.clone())
        .or(args.model.clone())
    {
        // Canonicalized because the child scenario inherits it as an env var and
        // may run with a different working directory.
        Some(p) => Some(
            std::fs::canonicalize(&p)
                .map_err(|e| anyhow::anyhow!("model {} is not readable: {e}", p.display()))?,
        ),
        None => None,
    };

    eprintln!("scenario matrix: {}", bases.join(", "));
    let runner = ScenarioRunner::new(scenario_exe, Vec::new());
    let result = collect::run(&CollectOptions {
        runner: &runner,
        bases: bases.clone(),
        model: model.clone(),
    })?;

    let mut notes = Notes::default();
    if args.skip_criterion {
        notes.criterion_note = "skipped (--skip-criterion)".into();
    } else {
        run_criterion(&core_dir, &target_dir, args.online, &mut notes);
    }
    if args.skip_report {
        notes.report_note = Some("skipped (--skip-report)".into());
    } else {
        run_report(&exe, args.full, model.as_deref(), &mut notes);
    }

    let mut env = collect::env_facts(&bases, model.as_deref());
    env.generator = "scale-bench collect".into();
    if let Some(e) = &elaborated {
        env.elaborated = e.provenance();
    }

    let out = args.out.clone().unwrap_or_else(|| {
        target_dir.join("scale-bench").join(format!(
            "metrics-{}.md",
            collect::stamp_utc(collect::now_unix())
        ))
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, collect::render(&env, &result.records, &notes))?;

    println!("Done. Metrics written to:");
    println!("  {}", out.display());
    // Repeated here on purpose: the elaboration announced this path before a
    // matrix that scrolls for many screens, and skipping the re-elaboration is
    // the only reason to know it.
    if let Some(e) = &elaborated {
        println!("Elaborated model kept at:");
        println!("  {}", e.model.display());
        println!("  reuse it with --model <that path> to skip elaboration next time.");
    }
    println!("Paste that file's contents back into the conversation for evaluation.");
    Ok(())
}

/// A sibling binary of `exe`, with the platform's executable suffix.
fn sibling(exe: &Path, name: &str) -> Option<PathBuf> {
    let dir = exe.parent()?;
    // Suffixed name first, bare name second — the same order the collector
    // scripts probe in, so a cross-built layout resolves identically.
    [
        dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        dir.join(name),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn run_criterion(core_dir: &Path, target_dir: &Path, online: bool, notes: &mut Notes) {
    // Short warm-up and few samples: this layer is here for the *shape* of the
    // distribution, and the collector already takes minutes.
    // Deselecting the `collect` feature is what keeps this layer working: cargo
    // builds a crate's *bin* targets for the bench profile too (so an integration
    // test can exec them), which means a bare `cargo bench` tries to relink
    // `collect.exe` — the process running right now. Windows refuses to replace a
    // locked executable and the whole layer dies with "failed to remove file
    // collect.exe (os error 5)". `--benches` alone does not help; the bins are built
    // regardless of target selection. The `collect` bin carries
    // `required-features = ["collect"]`, so dropping that feature is the one thing
    // that excludes it. `synth` and `golden` are named explicitly because the three
    // benches need them — a bench that grows a new dependency gets a compile error
    // here rather than silently vanishing from the table.
    let mut cmd = Command::new("cargo");
    cmd.current_dir(core_dir).args([
        "bench",
        "-p",
        "scale-bench",
        "--benches",
        "--no-default-features",
        "--features",
        "synth,golden",
    ]);
    if !online {
        cmd.arg("--offline");
    }
    cmd.args([
        "--",
        "--warm-up-time",
        "1",
        "--measurement-time",
        "3",
        "--sample-size",
        "20",
    ]);

    eprintln!("criterion: cargo bench -p scale-bench --benches (without the `collect` feature)");
    let started = SystemTime::now();
    let code = match cmd.status() {
        Ok(s) => s.code().unwrap_or(-1),
        Err(e) => {
            notes.criterion_note = format!("cargo bench could not be started: {e}");
            return;
        }
    };
    notes.criterion_note = format!(
        "cargo bench -p scale-bench --benches --no-default-features --features synth,golden \
         -- --warm-up-time 1 --measurement-time 3 --sample-size 20 (exit {code})"
    );
    notes.criterion_ran = true;
    notes.criterion_rows = harvest_criterion(&target_dir.join("criterion"), started);
}

/// Estimates written by *this* run, keyed on modification time.
fn harvest_criterion(criterion_dir: &Path, since: SystemTime) -> Vec<CriterionRow> {
    let mut rows = Vec::new();
    let mut stack = vec![criterion_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) != Some("estimates.json") {
                continue;
            }
            // Only the `new` directory holds this run's estimates; `base` and
            // `change` are comparisons against previous ones.
            if path.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("new")) {
                continue;
            }
            let fresh = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|m| m >= since)
                .unwrap_or(false);
            if !fresh {
                continue;
            }
            if let Some(row) = read_estimates(&path) {
                rows.push(row);
            }
        }
    }
    rows
}

fn read_estimates(path: &Path) -> Option<CriterionRow> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    // Criterion stores nanoseconds; the table is in ms.
    let point = |key: &str| -> f64 {
        json.get(key)
            .and_then(|v| v.get("point_estimate"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(f64::NAN)
            / 1e6
    };
    let dir = path.parent()?;
    let id = std::fs::read_to_string(dir.join("benchmark.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("full_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        // Fall back to the directory path, which still identifies the bench.
        .unwrap_or_else(|| dir.display().to_string());
    Some(CriterionRow {
        id,
        mean_ms: point("mean"),
        median_ms: point("median"),
        std_dev_ms: point("std_dev"),
    })
}

fn run_report(exe: &Path, full: bool, model: Option<&Path>, notes: &mut Notes) {
    let Some(report_exe) = sibling(exe, "report") else {
        notes.report_note = Some(
            "report bin not found beside collect — build it with \
             `cargo build --release -p scale-bench`"
                .into(),
        );
        return;
    };

    eprintln!("report bin: {}", report_exe.display());
    let mut cmd = Command::new(&report_exe);
    if full {
        cmd.arg("--full");
    }
    if let Some(model) = model {
        cmd.env("SCALE_BENCH_MODEL", model);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => {
            // Keep only the table: the bin also writes prose to stderr, and a
            // skipped row explains itself there.
            notes.report_table = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.starts_with('|'))
                .map(str::to_string)
                .collect();
            if notes.report_table.is_empty() {
                notes.report_note = Some("report bin produced no table".into());
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = collect::last_nonblank(&stderr).unwrap_or("no output");
            notes.report_note = Some(format!(
                "report bin failed (exit {}): {why}",
                out.status.code().unwrap_or(-1)
            ));
        }
        Err(e) => notes.report_note = Some(format!("report bin could not be started: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    fn parse(argv: &[&str]) -> Result<super::Args, String> {
        parse_args(argv.iter().map(|s| s.to_string()))
    }

    fn err(argv: &[&str]) -> String {
        parse(argv).err().expect("expected a usage error")
    }

    #[test]
    fn the_designlist_flags_land_in_their_fields() {
        let args = parse(&[
            "--filelist",
            "soc.f",
            "--top",
            "soc_top",
            "--include",
            "rtl/inc",
            "--include",
            "vendor/inc",
            "--model-out",
            "keep.json",
        ])
        .expect("valid");
        assert_eq!(args.filelist.as_deref(), Some("soc.f"));
        assert_eq!(args.top.as_deref(), Some("soc_top"));
        assert_eq!(args.incdirs, ["rtl/inc", "vendor/inc"]);
        assert_eq!(args.model_out.unwrap().to_str(), Some("keep.json"));
    }

    // The EDA-style short spellings the packaged `--bench` arm takes, so a
    // command line can be moved between the two entry points unchanged.
    #[test]
    fn the_short_designlist_spellings_are_accepted_too() {
        let args = parse(&["-f", "soc.f", "--top", "t", "-I", "rtl/inc"]).expect("valid");
        assert_eq!(args.filelist.as_deref(), Some("soc.f"));
        assert_eq!(args.incdirs, ["rtl/inc"]);
    }

    // Each rule exists because the alternative is a run that silently measures
    // something other than what was asked for.
    #[test]
    fn the_designlist_flags_require_each_other() {
        assert!(err(&["--filelist", "soc.f", "--model", "m.json"]).contains("mutually exclusive"));
        assert_eq!(
            err(&["--filelist", "soc.f"]),
            "--filelist requires --top <name>"
        );
        assert_eq!(
            err(&["--top", "soc_top"]),
            "--top requires --filelist <design.f>"
        );
        assert_eq!(
            err(&["--include", "rtl/inc"]),
            "--include requires --filelist <design.f>"
        );
        assert_eq!(
            err(&["--model-out", "keep.json"]),
            "--model-out requires --filelist <design.f>"
        );
        assert!(
            err(&["--filelist", "soc.f", "--top", "t", "--bases", "golden 665"])
                .contains("does not include it")
        );
        assert!(parse(&[
            "--filelist",
            "soc.f",
            "--top",
            "t",
            "--bases",
            "golden real"
        ])
        .is_ok());
    }

    // Regression: before #255 these arms took the next token blindly, so
    // `--model --out x` silently used `--out` as the model path.
    #[test]
    fn a_flag_where_a_value_is_expected_is_rejected() {
        assert!(err(&["--model", "--out", "m.md"]).contains("expects a value"));
        assert!(err(&["--filelist", "--top"]).contains("expects a value"));
        assert!(err(&["--model"]).contains("needs a value"));
    }

    #[test]
    fn the_pre_existing_flags_still_parse_as_before() {
        let args = parse(&[
            "--full",
            "--model",
            "m.json",
            "--bases",
            "golden 665",
            "--skip-criterion",
            "--skip-report",
            "--online",
            "--out",
            "m.md",
        ])
        .expect("valid");
        assert!(args.full && args.skip_criterion && args.skip_report && args.online);
        assert_eq!(args.model.unwrap().to_str(), Some("m.json"));
        assert_eq!(args.out.unwrap().to_str(), Some("m.md"));
        assert_eq!(args.bases.unwrap(), ["golden", "665"]);
        assert!(parse(&["--bases", "   "]).is_err());
        assert_eq!(err(&["-h"]), "help requested");
        assert!(err(&["--nope"]).contains("unknown argument"));
    }

    #[test]
    fn usage_documents_the_designlist_flags_and_the_harness_override() {
        for flag in ["--filelist", "--top", "--include", "--model-out"] {
            assert!(super::USAGE.contains(flag), "USAGE omits {flag}");
        }
        assert!(super::USAGE.contains("SVXPROBE_ELABORATE"));
    }
}
