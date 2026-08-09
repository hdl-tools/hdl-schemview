//! The GUI session logic, exercised end-to-end on the fixture (no UI toolkit).

use std::path::{Path, PathBuf};

use svxprobe_gui::{ConeLimits, Projection, SchematicGraph, Session, SignalEntry, TraceStepReq};
use svxprobe_model::{Dir, NodeKind};

fn names(sigs: &[SignalEntry]) -> Vec<&str> {
    sigs.iter().map(|e| e.name.as_str()).collect()
}

/// A projection-independent fingerprint of a graph: its boxes (id + label,
/// order-independent) and its edge count. Two graphs with the same shape render
/// the same schematic.
fn shape(g: &SchematicGraph) -> (Vec<(u32, String)>, usize) {
    let mut nodes: Vec<(u32, String)> = g.nodes.iter().map(|n| (n.id, n.label.clone())).collect();
    nodes.sort();
    (nodes, g.edges.len())
}

fn entry<'a>(sigs: &'a [SignalEntry], name: &str) -> &'a SignalEntry {
    sigs.iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no `{name}` in {:?}", names(sigs)))
}

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn session() -> Session {
    Session::load(
        &fixture("golden/hierarchy.json"),
        &fixture("traces/picorv32_soc.fst"),
        vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        repo_root(),
    )
    .unwrap()
}

#[test]
fn scope_and_expand() {
    let s = session();
    assert_eq!(s.design_top(), "picorv32_soc");
    let g = s.scope_graph("picorv32_soc.g_lane[0]").unwrap();
    assert!(g.nodes.iter().any(|n| n.label == "core"));
    assert!(!g.edges.is_empty());
}

#[test]
fn scope_graph_projection_threads_through() {
    let s = session();
    let scope = "picorv32_soc.g_lane[0]";

    // The bare `scope_graph` delegates with `ProcessLevel` — same graph.
    let base = s.scope_graph(scope).expect("base graph");
    let process = s
        .scope_graph_with(scope, Projection::ProcessLevel)
        .expect("process-level graph");
    assert_eq!(shape(&base), shape(&process), "bare call == ProcessLevel");

    // The committed fixture carries no gate primitives (the harness was not run
    // with `--gate-level`), so gate level is inert here: identical structure.
    let gate = s
        .scope_graph_with(scope, Projection::GateLevel)
        .expect("gate-level graph");
    assert_eq!(shape(&base), shape(&gate), "no gate prims ⇒ identical");
}

#[test]
fn expand_projection_threads_through() {
    let s = session();
    let g = s.scope_graph("picorv32_soc.g_lane[0]").unwrap();
    let inst = g
        .nodes
        .iter()
        .find(|n| n.expandable)
        .expect("an expandable box to drill");

    let base = s.expand(inst.id).expect("base expand");
    let process = s
        .expand_with(inst.id, Projection::ProcessLevel)
        .expect("process-level expand");
    assert_eq!(shape(&base), shape(&process), "bare call == ProcessLevel");

    let gate = s
        .expand_with(inst.id, Projection::GateLevel)
        .expect("gate-level expand");
    assert_eq!(shape(&base), shape(&gate), "no gate prims ⇒ identical");
}

const TRACE_NET: &str = "picorv32_soc.g_lane[0].core.mem_valid";

fn req(path: &str, dir: Dir, depth: Option<usize>) -> TraceStepReq {
    TraceStepReq {
        path: path.to_string(),
        dir,
        depth,
        fanout: None,
    }
}

#[test]
fn trace_graph_resolves_seeds_by_path() {
    // The session layer's whole job here: the frontend holds paths (that is what
    // a pin, a wire and a pop-out's localStorage snapshot carry), never NodeIds.
    let s = session();
    let g = s
        .trace_graph(
            &[req(TRACE_NET, Dir::Inout, None)],
            ConeLimits::default(),
            Projection::ProcessLevel,
        )
        .expect("a real net resolves");
    assert!(!g.nodes.is_empty(), "the seed net reaches something");
    assert!(!g.root.is_empty(), "a trace binds a breadcrumb scope");
}

#[test]
fn trace_graph_rejects_an_unknown_seed() {
    // Loudly, not by quietly returning a smaller graph: a step the user asked for
    // and did not get is a bug report, not a rendering decision.
    let s = session();
    let err = s
        .trace_graph(
            &[req("picorv32_soc.no_such_net", Dir::Inout, None)],
            ConeLimits::default(),
            Projection::ProcessLevel,
        )
        .expect_err("an unknown path must fail");
    assert!(
        err.to_string().contains("picorv32_soc.no_such_net"),
        "the message must name the path: {err}"
    );
}

