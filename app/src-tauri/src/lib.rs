//! Tauri shell for hdl-schemview: thin commands over [`svxprobe_gui::Session`].
//!
//! All logic lives in `svxprobe-gui` (CI-tested without a UI toolkit). These
//! commands just lock the session and forward calls; serialization is automatic.

use std::sync::Mutex;

use svxprobe_gui::{ProbeResponse, Session, StartupArgs, StartupError, TreeNode};
use svxprobe_schematic::SchematicGraph;
use svxprobe_wave::{TraceTimescale, ValueChange};
use tauri::State;

/// The loaded session (None until `load_design`).
#[derive(Default)]
struct AppState(Mutex<Option<Session>>);

/// CLI launch arguments parsed before the window opened (#136); `None` for a
/// normal no-argument launch. The frontend pulls this once via `startup_args`.
struct StartupState(Option<StartupArgs>);

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

/// Designlist flow (#93): elaborate a `.f` filelist with the external pyslang
/// harness (`svxprobe-elaborate` on PATH), then load the produced model.
/// `async` so the multi-second subprocess runs off the main thread and the
/// window keeps painting (Tauri executes non-async commands on the UI thread).
#[tauri::command]
async fn elaborate_and_load(
    state: State<'_, AppState>,
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
    let cwd = startup::invocation_dir(
        std::env::var_os("INIT_CWD"),
        std::env::current_dir().ok(),
    );
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
        .manage(AppState::default())
        .manage(resolve_startup())
        .invoke_handler(tauri::generate_handler![
            load_design,
            elaborate_and_load,
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
            startup_args,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hdl-schemview");
}
