//! The committed golden fixture, embedded rather than resolved from a path.
//!
//! Three call sites used to hold a byte-identical `golden_path()` built from
//! `env!("CARGO_MANIFEST_DIR")` — which bakes the *build machine's* source path
//! into the binary. That is fine for `cargo run` and useless in a packaged app
//! (#240), where the directory does not exist on the user's machine.
//!
//! `golden` is the one basis that holds still: it needs no `SCALE_BENCH_MODEL`,
//! no network and no `--full` RAM headroom, so every metrics file ever produced
//! measures byte-identical input — which is what makes it the cross-run
//! comparison anchor `docs/benchmarking.md` §Findings leans on. Dropping it in
//! packaged builds would cost exactly that anchor, so it is embedded instead:
//! 2.1 MB of read-only data, behind the `golden` cargo feature for builds that
//! would rather not carry it.

use std::borrow::Cow;

/// The `picorv32_soc` golden hierarchy (2,117,239 bytes as committed).
///
/// `Cow` so callers can pass the bytes onward without copying them out of the
/// binary's read-only data — at 2.1 MB a copy would show up in the very peak-RSS
/// figure the scenario exists to measure. `None` when built without the `golden`
/// feature; callers treat that as "skip this basis" rather than an error.
///
/// Note the path depth: `include_bytes!` resolves relative to *this file*, not
/// to `CARGO_MANIFEST_DIR`, hence one `..` more than the old `env!` joins used.
pub fn golden_bytes() -> Option<Cow<'static, [u8]>> {
    #[cfg(feature = "golden")]
    {
        Some(Cow::Borrowed(include_bytes!(
            "../../../../fixtures/picorv32_soc/golden/hierarchy.json"
        )))
    }
    #[cfg(not(feature = "golden"))]
    {
        None
    }
}