#[test]
fn trace_graph_defaults_to_one_hop_and_clamps_to_the_budget() {
    let s = session();
    let limits = ConeLimits::default();
    let one = s
        .trace_graph(
            &[req(TRACE_NET, Dir::Inout, None)],
            limits,
            Projection::ProcessLevel,
        )
        .unwrap();
    let explicit = s
        .trace_graph(
            &[req(TRACE_NET, Dir::Inout, Some(1))],
            limits,
            Projection::ProcessLevel,
        )
        .unwrap();
    assert_eq!(shape(&one), shape(&explicit), "omitted depth means one hop");

    // A step may not outrun the global cap, so asking for far more than
    // `limits.depth` gives the same graph as asking for exactly it.
    let capped = ConeLimits {
        depth: 2,
        ..ConeLimits::default()
    };
    let huge = s
        .trace_graph(
            &[req(TRACE_NET, Dir::Inout, Some(99))],
            capped,
            Projection::ProcessLevel,
        )
        .unwrap();
    let exact = s
        .trace_graph(
            &[req(TRACE_NET, Dir::Inout, Some(2))],
            capped,
            Projection::ProcessLevel,
        )
        .unwrap();
    assert_eq!(shape(&huge), shape(&exact), "depth clamps to limits.depth");
}

#[test]
fn trace_graph_projection_threads_through() {
    // Unlike the two tests above, this one asserts the projections *differ*. Those
    // walk `g_lane[0]`, whose own children are instances; a trace crosses into
    // `core`, where the committed golden really does carry gate primitives (it is
    // elaborated with `--gate-level` since #199). So "identical either way" would
    // pass here only if the parameter never reached the extractor.
    let s = session();
    let steps = [req(TRACE_NET, Dir::Inout, Some(2))];
    let limits = ConeLimits::default();
    let process = s
        .trace_graph(&steps, limits, Projection::ProcessLevel)
        .unwrap();
    let gate = s
        .trace_graph(&steps, limits, Projection::GateLevel)
        .unwrap();
    assert!(!process.nodes.is_empty(), "vacuous otherwise");
    assert_ne!(
        shape(&process),
        shape(&gate),
        "gate level must dissolve the combinational blocks a trace reaches"
    );
}

#[test]
fn trace_graph_accumulates_across_steps() {
    let s = session();
    let limits = ConeLimits::default();
    let proj = Projection::ProcessLevel;
    let one = s
        .trace_graph(&[req(TRACE_NET, Dir::In, Some(1))], limits, proj)
        .unwrap();
    let two = s
        .trace_graph(
            &[
                req(TRACE_NET, Dir::In, Some(1)),
                req(TRACE_NET, Dir::Out, Some(1)),
            ],
            limits,
            proj,
        )
        .unwrap();
    assert!(
        two.edges.len() > one.edges.len(),
        "the second step must add connectivity: {} vs {}",
        two.edges.len(),
        one.edges.len()
    );
}

#[test]
fn probe_signal_links_all_views() {
    let mut s = session();
    let r = s
        .probe_signal("TOP.tb.dut.g_lane[0].bus.valid", None)
        .expect("resolves");
    assert_eq!(r.anchor.path, "picorv32_soc.g_lane[0].bus.valid");
    // Source view target present.
    let src = r.source.expect("has source");
    assert!(src.path.ends_with("mem_if.sv"));
    // Waveform target present and loadable.
    assert!(r.wave.in_trace);
    let changes = s.signal_values(r.wave.signal_ref);
    assert!(changes.len() > 2, "bus.valid should toggle");
}

#[test]
fn name_refs_feeds_the_source_pane() {
    let s = session();
    // The committed golden is elaborated with --name-refs, so its files carry spans.
    let all: Vec<_> = s
        .source_files()
        .into_iter()
        .flat_map(|f| s.name_refs(f.id))
        .collect();
    assert!(
        !all.is_empty(),
        "golden carries identifier-occurrence spans"
    );
    // Every DTO is renderable: a nonempty class + a positive span length.
    assert!(all.iter().all(|r| !r.cls.is_empty() && r.len > 0));
    // The source pane colors at least these; the classes come from the model, kebab-cased.
    assert!(all.iter().any(|r| r.cls == "signal"), "signals classified");
    assert!(all.iter().any(|r| r.cls == "param"), "params classified");
}

