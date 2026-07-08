//! Command-line launch arguments for the desktop app (#136).
//!
//! EDA users expect single-dash long flags (`schemview -f soc.f -I rtl -top soc`).
//! `clap` tokenizes `-top` as clustered shorts and `tauri-plugin-cli` is `clap`
//! underneath, so both fight the requested syntax — this is a small hand-rolled
//! parser instead, with no new dependency. It lives in `svxprobe-gui` so
//! `cargo test --all` covers it headlessly under the PR gate (the app crate is
//! outside the core workspace and not CI-gated).
//!
//! The Tauri shell parses `std::env::args_os()` (lossy, so a non-UTF-8 arg
//! can't panic the pre-window path) at the top of `run()` before any window
//! is created and exposes the result to the frontend via a pull-based
//! `startup_args` command; the frontend prefills the load form and auto-loads,
//! byte-identical to clicking **Load**.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Parsed launch arguments — the designlist flow's form fields, filled from
/// argv instead of the UI. Mirrors `app/src/types.ts` `StartupArgs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupArgs {
    pub filelist: String,
    pub top: String,
    pub incdirs: Vec<String>,
    pub trace: String,
    pub src_root: String,
}

/// Why parsing did not yield launch args. `Help` is a clean `-h`/`--help`
/// request (print usage, exit 0); `Usage` is a user error (print to stderr,
/// exit 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupError {
    Help,
    Usage(String),
}

/// Usage text, shown for `-h`/`--help` and on a usage error.
pub const USAGE: &str = "\
hdl-schemview — RTL-level SystemVerilog cross-probe tool

USAGE:
    hdl-schemview [-f <filelist.f> -top <top>] [-I <incdir>]... [-trace <trace>] [-src-root <dir>]

OPTIONS:
    -f <filelist.f>     designlist (.f) to elaborate (required with -top)
    -top <name>         top module name (required with -f)
    -I <incdir>         include directory (repeatable)
    -trace <path>       waveform trace (VCD/FST) to load
    -src-root <dir>     source root for the source view (default: .)
    -h, --help          print this help

With no arguments the app opens the normal load form. Long flags accept either
one dash (-top) or two (--top).";

/// Parse launch arguments (the iterator must exclude the program name, e.g.
/// `std::env::args().skip(1)`).
///
/// - No arguments → `Ok(None)`: boot the normal GUI with today's load form.
/// - `-h`/`--help` → `Err(Help)`.
/// - A recognized, complete designlist (`-f` + `-top`) → `Ok(Some(_))`.
/// - Anything else (unknown flag, missing value, `-f` without `-top`, repeated
///   `-f`) → `Err(Usage(_))`.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<StartupArgs>, StartupError> {
    let mut it = args.into_iter().peekable();
    // No arguments at all: boot the normal GUI, no auto-load.
    if it.peek().is_none() {
        return Ok(None);
    }

    let mut filelist: Option<String> = None;
    let mut top: Option<String> = None;
    let mut incdirs: Vec<String> = Vec::new();
    let mut trace: Option<String> = None;
    let mut src_root: Option<String> = None;

    // Accept a long flag under either one dash (`-top`, EDA-style) or two
    // (`--top`); `value` pulls the following token, erroring if it is absent or
    // itself looks like a flag (`-f -top x` → the user forgot the filelist).
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| -> Result<String, StartupError> {
            match it.next() {
                Some(v) if !v.starts_with('-') => Ok(v),
                Some(v) => Err(StartupError::Usage(format!(
                    "{flag} expects a value, got flag '{v}'"
                ))),
                None => Err(StartupError::Usage(format!("missing value for {flag}"))),
            }
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(StartupError::Help),
            "-f" => {
                let v = value("-f")?;
                if filelist.is_some() {
                    return Err(StartupError::Usage(
                        "-f may be given only once (the session takes one filelist)".into(),
                    ));
                }
                filelist = Some(v);
            }
            "-top" | "--top" => top = Some(value("-top")?),
            "-I" => incdirs.push(value("-I")?),
            "-trace" | "--trace" => trace = Some(value("-trace")?),
            "-src-root" | "--src-root" => src_root = Some(value("-src-root")?),
            other => {
                return Err(StartupError::Usage(format!("unknown argument: {other}")));
            }
        }
    }

    // `-f` and `-top` are required together — the designlist flow needs both.
    match (filelist, top) {
        (Some(filelist), Some(top)) => Ok(Some(StartupArgs {
            filelist,
            top,
            incdirs,
            trace: trace.unwrap_or_default(),
            src_root: src_root.unwrap_or_else(|| ".".to_string()),
        })),
        (Some(_), None) => Err(StartupError::Usage("-f requires -top <name>".into())),
        (None, Some(_)) => Err(StartupError::Usage("-top requires -f <filelist>".into())),
        (None, None) => Err(StartupError::Usage(
            "-f <filelist> and -top <name> are required".into(),
        )),
    }
}

