// Typed wrappers over the Tauri commands exposed by app/src-tauri.
//
// Multi-session (#168): every command accepts an optional trailing `sessionId`.
// Omitting it (all existing callers) targets the backend's "main" session, so
// single-session behavior is unchanged; an independent window passes its own id
// to load/query a separate design + trace. `undefined` is dropped from the invoke
// payload, so the Rust `Option<String>` resolves to the default.
import { invoke } from "@tauri-apps/api/core";
import type {
  ProbeResponse,
  Projection,
  SchematicGraph,
  SignalEntry,
  SourceFile,
  StartupArgs,
  TraceTimescale,
  TreeNode,
  ValueChange,
} from "./types";

export const api = {
  loadDesign: (
    model: string,
    trace: string,
    excluded: string[],
    srcRoot: string,
    sessionId?: string,
  ) => invoke<string>("load_design", { sessionId, model, trace, excluded, srcRoot }),
  elaborateAndLoad: (
    filelist: string,
    top: string,
    incdirs: string[],
    trace: string,
    excluded: string[],
    srcRoot: string,
    sessionId?: string,
    // Declared C/C++ sources / search roots (#222). Empty ⇒ the harness skips the
    // HLS provenance pass entirely, so a pure-RTL designlist is unaffected.
    hlsSrc: string[] = [],
  ) =>
    invoke<string>("elaborate_and_load", {
      sessionId,
      filelist,
      top,
      incdirs,
      trace,
      excluded,
      srcRoot,
      hlsSrc,
    }),
  // Swap a session's trace, keeping its design (#176) — far cheaper than a full
  // loadDesign/elaborateAndLoad, which re-ingests (or re-elaborates) an unchanged design.
  loadTrace: (trace: string, sessionId?: string) =>
    invoke<void>("load_trace", { sessionId, trace }),
  unloadDesign: (sessionId?: string) => invoke<void>("unload_design", { sessionId }),

  // `projection` (#157) picks the schematic granularity: "process-level" (default,
  // omit) keeps today's one-box-per-process view; "gate-level" dissolves each
  // combinational block into its gate/mux primitives. `undefined` is dropped from
  // the payload, so the Rust `Option<Projection>` resolves to ProcessLevel.
  scopeGraph: (scope: string, sessionId?: string, projection?: Projection) =>
    invoke<SchematicGraph>("scope_graph", { sessionId, scope, projection }),
  expandNode: (node: number, sessionId?: string, projection?: Projection) =>
    invoke<SchematicGraph>("expand_node", { sessionId, node, projection }),
  hierarchyTree: (scope: string, depth: number, sessionId?: string) =>
    invoke<TreeNode>("hierarchy_tree", { sessionId, scope, depth }),
  // The signals inside a scope, for a waveform pane's signal picker (#171).
  scopeSignals: (scope: string, sessionId?: string) =>
    invoke<SignalEntry[]>("scope_signals", { sessionId, scope }),
  cone: (net: number, dir: string, depth: number, sessionId?: string) =>
    invoke<SchematicGraph>("cone", { sessionId, net, dir, depth }),

  probeNode: (path: string, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_node", { sessionId, path, context }),
  probeSignal: (fullName: string, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_signal", { sessionId, fullName, context }),
  probeSource: (file: number, offset: number, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_source", { sessionId, file, offset, context }),

  signalValues: (signalRef: number, sessionId?: string) =>
    invoke<ValueChange[]>("signal_values", { sessionId, signalRef }),
  sourceText: (file: number, sessionId?: string) =>
    invoke<string>("source_text", { sessionId, file }),
  // Every source file + language, so the frontend can reveal a C/C++ pane (#159).
  sourceFiles: (sessionId?: string) =>
    invoke<SourceFile[]>("source_files", { sessionId }),
  traceTimescale: (sessionId?: string) =>
    invoke<TraceTimescale | null>("trace_timescale", { sessionId }),
  startupArgs: () => invoke<StartupArgs | null>("startup_args", {}),
};