#[test]
fn probe_source_picker_with_context() {
    let mut s = session();
    // lane_state decl is shared across lanes (offset 1028 in file 0).
    let no_ctx = s.probe_source(0, 1028, None).expect("resolves");
    assert_eq!(no_ctx.alternatives.len(), 1, "picker offers the other lane");

    let ctx = s
        .probe_source(0, 1028, Some("picorv32_soc.g_lane[1]"))
        .expect("resolves");
    assert_eq!(ctx.anchor.path, "picorv32_soc.g_lane[1].lane_state");
}

#[test]
fn not_in_trace_is_explicit() {
    let mut s = session();
    let r = s.probe_node("picorv32_soc.g_lane[0].core", None).unwrap();
    assert!(!r.wave.in_trace, "an instance has no waveform");
}

// #176: swap only the trace, reusing the already-ingested design — the waveform
// panes' "Load trace…" path (#170), which otherwise re-ingests / re-elaborates a
// design that did not change.
#[test]
fn load_trace_reuses_the_design_and_queries_the_new_trace() {
    let mut s = session(); // FST
    let top = s.design_top();

    s.load_trace(&fixture("traces/picorv32_soc.vcd"))
        .expect("swaps to the VCD");

    // The design is reused, not reloaded: same top, scopes still queryable.
    assert_eq!(s.design_top(), top);
    assert!(s.scope_graph("picorv32_soc.g_lane[0]").is_some());
    // The signal re-resolves against the VCD and carries that trace's values.
    let r = s
        .probe_signal("TOP.tb.dut.g_lane[0].bus.valid", None)
        .expect("resolves in the VCD");
    assert!(r.wave.in_trace);
    assert!(
        s.signal_values(r.wave.signal_ref).len() > 2,
        "bus.valid toggles"
    );
}

// Re-matching is the dominant cost of a trace change, so a swap must keep the #153
// wave_index cache alive: the new trace's index is persisted for the next load of
// that (model, trace) pair, exactly as a fresh `Session::load` would.
#[test]
fn load_trace_persists_the_wave_index_cache() {
    let model = fixture("golden/hierarchy.json");
    let vcd = fixture("traces/picorv32_soc.vcd");
    let cache = svxprobe_ingest::wave_index_cache_path(Path::new(&model), Path::new(&vcd));
    let _ = std::fs::remove_file(&cache); // start from a cold cache

    let mut s = session(); // FST
    s.load_trace(&vcd).expect("swaps to the VCD");

    assert!(
        cache.exists(),
        "swapping to a trace should persist its wave_index for the next load (#153)"
    );
}

// A bad pick in a waveform pane's "Load trace…" dialog must not cost the user their
// loaded design: the trace is opened before any state changes, so a failed swap
// leaves the session serving exactly what it had.
#[test]
fn load_trace_failure_leaves_the_session_intact() {
    let mut s = session(); // FST
    assert!(s.load_trace(&fixture("traces/no-such-trace.fst")).is_err());

    assert_eq!(s.design_top(), "picorv32_soc");
    let r = s
        .probe_signal("TOP.tb.dut.g_lane[0].bus.valid", None)
        .expect("still resolves against the original trace");
    assert!(r.wave.in_trace);
    assert!(
        s.signal_values(r.wave.signal_ref).len() > 2,
        "the original trace's values are still loadable"
    );
}

// --- scope_signals: the waveform pane's signal picker (#171) ---
//
// The tree lists a design's *scopes* (`hierarchy_tree`); `scope_signals` lists what is
// *inside* one, so a waveform pane can pick lanes without borrowing another window's
// tree. Every assertion below exists to keep the picker from becoming a second,
// disagreeing source of truth alongside the cross-probe.

// The picker lists a scope's own signal declarations and nothing else: not its
// elaboration-time `Param`s (never traceable), not the `Interface` bundle or child
// `Instance`s (those are the *tree's* rows), not a synthesized `FF` (no declaration).
#[test]
fn scope_signals_lists_only_the_scope_s_own_signals() {
    let s = session();
    let sigs = s.scope_signals("picorv32_soc.g_lane[0]").expect("a scope");
    // The lane declares exactly one signal; `gi`/`bus`/`$ff27`/`core`/`memory` are all
    // other kinds of thing.
    assert_eq!(names(&sigs), ["lane_state"]);
}

