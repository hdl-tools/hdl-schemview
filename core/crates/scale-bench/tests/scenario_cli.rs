//! The scenario command-line contract (#240).
//!
//! Nothing covered this before: the parser lived inside a `[[bin]]`, which is not
//! importable, so its flags, defaults and error cases were only ever exercised by
//! running the binary. They matter more now — the packaged app's
//! `--bench-scenario` arm forwards an argv tail into the same parser, and
//! `docs/benchmarking.md` promises a hand-run scenario is comparable to a
//! collected one, which is only true if the defaults hold.

use scale_bench::scenario::{self, NAV_ITERS, NAV_SCOPES};

fn args(argv: &[&str]) -> Result<scenario::ScenarioArgs, String> {
    scenario::parse_args(argv.iter().map(|s| s.to_string()))
}

#[test]
fn basis_and_mode_are_required() {
    assert!(args(&[]).is_err());
    assert!(args(&["--basis", "665"]).is_err());
    assert!(args(&["--mode", "nav"]).is_err());
    let ok = args(&["--basis", "665", "--mode", "nav"]).expect("both given");
    assert_eq!(ok.basis, "665");
    assert_eq!(ok.mode, "nav");
}

#[test]
fn nav_defaults_are_the_documented_ones() {
    // The collector relies on these rather than passing them, so a change here
    // silently makes new runs incomparable to every published one.
    let a = args(&["--basis", "golden", "--mode", "nav"]).expect("parses");
    assert_eq!((a.nav_scopes, a.nav_iters), (NAV_SCOPES, NAV_ITERS));
    assert_eq!((NAV_SCOPES, NAV_ITERS), (32, 3));
    // `--signals` stays absent so the mode can pick its own default.
    assert_eq!(a.signals, None);
}

#[test]
fn nav_counts_are_clamped_to_at_least_one() {
    // Zero scopes would divide by zero downstream; zero iterations would report
    // empty percentile samples as if they were measurements.
    let a = args(&[
        "--basis",
        "665",
        "--mode",
        "nav",
        "--nav-scopes",
        "0",
        "--nav-iters",
        "0",
    ])
    .expect("parses");
    assert_eq!((a.nav_scopes, a.nav_iters), (1, 1));
}

#[test]
fn signals_is_parsed_when_given() {
    let a = args(&["--basis", "665", "--mode", "match", "--signals", "10000"]).expect("parses");
    assert_eq!(a.signals, Some(10_000));
    // A non-number is a usage error, not a silent zero.
    assert!(args(&["--basis", "665", "--mode", "match", "--signals", "many"]).is_err());
}

#[test]
fn unknown_flags_and_missing_values_are_errors() {
    assert!(args(&["--basis", "665", "--mode", "nav", "--nope"]).is_err());
    assert!(args(&["--basis"]).is_err());
    assert!(args(&["--basis", "665", "--mode"]).is_err());
}

#[test]
fn help_is_an_error_the_caller_turns_into_usage() {
    // The bin and the app both print USAGE and exit 2 for this, which is why it
    // is an `Err` rather than a distinct variant.
    assert!(args(&["-h"]).is_err());
    assert!(args(&["--help"]).is_err());
}

#[test]
fn usage_lists_every_mode_the_dispatcher_accepts() {
    // A mode added to `run` but not to USAGE would be undiscoverable; one listed
    // but unhandled would fail only at runtime.
    for mode in [
        "prepare",
        "from_slice",
        "cache_hit",
        "access_checked",
        "access_unchecked",
        "nav",
        "match",
    ] {
        assert!(
            scenario::USAGE.contains(mode),
            "USAGE does not mention the `{mode}` mode"
        );
    }
    for mode in scenario::LOAD_MODES {
        assert!(scenario::USAGE.contains(mode));
    }
}

#[test]
fn an_unknown_mode_fails_rather_than_silently_measuring_nothing() {
    let a = args(&["--basis", "665", "--mode", "nope"]).expect("parsing accepts any mode string");
    // Mode validity is the dispatcher's call, so the error surfaces there.
    let err = scenario::run(&a).expect_err("unknown mode must fail");
    assert!(err.contains("unknown mode"), "unexpected error: {err}");
}
