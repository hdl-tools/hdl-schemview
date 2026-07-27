//! The `--bench` / `--bench-scenario` launch arms (#240 tier 1, ADR 0009).
//!
//! The deployment target has no network, no toolchain and no package manager, so
//! `cargo bench` and the collector scripts are all unavailable there — yet
//! producing a metrics file is exactly what #22/#155 decisions are made from.
//! The whole matrix therefore lives in `scale_bench::collect`, and this module is
//! only the seam that lets the app binary drive it:
//!
//! - `--bench` is the parent: it runs the matrix and writes `metrics-<stamp>.md`.
//! - `--bench-scenario` is the child half of the parent's **self re-exec**. Peak
//!   RSS is only attributable to a process that did exactly one thing, so the
//!   parent spawns one child per row rather than looping in-process — and since a
//!   bundle carries no second executable, the child it spawns is *itself*.
//!
//! Deliberately thin: everything worth testing lives in `scale_bench::collect`
//! (`packaged_notes`, the pure `render`) or `svxprobe_gui::startup`
//! (`bench_out_path`), both of which `cargo test --all` covers. The app crate is
//! outside the core workspace and CI only `cargo build`s it.

use svxprobe_gui::startup::Launch;

/// Set on every spawned scenario child. The child is *this same binary*, so if
/// the re-exec ever loses the `--bench-scenario` marker — an AppImage `AppRun`
/// wrapper rewriting argv is the realistic way — the child would fall through to
/// the GUI arm and open a window. Once per matrix row. The guard turns that into
/// one clear error instead of dozens of windows.
const CHILD_MARKER: &str = "SVX_BENCH_CHILD";

/// Handle the benchmark launch arms, or hand the GUI arm back to the caller.
///
/// Called before `tauri::Builder::default()` so a headless run constructs no
/// Tauri state at all. Diverges (via `std::process::exit`) for both bench arms.
pub fn intercept(launch: Launch) -> Launch {
    if let Launch::Gui(_) = &launch {
        // Not `--bench-scenario`, yet spawned by a `--bench` parent: the self
        // re-exec is broken on this platform. Never open a window here.
        if std::env::var_os(CHILD_MARKER).is_some() {
            eprintln!(
                "error: launched as a benchmark child but without --bench-scenario — the \
                 self re-exec is not working on this platform.\n\
                 Run the scenarios with the dev `scale-bench` binaries instead, and please \
                 report the platform (see docs/benchmarking.md)."
            );
            std::process::exit(3);
        }
        return launch;
    }
    imp::dispatch(launch)
}

#[cfg(feature = "bench")]
mod imp {
    use super::CHILD_MARKER;
    use std::path::PathBuf;
    use svxprobe_gui::startup::{self, BenchRequest, Launch};

    use scale_bench::collect::{self, CollectOptions, ScenarioRunner};

    pub fn dispatch(launch: Launch) -> ! {
        match launch {
            // Verbatim pass-through: the scenario CLI owns its own validation,
            // usage text and exit codes, so the two contracts cannot fork.
            Launch::BenchScenario(rest) => {
                std::process::exit(scale_bench::scenario::main_with(rest))
            }
            Launch::Bench(req) => match run(req) {
                Ok(()) => std::process::exit(0),
                Err(msg) => {
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
            },
            // `intercept` returns the GUI arm before ever calling this.
            Launch::Gui(_) => unreachable!("the GUI arm is handled by intercept"),
        }
    }

    /// Mirrors `scale-bench/src/bin/collect.rs`'s `run`, minus the two layers a
    /// packaged build cannot have (criterion needs `cargo bench`; the `report`
    /// bin is a second executable). Same matrix, same renderer, same file.
    fn run(req: BenchRequest) -> Result<(), String> {
        let cwd =
            startup::invocation_dir(std::env::var_os("INIT_CWD"), std::env::current_dir().ok());

        // Canonicalized because the child inherits it as SCALE_BENCH_MODEL and
        // may run with a different working directory.
        let model = match &req.model {
            Some(p) => {
                let abs = startup::resolve_path(p, &cwd);
                Some(
                    std::fs::canonicalize(&abs)
                        .map_err(|e| format!("-model {abs} is not readable: {e}"))?,
                )
            }
            None => None,
        };

        let bases = req
            .bases
            .clone()
            .unwrap_or_else(|| collect::default_bases(req.full, model.is_some()));
        if bases.is_empty() {
            return Err(
                "no bases to run — this build has neither the `golden` nor the \
                        `synth` feature, and no -model was given"
                    .into(),
            );
        }

        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate this executable to re-exec it: {e}"))?;

        // Inherited by every child `collect::run` spawns, which is why the guard
        // needs no change to ScenarioRunner. Sound: single-threaded, before the
        // Tauri builder. (Safe in edition 2021; `unsafe` only from 2024.)
        std::env::set_var(CHILD_MARKER, "1");

        eprintln!("scenario matrix: {}", bases.join(", "));
        let runner = ScenarioRunner::new(exe, vec!["--bench-scenario".into()]);
        let result = collect::run(&CollectOptions {
            runner: &runner,
            bases: bases.clone(),
            model: model.clone(),
        })
        .map_err(|e| format!("{e:#}"))?;

        let mut env = collect::env_facts(&bases, model.as_deref());
        // `env_facts` leaves this empty on purpose: a packaged run must be
        // identifiable as one from the file alone.
        env.generator = "hdl-schemview --bench".into();

        let out: PathBuf = startup::bench_out_path(
            req.out.as_deref(),
            &cwd,
            &collect::stamp_utc(collect::now_unix()),
        );
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let text = collect::render(&env, &result.records, &collect::packaged_notes());
        std::fs::write(&out, text).map_err(|e| format!("cannot write {}: {e}", out.display()))?;

        println!("Done. Metrics written to:");
        println!("  {}", out.display());
        println!("Paste that file's contents back into the conversation for evaluation.");
        Ok(())
    }
}

#[cfg(not(feature = "bench"))]
mod imp {
    use svxprobe_gui::startup::{Launch, USAGE};

    /// The flags still parse — the contract and its tests are fixed in
    /// `svxprobe-gui` either way — but a build without the `bench` feature
    /// carries no matrix to run, so say so rather than open a window.
    pub fn dispatch(_launch: Launch) -> ! {
        eprintln!("error: this build was compiled without the benchmark feature\n\n{USAGE}");
        std::process::exit(2);
    }
}