// Every port in this design is *two* nodes at one canonical path — the `Port` and its
// backing `Var` (the pattern `ingest`'s name-uniqueness check whitelists). One signal
// must mean one row, and that row must name the node a click actually selects:
// `CrossProbe::best_kind` ranks Var < Net < Port, so the representative is the `Var`.
#[test]
fn scope_signals_collapses_the_port_backing_net_dual_node() {
    let s = session();
    let sigs = s.scope_signals("picorv32_soc").expect("the top scope");
    for port in ["clk", "resetn", "core_trap"] {
        let hits: Vec<_> = sigs.iter().filter(|e| e.name == port).collect();
        assert_eq!(hits.len(), 1, "`{port}` is one signal, not two rows");
        assert_eq!(
            hits[0].kind,
            NodeKind::Var,
            "`{port}` must report the kind `probe_node` anchors on"
        );
    }
}

// The anti-divergence gate: a row's `in_trace` and the click that follows it must be
// the same answer. They can only disagree if the picker re-derives the trace link
// instead of asking the cross-probe — e.g. testing `wave_index.signal_of(child_id)`
// on a dual-node `Port` whose backing `Var` is the matched node.
// This would pass trivially if `in_trace` were hardcoded `true`. It isn't testable
// otherwise here: the fixture's Verilator trace dumps *every* design signal (all 477
// resolve, matching `wave_index`'s linked count exactly), so no scope in it can produce
// a dimmed row. `not_in_trace_is_explicit` pins the `false` arm on the shared
// `to_wave` path this delegates to; the picker's dimmed rendering is a manual check.
#[test]
fn scope_signals_in_trace_agrees_with_probe_node() {
    let mut s = session();
    let mut checked = 0;
    for scope in [
        "picorv32_soc.g_lane[0].bus",
        "picorv32_soc.g_lane[0].memory",
    ] {
        for e in s.scope_signals(scope).expect("a scope") {
            let r = s
                .probe_node(&e.path, None)
                .unwrap_or_else(|| panic!("`{}` must resolve", e.path));
            assert_eq!(
                r.wave.in_trace, e.in_trace,
                "`{}`: picker says in_trace={}, probe_node says {}",
                e.path, e.in_trace, r.wave.in_trace
            );
            assert_eq!(r.anchor.path, e.path, "the row and the click name one node");
            checked += 1;
        }
    }
    // Anti-vacuity: an empty loop would assert nothing at all.
    assert!(checked >= 10, "only checked {checked} rows");
}

// Widths come from the schematic's own `pin_width`, so a picker row and a schematic pin
// annotate one signal identically — including the enum fallback, where the range comes
// from the model's normalized enum table rather than the declared type.
#[test]
fn scope_signals_reports_widths_like_schematic_pins() {
    let s = session();
    let bus = s.scope_signals("picorv32_soc.g_lane[0].bus").unwrap();
    assert_eq!(entry(&bus, "addr").width.as_deref(), Some("[31:0]"));
    assert_eq!(entry(&bus, "valid").width, None, "a scalar has no range");

    let lane = s.scope_signals("picorv32_soc.g_lane[0]").unwrap();
    assert_eq!(
        entry(&lane, "lane_state").width.as_deref(),
        Some("[1:0]"),
        "`lane_state_e` has no packed range; its width is an enum-table fact"
    );

    // A memory's row reports its *element* width, like the MEMORY glyph's pins (#112).
    let mem = s.scope_signals("picorv32_soc.g_lane[0].memory").unwrap();
    let ram = entry(&mem, "ram");
    assert_eq!(ram.kind, NodeKind::Memory);
    assert_eq!(ram.width.as_deref(), Some("[31:0]"));
}

// A bare interface bundle is a tree scope (#97), so the picker must answer for it: its
// members are ordinary signals. Its `Modport` views are not — they are views, and the
// tree drills into them.
#[test]
fn scope_signals_of_an_interface_bundle_lists_its_members() {
    let s = session();
    let sigs = s.scope_signals("picorv32_soc.g_lane[0].bus").unwrap();
    // Declaration order, matching the source — `clk`'s dual node comes before `valid`.
    assert_eq!(
        names(&sigs),
        ["clk", "valid", "instr", "ready", "addr", "wdata", "wstrb", "rdata"]
    );
}

// The shell 404s a path that names no scope, exactly as `hierarchy_tree` does. A signal
// is not a scope: the picker lists what is *in* a scope, and a signal contains nothing.
#[test]
fn scope_signals_rejects_non_scopes() {
    let s = session();
    assert!(s.scope_signals("nope").is_none(), "unknown path");
    assert!(
        s.scope_signals("picorv32_soc.clk").is_none(),
        "a signal is not a scope"
    );
}

