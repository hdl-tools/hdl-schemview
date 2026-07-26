//! End-to-end: the driver actually spawning scenarios (#240).
//!
//! `#[ignore]`d on purpose. It needs a built `scenario` binary and runs nine real
//! measurements, which is minutes of wall time — too slow for the PR gate, where
//! `tests/collect.rs` already covers every formatting decision against hand-built
//! records. Run it when the *spawning* is what changed:
//!
//! ```text
//! cargo build --release -p scale-bench
//! cargo test --release -p scale-bench --test collect_e2e -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use scale_bench::collect::{self, CollectOptions, Notes, ScenarioRunner};

/// The `scenario` binary beside this test's own executable.
fn scenario_exe() -> Option<PathBuf> {
    // A test binary lands in `target/<profile>/deps/`, so the bins are one level up.
    let exe = std::env::current_exe().ok()?;
    let deps = exe.parent()?.to_path_buf();
    let profile = deps.parent()?.to_path_buf();
    [profile, deps]
        .into_iter()
        .flat_map(|dir| {
            [
                dir.join(format!("scenario{}", std::env::consts::EXE_SUFFIX)),
                dir.join("scenario"),
            ]
        })
        .find(|p| p.is_file())
}

#[test]
#[ignore = "spawns nine real scenarios; needs a built `scenario` binary"]
fn the_matrix_runs_end_to_end_on_one_basis() {
    let Some(exe) = scenario_exe() else {
        panic!("no `scenario` binary found — run `cargo build -p scale-bench` first");
    };
    let runner = ScenarioRunner::new(exe, Vec::new());
    let result = collect::run(&CollectOptions {
        runner: &runner,
        bases: vec!["665".into()],
        model: None,
    })
    .expect("the runner resolves");

    // Five load modes, then nav + three matcher points because prepare succeeded.
    assert_eq!(result.records.len(), 9, "unexpected matrix size");
    for rec in &result.records {
        assert!(
            rec.ok(),
            "{}/{} failed: {}",
            rec.basis(),
            rec.mode(),
            rec.error()
        );
    }

    // The rendered file must be complete enough to hand over as-is.
    let env = collect::env_facts(&["665".to_string()], None);
    let text = collect::render(&env, &result.records, &Notes::default());
    assert!(text.contains("| 665 | prepare |"));
    assert!(text.contains("## Scripted navigation"));
    assert!(text.contains("## Raw scenario records"));
}

#[test]
#[ignore = "spawns a scenario process"]
fn an_unusable_runner_fails_before_measuring_anything() {
    // A missing binary must not be reported as N identical failure rows that look
    // like measurements.
    let runner = ScenarioRunner::new(Path::new("definitely-not-a-binary"), Vec::new());
    let err = collect::run(&CollectOptions {
        runner: &runner,
        bases: vec!["665".into()],
        model: None,
    })
    .expect_err("an absent runner is fatal");
    assert!(
        err.to_string().contains("scenario runner not found"),
        "unexpected error: {err:#}"
    );
}

#[test]
#[ignore = "spawns a scenario process"]
fn a_failing_scenario_becomes_a_row_rather_than_aborting_the_run() {
    // An unknown basis fails in `prepare`, which is the cheapest way to exercise
    // the failure path: the run must still produce its five load rows, and skip
    // nav/match rather than repeating the same error four more times.
    let Some(exe) = scenario_exe() else {
        panic!("no `scenario` binary found — run `cargo build -p scale-bench` first");
    };
    let runner = ScenarioRunner::new(exe, Vec::new());
    let result = collect::run(&CollectOptions {
        runner: &runner,
        bases: vec!["not-a-basis".into()],
        model: None,
    })
    .expect("the run itself must not fail");

    assert_eq!(
        result.records.len(),
        5,
        "nav/match should have been skipped"
    );
    assert!(result.records.iter().all(|r| !r.ok()));
    let text = collect::render(
        &collect::env_facts(&["not-a-basis".to_string()], None),
        &result.records,
        &Notes::default(),
    );
    assert!(text.contains("**FAILED**"), "no failure row rendered");
}
