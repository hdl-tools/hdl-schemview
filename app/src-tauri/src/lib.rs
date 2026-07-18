//! Tauri shell for hdl-schemview: thin commands over [`svxprobe_gui::Session`].
//!
//! All logic lives in `svxprobe-gui` (CI-tested without a UI toolkit). These
//! commands just lock the session and forward calls; serialization is automatic.
//!
//! **Multi-session (#168):** the shell keys sessions by id in a map so that
//! independent windows (e.g. waveform panes each loading their own trace, #170)
//! can hold different designs/traces at once. Every command takes an optional
//! `session_id`; omitting it (the default for existing frontend calls) targets the
//! `"main"` session, so single-session behavior is unchanged. Windows create their
//! own session by passing a distinct id to `load_design`/`elaborate_and_load` and
//! drop it via `unload_design` on close.

use std::collections::HashMap;
use std::sync::Mutex;

use svxprobe_gui::{
    ProbeResponse, Projection, Session, SignalEntry, StartupArgs, StartupError, TreeNode,
};
use svxprobe_schematic::SchematicGraph;
use svxprobe_wave::{TraceTimescale, ValueChange};
use tauri::State;

/// The session id used when a command omits `session_id` — the main window's
/// design. Keeps existing single-session callers working untouched (#168).
const DEFAULT_SESSION: &str = "main";

/// Loaded sessions keyed by id (empty until the first `load_design`). Each entry
/// is one loaded design + trace; independent windows own separate entries (#168).
#[derive(Default)]
struct AppState(Mutex<HashMap<String, Session>>);

/// CLI launch arguments parsed before the window opened (#136); `None` for a
/// normal no-argument launch. The frontend pulls this once via `startup_args`.
struct StartupState(Option<StartupArgs>);

type CmdResult<T> = Result<T, String>;

/// Resolve `session_id` (defaulting to [`DEFAULT_SESSION`]), look up its loaded
/// session, and run `f` against it. Errors loudly if that session isn't loaded.
fn with_session<T>(
    state: &State<AppState>,
    session_id: Option<String>,
    f: impl FnOnce(&mut Session) -> CmdResult<T>,
) -> CmdResult<T> {
    let id = session_id.unwrap_or_else(|| DEFAULT_SESSION.to_string());
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let session = guard
        .get_mut(&id)
        .ok_or_else(|| format!("no design loaded for session '{id}'"))?;
    f(session)
}

/// Insert a freshly-loaded session under its id (default `"main"`) and return the
/// design's top scope. Replaces any existing session with that id.
fn store_session(
    state: &State<AppState>,
    session_id: Option<String>,
    session: Session,
) -> CmdResult<String> {
    let id = session_id.unwrap_or_else(|| DEFAULT_SESSION.to_string());
    let top = session.design_top();
    state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id, session);
    Ok(top)
}

#[tauri::command]
fn load_design(
    state: State<AppState>,
    session_id: Option<String>,
    model: String,
    trace: String,
    excluded: Vec<String>,
    src_root: String,
) -> CmdResult<String> {
    let session = Session::load(&model, &trace, excluded, &src_root).map_err(|e| e.to_string())?;
    store_session(&state, session_id, session)
}

/// Designlist flow (#93): elaborate a `.f` filelist with the external pyslang
/// harness (`svxprobe-elaborate` on PATH), then load the produced model.
/// `async` so the multi-second subprocess runs off the main thread and the
/// window keeps painting (Tauri executes non-async commands on the UI thread).
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri command mirrors the elaborate flow's inputs + session_id
async fn elaborate_and_load(
    state: State<'_, AppState>,
    session_id: Option<String>,
    filelist: String,
    top: String,
    incdirs: Vec<String>,
    trace: String,
    excluded: Vec<String>,
    src_root: String,
) -> CmdResult<String> {
    let session =
        Session::elaborate_and_load(&filelist, &top, &incdirs, &trace, excluded, &src_root)
            .map_err(|e| e.to_string())?;
    store_session(&state, session_id, session)
}

/// Swap a session's trace, reusing its already-ingested design (#176). Backs a waveform
/// pane's "Load trace…" (#170): the design is unchanged, so this skips the model
/// re-ingest — and, for a designlist, the whole re-elaboration — that a full
/// `load_design`/`elaborate_and_load` would pay.
#[tauri::command]
fn load_trace(state: State<AppState>, session_id: Option<String>, trace: String) -> CmdResult<()> {
    with_session(&state, session_id, |s| {
        s.load_trace(&trace).map_err(|e| e.to_string())
    })
}

/// Drop a session (window closed, #168). Idempotent — unloading an unknown id is a
/// no-op, so a double close or a never-loaded pane can't error.
#[tauri::command]
fn unload_design(state: State<AppState>, session_id: Option<String>) -> CmdResult<()> {
    let id = session_id.unwrap_or_else(|| DEFAULT_SESSION.to_string());
    state.0.lock().map_err(|e| e.to_string())?.remove(&id);
    Ok(())
}

#[tauri::command]
fn scope_graph(
    state: State<AppState>,
    session_id: Option<String>,
    scope: String,
    projection: Option<Projection>,
) -> CmdResult<SchematicGraph> {
    let projection = projection.unwrap_or_default();
    with_session(&state, session_id, |s| {
        s.scope_graph_with(&scope, projection)
            .ok_or_else(|| format!("scope not found: {scope}"))
    })
}

