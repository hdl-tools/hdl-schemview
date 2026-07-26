//! One measured scenario per process — a shim over `scale_bench::scenario`.
//!
//! The logic moved into the lib for #240: a `[[bin]]` cannot be imported, and on
//! an isolated machine there is no `scenario` binary to spawn — the packaged app
//! runs the same code through its hidden `--bench-scenario` arm. Keeping this
//! bin a one-liner is what guarantees the two entry points share a single
//! contract (flags, the one-line JSON on stdout, exit 2 on a bad command line
//! and 1 on a failed run).
//!
//! See `scale_bench::scenario` for the modes and what each one measures.

fn main() {
    let code = scale_bench::scenario::main_with(std::env::args().skip(1));
    std::process::exit(code);
}
