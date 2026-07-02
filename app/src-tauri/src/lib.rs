//! Tauri shell for hdl-schemview: thin commands over [`svxprobe_gui::Session`].
//!
//! All logic lives in `svxprobe-gui` (CI-tested without a UI toolkit). These
//! commands just lock the session and forward calls; serialization is automatic.

use std::sync::Mutex;

use svxprobe_gui::{ProbeResponse, Session, TreeNode};
use svxprobe_schematic::SchematicGraph;
use svxprobe_wave::{TraceTimescale, ValueChange};
use tauri::State;

/// The loaded session (None until `load_design`).
#[derive(Default)]
struct AppState(Mutex<Option<Session>>);

type CmdResult<T> = Result<T, String>;

fn with_session<T>(
    state: &State<AppState>,
    f: impl FnOnce(&mut Session) -> CmdResult<T>,
) -> CmdResult<T> {
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no design loaded")?;
    f(session)
}

#[tauri::command]
fn load_design(
    state: State<AppState>,
    model: String,
    trace: String,
    excluded: Vec<String>,
    src_root: String,
) -> CmdResult<String> {
    let session = Session::load(&model, &trace, excluded, &src_root).map_err(|e| e.to_string())?;
    let top = session.design_top();
    *state.0.lock().map_err(|e| e.to_string())? = Some(session);
    Ok(top)
}

#[tauri::command]
fn scope_graph(state: State<AppState>, scope: String) -> CmdResult<SchematicGraph> {
    with_session(&state, |s| {
        s.scope_graph(&scope).ok_or_else(|| format!("scope not found: {scope}"))
    })
}

#[tauri::command]
fn expand_node(state: State<AppState>, node: u32) -> CmdResult<SchematicGraph> {
    with_session(&state, |s| s.expand(node).ok_or_else(|| "not expandable".into()))
}

#[tauri::command]
fn hierarchy_tree(state: State<AppState>, scope: String, depth: usize) -> CmdResult<TreeNode> {
    with_session(&state, |s| {
        s.hierarchy_tree(&scope, depth)
            .ok_or_else(|| format!("scope not found: {scope}"))
    })
}

#[tauri::command]
fn cone(state: State<AppState>, net: u32, dir: String, depth: usize) -> CmdResult<SchematicGraph> {
    with_session(&state, |s| Ok(s.cone(net, &dir, depth)))
}

#[tauri::command]
fn probe_signal(
    state: State<AppState>,
    full_name: String,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, |s| Ok(s.probe_signal(&full_name, context.as_deref())))
}

#[tauri::command]
fn probe_node(
    state: State<AppState>,
    path: String,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, |s| Ok(s.probe_node(&path, context.as_deref())))
}

#[tauri::command]
fn probe_source(
    state: State<AppState>,
    file: u32,
    offset: usize,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, |s| Ok(s.probe_source(file, offset, context.as_deref())))
}

#[tauri::command]
fn signal_values(state: State<AppState>, signal_ref: u32) -> CmdResult<Vec<ValueChange>> {
    with_session(&state, |s| Ok(s.signal_values(signal_ref)))
}

#[tauri::command]
fn source_text(state: State<AppState>, file: u32) -> CmdResult<String> {
    with_session(&state, |s| s.source_text(file).map_err(|e| e.to_string()))
}

#[tauri::command]
fn trace_timescale(state: State<AppState>) -> CmdResult<Option<TraceTimescale>> {
    with_session(&state, |s| Ok(s.trace_timescale()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            load_design,
            scope_graph,
            expand_node,
            hierarchy_tree,
            cone,
            probe_signal,
            probe_node,
            probe_source,
            signal_values,
            source_text,
            trace_timescale,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hdl-schemview");
}
