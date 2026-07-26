// Prevent an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Before anything can print: the subsystem attribute above leaves a release
    // build with no std handles, so `-h`, usage errors and `--bench` progress
    // would otherwise vanish when launched from a terminal (#240).
    hdl_schemview_app_lib::attach_parent_console();
    hdl_schemview_app_lib::run()
}