// Every row the tree renders must open in the picker — the two are one UI, and a tree
// row the picker can't answer for is a dead click. Mirrors `hierarchy_tree`'s own
// "every node is a valid schematic root" contract.
#[test]
fn scope_signals_covers_every_tree_row() {
    let s = session();
    let lane = s.hierarchy_tree("picorv32_soc.g_lane[0]", 1).unwrap();
    for c in &lane.children {
        assert!(
            s.scope_signals(&c.path).is_some(),
            "tree row `{}` must open in the picker",
            c.path
        );
    }
    // `core.genblk1` holds only logic (assigns), so it is not a navigable scope (#184)
    // and never appears as a picker row; addressing it directly resolves nothing,
    // exactly as `hierarchy_tree`/`scope_graph` reject it.
    assert!(s
        .scope_signals("picorv32_soc.g_lane[0].core.genblk1")
        .is_none());
}

// A pane's "Load trace…" (#170/#176) swaps the trace under an unchanged design, so the
// picker's tree stands but its `in_trace` flags move. They must keep agreeing with the
// cross-probe against the *new* trace.
#[test]
fn scope_signals_reflects_a_trace_swap() {
    let mut s = session(); // FST
    let before = s.scope_signals("picorv32_soc.g_lane[0].bus").unwrap();

    s.load_trace(&fixture("traces/picorv32_soc.vcd"))
        .expect("swaps to the VCD");

    let after = s.scope_signals("picorv32_soc.g_lane[0].bus").unwrap();
    // The design didn't change, so the rows themselves don't.
    assert_eq!(names(&before), names(&after), "same design, same signals");
    for e in &after {
        let r = s.probe_node(&e.path, None).expect("resolves");
        assert_eq!(
            r.wave.in_trace, e.in_trace,
            "`{}` must agree with the VCD, not the FST",
            e.path
        );
    }
}

#[test]
fn source_text_loads() {
    let s = session();
    let g = s.scope_graph("picorv32_soc.g_lane[0]").unwrap();
    // file 0 is the wrapper; ensure we can read some source.
    let text = s.source_text(0).unwrap();
    assert!(text.contains("module"));
    let _ = g;
}

#[test]
fn hierarchy_tree_is_lazy_and_navigable() {
    let s = session();

    // depth 1: the top plus its direct structural children. A generate-for's
    // array container (`g_lane`) is hoisted away (#107) — the per-iteration
    // scopes are the tree levels, mirroring the schematic's GenBlock dissolve
    // but stopping at the iterations. Grandchildren are not fetched but
    // flagged expandable so the frontend loads them lazily.
    let root = s.hierarchy_tree("picorv32_soc", 1).expect("tree root");
    assert_eq!(root.label, "picorv32_soc");
    assert_eq!(root.path, "picorv32_soc");
    assert!(root.expandable);
    let labels: Vec<&str> = root.children.iter().map(|c| c.label.as_str()).collect();
    assert!(!labels.contains(&"g_lane"), "container hoisted: {labels:?}");
    assert!(labels.contains(&"g_lane[1]"), "children: {labels:?}");
    let lane0 = root
        .children
        .iter()
        .find(|c| c.label == "g_lane[0]")
        .expect("iteration child");
    assert!(lane0.expandable, "iteration has instances below");
    assert!(lane0.children.is_empty(), "depth-1 stops here (lazy)");

    // depth 2 reaches the iterations' instances in one call.
    let deep = s.hierarchy_tree("picorv32_soc", 2).expect("tree root");
    let lane0 = deep
        .children
        .iter()
        .find(|c| c.label == "g_lane[0]")
        .unwrap();
    assert!(
        lane0.children.iter().any(|c| c.label == "core"),
        "instances: {:?}",
        lane0.children.iter().map(|c| &c.label).collect::<Vec<_>>()
    );

    // Expanding a child = re-querying with its path (what the frontend does).
    let lane = s
        .hierarchy_tree("picorv32_soc.g_lane[0]", 1)
        .expect("lane subtree");
    let labels: Vec<&str> = lane.children.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"core"), "children: {labels:?}");
    assert!(labels.contains(&"memory"), "children: {labels:?}");
    // A bare interface bundle is a navigable scope now (#97) — it drills into
    // its modport views — so the tree lists it alongside the instances.
    assert!(labels.contains(&"bus"), "children: {labels:?}");
    let core = lane.children.iter().find(|c| c.label == "core").unwrap();
    assert_eq!(core.module.as_deref(), Some("picorv32"), "module sublabel");

    // Every tree node's path opens as a schematic scope.
    for c in &lane.children {
        assert!(s.scope_graph(&c.path).is_some(), "{} is navigable", c.path);
    }

    // Unknown scopes yield None; a bare interface bundle resolves (#97).
    assert!(s.hierarchy_tree("nope", 1).is_none());
    assert!(s.hierarchy_tree("picorv32_soc.g_lane[0].bus", 1).is_some());
}