/// The directory a relative launch path should resolve against: the directory
/// the user invoked from, not the tool's working directory.
///
/// `npm run tauri dev` runs the binary with its cwd set to `src-tauri` (the
/// tool path), so `current_dir()` there is wrong for a user's relative path —
/// but npm records the real invocation directory in `INIT_CWD`, which
/// propagates to the spawned binary. Prefer it; a bundled binary launched
/// straight from a terminal has no `INIT_CWD` and its `current_dir()` is
/// already the user's directory. Pure (takes both candidates) so it is
/// testable without touching the environment.
pub fn invocation_dir(init_cwd: Option<OsString>, current_dir: Option<PathBuf>) -> PathBuf {
    init_cwd
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or(current_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Make a possibly-relative path absolute against `cwd` (leaving it untouched
/// when already absolute or empty). Kept separate from existence checks so it
/// stays pure and testable without a filesystem.
pub fn resolve_path(p: &str, cwd: &Path) -> String {
    if p.is_empty() {
        return String::new();
    }
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        cwd.join(path).to_string_lossy().into_owned()
    }
}

impl StartupArgs {
    /// Resolve the designlist/trace/src-root/incdir paths against `cwd` (so
    /// relative paths behave the same under `tauri dev` and a bundled binary),
    /// then check that the filelist and, if given, the trace are real files.
    /// Missing incdirs / a bad top name are left to the harness's existing
    /// error surface in the status line. Returns a plain message on a bad path
    /// (a distinct error from `parse`'s `Help`/`Usage`, so the caller never has
    /// to handle a `Help` that this path cannot produce).
    pub fn resolve(mut self, cwd: &Path) -> Result<Self, String> {
        self.filelist = resolve_path(&self.filelist, cwd);
        self.trace = resolve_path(&self.trace, cwd);
        self.src_root = resolve_path(&self.src_root, cwd);
        self.incdirs = self.incdirs.iter().map(|d| resolve_path(d, cwd)).collect();
        if !Path::new(&self.filelist).is_file() {
            return Err(format!(
                "filelist not found or not a file: {}",
                self.filelist
            ));
        }
        if !self.trace.is_empty() && !Path::new(&self.trace).is_file() {
            return Err(format!("trace not found or not a file: {}", self.trace));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> StartupArgs {
        parse(args.iter().map(|s| s.to_string()))
            .expect("parse should succeed")
            .expect("should yield args")
    }

    #[test]
    fn no_args_boots_the_normal_gui() {
        assert_eq!(parse(std::iter::empty()), Ok(None));
    }

    #[test]
    fn happy_path_collects_the_designlist_fields() {
        let a = parse_ok(&[
            "-f",
            "soc.f",
            "-top",
            "soc_top",
            "-trace",
            "sim.fst",
            "-src-root",
            "rtl",
        ]);
        assert_eq!(a.filelist, "soc.f");
        assert_eq!(a.top, "soc_top");
        assert_eq!(a.trace, "sim.fst");
        assert_eq!(a.src_root, "rtl");
    }

    #[test]
    fn trace_and_src_root_are_optional_with_defaults() {
        let a = parse_ok(&["-f", "soc.f", "-top", "soc_top"]);
        assert_eq!(a.trace, "");
        assert_eq!(a.src_root, ".");
        assert!(a.incdirs.is_empty());
    }

    #[test]
    fn include_dirs_are_repeatable() {
        let a = parse_ok(&["-f", "soc.f", "-top", "soc_top", "-I", "rtl", "-I", "inc"]);
        assert_eq!(a.incdirs, vec!["rtl".to_string(), "inc".to_string()]);
    }

    #[test]
    fn long_flags_accept_one_or_two_dashes() {
        let one = parse_ok(&["-f", "soc.f", "-top", "soc_top"]);
        let two = parse_ok(&["-f", "soc.f", "--top", "soc_top"]);
        assert_eq!(one.top, "soc_top");
        assert_eq!(two.top, "soc_top");
    }

    #[test]
    fn filelist_without_top_is_a_usage_error() {
        assert!(matches!(
            parse(["-f", "soc.f"].iter().map(|s| s.to_string())),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn top_without_filelist_is_a_usage_error() {
        assert!(matches!(
            parse(["-top", "soc_top"].iter().map(|s| s.to_string())),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn repeated_filelist_is_rejected() {
        assert!(matches!(
            parse(
                ["-f", "a.f", "-f", "b.f", "-top", "t"]
                    .iter()
                    .map(|s| s.to_string())
            ),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        assert!(matches!(
            parse(["--nope"].iter().map(|s| s.to_string())),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn missing_flag_value_is_a_usage_error() {
        assert!(matches!(
            parse(["-f"].iter().map(|s| s.to_string())),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn a_flag_where_a_value_is_expected_is_a_usage_error() {
        // `-f -top t` — the user forgot the filelist; catch it at the `-f`, not
        // by silently swallowing `-top` as the filelist value.
        assert!(matches!(
            parse(["-f", "-top", "t"].iter().map(|s| s.to_string())),
            Err(StartupError::Usage(_))
        ));
    }

    #[test]
    fn a_repeated_single_value_flag_takes_the_last() {
        // `-f` is unique (one filelist per session); the other single-value
        // flags document last-wins.
        let a = parse_ok(&[
            "-f", "a.f", "-top", "t1", "-top", "t2", "-trace", "x", "-trace", "y",
        ]);
        assert_eq!(a.top, "t2");
        assert_eq!(a.trace, "y");
    }

    #[test]
    fn help_is_requested_explicitly() {
        assert_eq!(
            parse(["-h"].iter().map(|s| s.to_string())),
            Err(StartupError::Help)
        );
        assert_eq!(
            parse(["--help"].iter().map(|s| s.to_string())),
            Err(StartupError::Help)
        );
    }

    #[test]
    fn invocation_dir_prefers_init_cwd_over_current_dir() {
        // `npm run tauri dev` runs the binary with cwd = src-tauri (the tool
        // path) but records the user's real shell dir in INIT_CWD — prefer it
        // so relative launch paths resolve from where the user actually is.
        assert_eq!(
            invocation_dir(
                Some("/home/u/proj".into()),
                Some(PathBuf::from("/home/u/proj/app/src-tauri")),
            ),
            PathBuf::from("/home/u/proj"),
        );
        // A bundled binary launched from a terminal: no INIT_CWD, cwd is right.
        assert_eq!(
            invocation_dir(None, Some(PathBuf::from("/tmp/run"))),
            PathBuf::from("/tmp/run"),
        );
        // An empty INIT_CWD is treated as unset.
        assert_eq!(
            invocation_dir(Some("".into()), Some(PathBuf::from("/tmp/run"))),
            PathBuf::from("/tmp/run"),
        );
        // Neither available: fall back to the process cwd (".").
        assert_eq!(invocation_dir(None, None), PathBuf::from("."));
    }

    #[test]
    fn resolve_path_makes_relative_absolute_against_cwd() {
        let cwd = Path::new("/work/proj");
        assert_eq!(resolve_path("", cwd), "");
        // A relative path is joined under cwd; an absolute one is left alone.
        assert!(resolve_path("soc.f", cwd)
            .replace('\\', "/")
            .ends_with("/work/proj/soc.f"));
    }
}
