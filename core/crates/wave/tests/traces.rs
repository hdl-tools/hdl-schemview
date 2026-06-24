//! Load the committed tier-1 traces (both FST and VCD) via wellen.

use std::path::PathBuf;

use svxprobe_wave::LoadedWave;

fn trace(ext: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!(
            "../../../fixtures/picorv32_soc/traces/picorv32_soc.{ext}"
        ))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn loads_fst() {
    let w = LoadedWave::open(&trace("fst")).expect("open fst");
    let s = w.summary();
    assert!(s.scopes > 0 && s.vars > 0, "fst summary empty: {s:?}");
    // The generate-array scope naming the matcher must handle is present.
    assert!(
        w.var_full_names().iter().any(|n| n.contains("g_lane")),
        "no g_lane signals in fst"
    );
}

#[test]
fn loads_vcd() {
    let w = LoadedWave::open(&trace("vcd")).expect("open vcd");
    let s = w.summary();
    assert!(s.scopes > 0 && s.vars > 0, "vcd summary empty: {s:?}");
}

#[test]
fn fst_and_vcd_agree() {
    // The two traces come from the same deterministic run, so their hierarchies
    // must match in size.
    let fst = LoadedWave::open(&trace("fst")).expect("open fst");
    let vcd = LoadedWave::open(&trace("vcd")).expect("open vcd");
    assert_eq!(
        fst.var_count(),
        vcd.var_count(),
        "var count differs across formats"
    );
    assert_eq!(
        fst.scope_count(),
        vcd.scope_count(),
        "scope count differs across formats"
    );
}