// A tree row is identified by its path, and the row map in `tree.ts` is path-keyed, so
// rows that repeat a path are dead clicks — only the last of them can ever highlight
// (#178). g_lane[0] carries several real rows (core/memory/bus); their paths must be
// distinct.
#[test]
fn tree_rows_never_repeat_a_path() {
    let s = session();
    let core = s
        .hierarchy_tree("picorv32_soc.g_lane[0]", 1)
        .expect("lane subtree");

    let paths: Vec<&str> = core.children.iter().map(|c| c.path.as_str()).collect();
    let mut uniq = paths.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), paths.len(), "duplicate tree rows: {paths:?}");
}

// A generate block that holds only logic (`genblk1/2/3` — assigns and a comb, no
// instance or interface) is a syntactic wrapper, not a navigable design scope (#184).
// core (picorv32) is all logic, so after pruning it is a leaf: no genblk row appears,
// and addressing one directly resolves nothing — the frontend then walks up to the
// enclosing module, where the logic already renders dissolved.
#[test]
fn logic_only_generate_blocks_are_pruned_from_the_tree() {
    let s = session();
    let core = s
        .hierarchy_tree("picorv32_soc.g_lane[0].core", 1)
        .expect("core subtree");

    let genblks: Vec<&str> = core
        .children
        .iter()
        .map(|c| c.label.as_str())
        .filter(|l| l.starts_with("genblk"))
        .collect();
    assert!(
        genblks.is_empty(),
        "logic-only genblk rows leaked: {genblks:?}"
    );

    assert!(
        s.hierarchy_tree("picorv32_soc.g_lane[0].core.genblk3", 1)
            .is_none(),
        "a logic-only genblk must not resolve as a tree scope"
    );
    assert!(
        s.scope_graph("picorv32_soc.g_lane[0].core.genblk3")
            .is_none(),
        "a logic-only genblk must not resolve as a schematic scope"
    );
}

// The prune is contents-based, not keyword-based: a generate block that holds
// instances stays navigable. g_lane[0] is itself a GenBlock (a generate-for iteration)
// but holds core/memory/bus — the tool's most useful view. #184 must not touch it.
#[test]
fn instance_bearing_generate_blocks_stay_navigable() {
    let s = session();
    let top = s.hierarchy_tree("picorv32_soc", 1).unwrap();
    assert!(
        top.children.iter().any(|c| c.label == "g_lane[0]"),
        "instance-bearing generate iteration must stay a tree row: {:?}",
        top.children.iter().map(|c| &c.label).collect::<Vec<_>>()
    );
    assert!(s.scope_graph("picorv32_soc.g_lane[0]").is_some());
}

#[test]
fn elaborate_and_load_runs_the_harness() {
    // Requires `svxprobe-elaborate` on PATH — the app's runtime contract for
    // designlist loading (#93). Skip when absent (e.g. the Rust CI job carries
    // no Python env); everything past the subprocess boundary is the same
    // ingest/load path the other tests cover via Session::load.
    let available = std::process::Command::new("svxprobe-elaborate")
        .arg("--help")
        .output()
        .is_ok_and(|o| o.status.success());
    if !available {
        eprintln!("skipping: svxprobe-elaborate not on PATH");
        return;
    }
    let s = Session::elaborate_and_load(
        &fixture("picorv32_soc.f"),
        "picorv32_soc",
        &[],
        &fixture("traces/picorv32_soc.fst"),
        vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        repo_root(),
        &[], // no declared C sources (#222) ⇒ no --hls-map, pure-RTL path unchanged
    )
    .unwrap();
    assert_eq!(s.design_top(), "picorv32_soc");
    assert!(s.scope_graph("picorv32_soc.g_lane[0]").is_some());

    // A bad top must surface the harness error, not a silent empty design.
    let err = Session::elaborate_and_load(
        &fixture("picorv32_soc.f"),
        "no_such_top",
        &[],
        &fixture("traces/picorv32_soc.fst"),
        vec![],
        repo_root(),
        &[],
    );
    assert!(err.is_err(), "unknown top should fail loudly");
}