#[tauri::command]
fn expand_node(
    state: State<AppState>,
    session_id: Option<String>,
    node: u32,
    projection: Option<Projection>,
) -> CmdResult<SchematicGraph> {
    let projection = projection.unwrap_or_default();
    with_session(&state, session_id, |s| {
        s.expand_with(node, projection)
            .ok_or_else(|| "not expandable".into())
    })
}

#[tauri::command]
fn hierarchy_tree(
    state: State<AppState>,
    session_id: Option<String>,
    scope: String,
    depth: usize,
) -> CmdResult<TreeNode> {
    with_session(&state, session_id, |s| {
        s.hierarchy_tree(&scope, depth)
            .ok_or_else(|| format!("scope not found: {scope}"))
    })
}

/// The signals inside a scope, for a waveform pane's signal picker (#171). Paired
/// with `hierarchy_tree`: that lists the scopes, this lists what is in one. Scoped
/// to `session_id`, so each independent waveform pane answers from its own trace.
#[tauri::command]
fn scope_signals(
    state: State<AppState>,
    session_id: Option<String>,
    scope: String,
) -> CmdResult<Vec<SignalEntry>> {
    with_session(&state, session_id, |s| {
        s.scope_signals(&scope)
            .ok_or_else(|| format!("scope not found: {scope}"))
    })
}

#[tauri::command]
fn cone(
    state: State<AppState>,
    session_id: Option<String>,
    net: u32,
    dir: String,
    depth: usize,
) -> CmdResult<SchematicGraph> {
    with_session(&state, session_id, |s| Ok(s.cone(net, &dir, depth)))
}

#[tauri::command]
fn probe_signal(
    state: State<AppState>,
    session_id: Option<String>,
    full_name: String,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, session_id, |s| {
        Ok(s.probe_signal(&full_name, context.as_deref()))
    })
}

#[tauri::command]
fn probe_node(
    state: State<AppState>,
    session_id: Option<String>,
    path: String,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, session_id, |s| {
        Ok(s.probe_node(&path, context.as_deref()))
    })
}

#[tauri::command]
fn probe_source(
    state: State<AppState>,
    session_id: Option<String>,
    file: u32,
    offset: usize,
    context: Option<String>,
) -> CmdResult<Option<ProbeResponse>> {
    with_session(&state, session_id, |s| {
        Ok(s.probe_source(file, offset, context.as_deref()))
    })
}

#[tauri::command]
fn signal_values(
    state: State<AppState>,
    session_id: Option<String>,
    signal_ref: u32,
) -> CmdResult<Vec<ValueChange>> {
    with_session(&state, session_id, |s| Ok(s.signal_values(signal_ref)))
}

#[tauri::command]
fn source_text(state: State<AppState>, session_id: Option<String>, file: u32) -> CmdResult<String> {
    with_session(&state, session_id, |s| {
        s.source_text(file).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn trace_timescale(
    state: State<AppState>,
    session_id: Option<String>,
) -> CmdResult<Option<TraceTimescale>> {
    with_session(&state, session_id, |s| Ok(s.trace_timescale()))
}

/// The launch arguments parsed from argv (#136), or `null` for a normal launch.
/// The frontend queries this once at init: if present it prefills the load form
/// and auto-loads, byte-identical to clicking **Load**.
#[tauri::command]
fn startup_args(startup: State<StartupState>) -> Option<StartupArgs> {
    startup.0.clone()
}

/// Parse (and path-resolve) the CLI launch arguments before any window opens
/// (#136). Exits the process directly on the terminal cases, EDA-tool style:
/// `-h`/`--help` prints usage to stdout and exits 0; a usage error prints to
/// stderr and exits 2; a missing filelist/trace prints to stderr and exits 1.
/// A normal no-argument launch returns `StartupState(None)`.
fn resolve_startup() -> StartupState {
    use svxprobe_gui::startup::{self, USAGE};
    // `args_os` + lossy conversion, not `args()` — the latter panics on a
    // non-UTF-8 argument, which in a release (windows_subsystem) build would
    // make the app vanish with no console output, the worst mode for a CLI.
    let argv = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    let parsed = match startup::parse(argv) {
        Ok(None) => return StartupState(None),
        Ok(Some(a)) => a,
        Err(StartupError::Help) => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Err(StartupError::Usage(msg)) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    // Resolve relative paths against the directory the user launched from —
    // INIT_CWD under `npm run tauri dev` (the tauri CLI runs the binary from
    // src-tauri), else the process cwd for a bundled binary — so a relative
    // `-f`/`-trace` means the same thing in both, and fail fast on a bad path.
    let cwd = startup::invocation_dir(std::env::var_os("INIT_CWD"), std::env::current_dir().ok());
    match parsed.resolve(&cwd) {
        Ok(a) => StartupState(Some(a)),
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Native file picker for a waveform pane's "Load trace…" (#170).
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(resolve_startup())
        .invoke_handler(tauri::generate_handler![
            load_design,
            elaborate_and_load,
            load_trace,
            unload_design,
            scope_graph,
            expand_node,
            hierarchy_tree,
            scope_signals,
            cone,
            probe_signal,
            probe_node,
            probe_source,
            signal_values,
            source_text,
            trace_timescale,
            startup_args,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hdl-schemview");
}
