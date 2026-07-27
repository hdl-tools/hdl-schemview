//! Makes `SVX_BUILD_REV` a tracked input (#240 tier 1).
//!
//! `collect::build_provenance` reads it with `option_env!`, i.e. at *compile*
//! time, and CI restores a cached `target/` between runs. Without this cargo
//! would happily reuse an rlib built from an earlier commit, so the metrics
//! file's `build` row would name a revision it was **not** built from — a wrong
//! provenance claim, which is worse than the honest "no SVX_BUILD_REV" fallback.
fn main() {
    println!("cargo:rerun-if-env-changed=SVX_BUILD_REV");
}
