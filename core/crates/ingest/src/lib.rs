//! Ingest the pyslang harness JSON into a [`svxprobe_model::Design`].
//!
//! Thin layer: deserialize the Node-model document and hand it to the model
//! crate to build indices. The JSON contract is `elaborate/schema/model.schema.json`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use memmap2::Mmap;
use rkyv::rancor::Error as RkyvError;
use svxprobe_model::{Design, Document, Node, NodeKind};

/// Bumped whenever the rkyv archive layout changes incompatibly; gates a cache
/// hit alongside the JSON `schema_version` (#21, ADR 0003).
pub const RKYV_FORMAT_VERSION: u32 = 1;

/// Hidden directory, beside the model JSON, holding derived rkyv load caches.
const CACHE_DIR: &str = ".schemview_data";

/// Parse a Node-model document from a byte slice and build a [`Design`].
pub fn from_slice(bytes: &[u8]) -> Result<Design> {
    let doc: Document = serde_json::from_slice(bytes).context("deserializing Node-model JSON")?;
    validate(&doc)?;
    Ok(Design::from_document(doc))
}

/// Read and parse a Node-model document from a file path, using a derived rkyv
/// cache beside the JSON when a fresh, version-matched one exists.
///
/// Cache path: `<model dir>/.schemview_data/<file name>.rkyv`. On a hit the
/// archive is mmap'd and deserialized, skipping the JSON parse + validation. On
/// a miss, a stale source, a version mismatch, or any corruption, the JSON is
/// parsed as before and the cache is (best-effort) rewritten.
pub fn from_path(path: impl AsRef<Path>) -> Result<Design> {
    let path = path.as_ref();
    if let Some(design) = try_load_cache(path) {
        return Ok(design);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("reading model file {}", path.display()))?;
    let doc: Document = serde_json::from_slice(&bytes).context("deserializing Node-model JSON")?;
    validate(&doc)?;
    // Best-effort: a cache-write failure (read-only dir, full disk) must not fail
    // the load — the JSON stays golden.
    let (doc, _) = write_cache(path, doc);
    Ok(Design::from_document(doc))
}

/// Parse + validate the JSON and (re)write the rkyv cache beside it, without
/// building a `Design`. Backs the `svxprobe cache` pre-warm subcommand (#21) so a
/// deploy can generate the archive ahead of a cache-less launch. Returns the
/// archive path.
pub fn build_cache(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).with_context(|| format!("reading model file {}", path.display()))?;
    let doc: Document = serde_json::from_slice(&bytes).context("deserializing Node-model JSON")?;
    validate(&doc)?;
    let (_, res) = write_cache(path, doc);
    res?;
    Ok(cache_path_for(path))
}

/// `<model dir>/.schemview_data/<model file name>.rkyv`.
fn cache_path_for(model: &Path) -> PathBuf {
    let dir = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = model
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".rkyv");
    dir.join(CACHE_DIR).join(name)
}

/// (byte length, mtime ns since the Unix epoch) of the source JSON — the
/// staleness key. Any error (missing file, pre-epoch mtime) means "no basis to
/// trust a cache", so the caller reparses.
fn source_meta(model: &Path) -> Result<(u64, u64)> {
    let md = fs::metadata(model)?;
    let mtime = md
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("source mtime precedes the Unix epoch: {e}"))?
        .as_nanos() as u64;
    Ok((md.len(), mtime))
}

/// Try to satisfy the load from a fresh, valid cache. Returns `None` (so the
/// caller falls back to JSON) on any miss: absent cache, unreadable source,
/// version mismatch, stale source, or a corrupt/invalid archive.
fn try_load_cache(model: &Path) -> Option<Design> {
    let (src_len, src_mtime_ns) = source_meta(model).ok()?;
    let cache = cache_path_for(model);
    let file = File::open(&cache).ok()?;
    // SAFETY: a read-only mmap of a regular file we just opened; the mapping is
    // used only within this function and dropped before it returns. A concurrent
    // truncation could yield garbage, but the validating `access` below rejects
    // it (→ `None` → JSON fallback), never UB in safe code paths.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let archived = rkyv::access::<ArchivedCacheArchive, RkyvError>(&mmap).ok()?;
    if archived.rkyv_format_version.to_native() != RKYV_FORMAT_VERSION
        || archived.schema_version.to_native() != 1
        || archived.src_len.to_native() != src_len
        || archived.src_mtime_ns.to_native() != src_mtime_ns
    {
        return None;
    }
    // Fresh + valid: the cache was validated when built, so skip the
    // referential-integrity pass; just materialize the doc and rebuild indices.
    let doc: Document = rkyv::deserialize::<Document, RkyvError>(&archived.doc).ok()?;
    Some(Design::from_document(doc))
}

/// Serialize `doc` (wrapped with the version + staleness header) to the cache
/// path, moving `doc` in and back out so no clone is needed. Returns `doc` plus
/// the write result (best-effort at the call site).
fn write_cache(model: &Path, doc: Document) -> (Document, Result<()>) {
    let (src_len, src_mtime_ns) = match source_meta(model) {
        Ok(m) => m,
        Err(e) => return (doc, Err(e)),
    };
    let archive = CacheArchive {
        rkyv_format_version: RKYV_FORMAT_VERSION,
        schema_version: doc.schema_version,
        src_len,
        src_mtime_ns,
        doc,
    };
    let res = serialize_and_write(&cache_path_for(model), &archive);
    (archive.doc, res)
}

fn serialize_and_write(cache: &Path, archive: &CacheArchive) -> Result<()> {
    let bytes = rkyv::to_bytes::<RkyvError>(archive).context("serializing rkyv cache")?;
    write_bytes_atomic(cache, &bytes)
}

/// Write `bytes` to `cache` via a temp file + rename, so a crash mid-write can't
/// leave a truncated archive resident (a later run would reject it, but this
/// avoids the churn). Shared by the model cache and the wave_index cache (#153).
fn write_bytes_atomic(cache: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = cache.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating cache dir {}", dir.display()))?;
    }
    let mut tmp = cache.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).with_context(|| format!("writing cache {}", tmp.display()))?;
    fs::rename(&tmp, cache).with_context(|| format!("finalizing cache {}", cache.display()))?;
    Ok(())
}

/// Self-describing cache: a version + staleness header, checked on the archived
/// view before the (large) `doc` is deserialized.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CacheArchive {
    rkyv_format_version: u32,
    schema_version: u32,
    src_len: u64,
    src_mtime_ns: u64,
    doc: Document,
}

// --- wave_index cache (#153, ADR 0003 Phase-A follow-up) ---
//
// The matcher's `wave_index` (trace signal ↔ model NodeId) is computation, not
// I/O, so the #21 model cache can't skip it. Persisting the resolved pairs makes
// that ~O(signals × path_len) pass a one-time cost. The pairs map *trace* var_refs
// to *model* NodeIds, so the cache is keyed on BOTH files' staleness plus a hash
// of the match options (a changed model can renumber node ids; changed exclusions
// change the mapping). It lives beside the model JSON, like the model cache.

/// Bumped whenever the wave_index archive layout changes incompatibly (#153);
/// independent of [`RKYV_FORMAT_VERSION`], which gates the model cache.
pub const WAVE_INDEX_FORMAT_VERSION: u32 = 1;

/// `<model dir>/.schemview_data/<model name>.<trace name>.waveidx.rkyv`. Keyed by
/// both file names so distinct model/trace pairs in one dir don't collide.
pub fn wave_index_cache_path(model: &Path, trace: &Path) -> PathBuf {
    let dir = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let model_name = model.file_name().unwrap_or_default().to_string_lossy();
    let trace_name = trace.file_name().unwrap_or_default().to_string_lossy();
    dir.join(CACHE_DIR)
        .join(format!("{model_name}.{trace_name}.waveidx.rkyv"))
}

/// Try to load a fresh, version-matched `wave_index` for `(model, trace, opts_hash)`.
/// Returns the persisted `(NodeId, var_ref)` pairs, or `None` on any miss: absent
/// cache, unreadable source, version mismatch, either file stale, a different
/// `opts_hash`, or a corrupt/invalid archive. `None` means "re-run the matcher".
pub fn try_load_wave_index(model: &Path, trace: &Path, opts_hash: u64) -> Option<Vec<(u32, u64)>> {
    let (model_len, model_mtime_ns) = source_meta(model).ok()?;
    let (trace_len, trace_mtime_ns) = source_meta(trace).ok()?;
    let cache = wave_index_cache_path(model, trace);
    let file = File::open(&cache).ok()?;
    // SAFETY: read-only mmap of a regular file we just opened, used only within
    // this function; the validating `access` below rejects a concurrently
    // truncated mapping (→ `None` → re-match) rather than risking UB.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let archived = rkyv::access::<ArchivedWaveIndexArchive, RkyvError>(&mmap).ok()?;
    if archived.wave_index_format_version.to_native() != WAVE_INDEX_FORMAT_VERSION
        || archived.model_len.to_native() != model_len
        || archived.model_mtime_ns.to_native() != model_mtime_ns
        || archived.trace_len.to_native() != trace_len
        || archived.trace_mtime_ns.to_native() != trace_mtime_ns
        || archived.opts_hash.to_native() != opts_hash
    {
        return None;
    }
    rkyv::deserialize::<Vec<(u32, u64)>, RkyvError>(&archived.pairs).ok()
}

/// Persist the resolved `wave_index` pairs for `(model, trace, opts_hash)`. Called
/// best-effort at the match site — a write failure must not fail the load.
pub fn write_wave_index(
    model: &Path,
    trace: &Path,
    opts_hash: u64,
    pairs: &[(u32, u64)],
) -> Result<()> {
    let (model_len, model_mtime_ns) = source_meta(model)?;
    let (trace_len, trace_mtime_ns) = source_meta(trace)?;
    let archive = WaveIndexArchive {
        wave_index_format_version: WAVE_INDEX_FORMAT_VERSION,
        model_len,
        model_mtime_ns,
        trace_len,
        trace_mtime_ns,
        opts_hash,
        pairs: pairs.to_vec(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&archive).context("serializing wave_index cache")?;
    write_bytes_atomic(&wave_index_cache_path(model, trace), &bytes)
}

/// Version + dual-staleness header for the wave_index cache, checked on the
/// archived view before the `pairs` are deserialized.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct WaveIndexArchive {
    wave_index_format_version: u32,
    model_len: u64,
    model_mtime_ns: u64,
    trace_len: u64,
    trace_mtime_ns: u64,
    opts_hash: u64,
    pairs: Vec<(u32, u64)>,
}

/// Cheap referential-integrity checks beyond serde's structural parse.
fn validate(doc: &Document) -> Result<()> {
    anyhow::ensure!(
        doc.schema_version == 1,
        "unsupported schema_version {}",
        doc.schema_version
    );
    let n = doc.nodes.len() as u32;
    for node in &doc.nodes {
        if let Some(p) = node.parent {
            anyhow::ensure!(p < n, "node {} has out-of-range parent {}", node.id, p);
        }
        for &c in &node.children {
            anyhow::ensure!(c < n, "node {} has out-of-range child {}", node.id, c);
        }
    }
    for e in &doc.edges {
        anyhow::ensure!(
            e.port < n && e.endpoint < n,
            "edge {} references out-of-range node",
            e.id
        );
    }
    check_name_uniqueness(doc)?;
    Ok(())
}

/// Enforce that, within a scope, a `name` identifies a single logical object.
///
/// Cross-probe and the schematic resolve names to objects, so an accidental
/// name overlap can silently resolve to the wrong node. The one legitimate
/// same-name case is the **port/backing-net dual-node pattern** — a `Port` and
/// its same-path `Net`/`Var`, i.e. the equivalence set `nodes_at_path` returns —
/// which is whitelisted. Only real SV declarations (`Instance`/`Net`/`Port`/
/// `Var`) are checked; synthetic logic nodes (`Ff`/`Comb`/`Latch`/`Assign`) and
/// unnamed generate blocks carry generic, path-distinguished labels rather than
/// identifiers, so they are exempt.
fn check_name_uniqueness(doc: &Document) -> Result<()> {
    let is_decl = |k: NodeKind| {
        matches!(
            k,
            NodeKind::Instance | NodeKind::Net | NodeKind::Port | NodeKind::Var
        )
    };
    // scope (parent) -> name -> the declarations carrying that name.
    let mut scopes: HashMap<Option<u32>, HashMap<&str, Vec<&Node>>> = HashMap::new();
    for node in &doc.nodes {
        if is_decl(node.kind) && !node.name.is_empty() {
            scopes
                .entry(node.parent)
                .or_default()
                .entry(node.name.as_str())
                .or_default()
                .push(node);
        }
    }
    for names in scopes.values() {
        for (name, members) in names {
            if members.len() < 2 {
                continue;
            }
            // The dual-node pattern: every member is the same object (one path)
            // and a signal kind (Port + its backing Net/Var). Anything else — a
            // structural kind in the set, or more than one distinct path — is an
            // ambiguous name and rejected.
            let same_path = members.iter().all(|m| m.path == members[0].path);
            let all_signal = members
                .iter()
                .all(|m| matches!(m.kind, NodeKind::Port | NodeKind::Net | NodeKind::Var));
            anyhow::ensure!(
                same_path && all_signal,
                "name collision in scope: '{}' maps to {} distinct objects [{}]",
                name,
                members.len(),
                members
                    .iter()
                    .map(|m| format!("{:?} {}", m.kind, m.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
        "schema_version": 1,
        "design": "t",
        "files": [{"id": 0, "path": "t.sv"}],
        "nodes": [
            {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
             "children":[1],"symbol_key":"t"},
            {"id":1,"kind":"Var","name":"a","path":"t.a","parent":0,
             "children":[],"symbol_key":"t.a","type":"logic"}
        ]
    }"#;

    #[test]
    fn parses_and_indexes() {
        let d = from_slice(DOC.as_bytes()).unwrap();
        assert_eq!(d.nodes().len(), 2);
        assert_eq!(d.nodes_at_path("t.a"), &[1]);
    }

    #[test]
    fn rejects_bad_refs() {
        let bad = DOC.replace("\"children\":[1]", "\"children\":[99]");
        assert!(from_slice(bad.as_bytes()).is_err());
    }

    // A Port and its same-path backing Net/Var is the legitimate dual-node pattern
    // (the cross-probe equivalence set), not a collision — it must be accepted.
    const DUAL_NODE: &str = r#"{
        "schema_version": 1,
        "design": "t",
        "files": [{"id": 0, "path": "t.sv"}],
        "nodes": [
            {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
             "children":[1,2],"symbol_key":"t"},
            {"id":1,"kind":"Port","name":"clk","path":"t.clk","parent":0,
             "children":[],"symbol_key":"t.clk","dir":"in"},
            {"id":2,"kind":"Net","name":"clk","path":"t.clk","parent":0,
             "children":[],"symbol_key":"t.clk#net"}
        ]
    }"#;

    #[test]
    fn accepts_port_backing_net_dual_node() {
        assert!(from_slice(DUAL_NODE.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_same_name_distinct_objects() {
        // Two sibling Vars with the same name but different paths: the name no
        // longer identifies one object, so name-based lookups could mis-resolve.
        let bad = r#"{
            "schema_version": 1,
            "design": "t",
            "files": [{"id": 0, "path": "t.sv"}],
            "nodes": [
                {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
                 "children":[1,2],"symbol_key":"t"},
                {"id":1,"kind":"Var","name":"a","path":"t.a","parent":0,
                 "children":[],"symbol_key":"t.a"},
                {"id":2,"kind":"Var","name":"a","path":"t.a$dup","parent":0,
                 "children":[],"symbol_key":"t.a$dup"}
            ]
        }"#;
        assert!(from_slice(bad.as_bytes()).is_err());
    }

    #[test]
    fn rejects_instance_vs_signal_name_collision() {
        // An Instance sharing a name with a same-scope net is a cross-kind clash:
        // the structural box and the signal would be indistinguishable by name.
        let bad = r#"{
            "schema_version": 1,
            "design": "t",
            "files": [{"id": 0, "path": "t.sv"}],
            "nodes": [
                {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
                 "children":[1,2],"symbol_key":"t"},
                {"id":1,"kind":"Instance","name":"m","path":"t.m","parent":0,
                 "children":[],"symbol_key":"t.m"},
                {"id":2,"kind":"Net","name":"m","path":"t.m","parent":0,
                 "children":[],"symbol_key":"t.m#net"}
            ]
        }"#;
        assert!(from_slice(bad.as_bytes()).is_err());
    }

    #[test]
    fn golden_modport_membership_resolves() {
        // Every Modport node in the fixture carries its membership (#95), and
        // each member resolves to a real signal in the parent interface
        // instance via `Design::modport_member_nodes` — the lookup the
        // schematic's modport rendering builds on.
        let golden = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/picorv32_soc/golden/hierarchy.json");
        let d = from_slice(&std::fs::read(golden).unwrap()).unwrap();
        let modports: Vec<_> = d
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Modport)
            .collect();
        assert!(!modports.is_empty(), "fixture has modport nodes");
        for mp in modports {
            let members = mp
                .members
                .as_ref()
                .unwrap_or_else(|| panic!("modport {} carries members", mp.path));
            assert!(!members.is_empty(), "modport {} has members", mp.path);
            for m in members {
                assert!(
                    !d.modport_member_nodes(mp.id, &m.name).is_empty(),
                    "member {}.{} resolves to a signal node",
                    mp.path,
                    m.name
                );
            }
        }
    }

    #[test]
    fn golden_fixture_passes_name_uniqueness() {
        // The committed fixture exercises the dual-node pattern heavily (every port
        // has a backing net/var) plus path-distinguished synthetic logic nodes;
        // it must still load cleanly under the uniqueness check.
        let golden = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/picorv32_soc/golden/hierarchy.json");
        assert!(from_slice(&std::fs::read(golden).unwrap()).is_ok());
    }

    // --- rkyv derived cache (#21) ---

    /// A fresh, isolated scratch dir under the system temp dir for a cache test.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("svxprobe_ingest_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cache_miss_writes_archive_then_hits() {
        let dir = scratch("hit");
        let model = dir.join("hierarchy.json");
        std::fs::write(&model, DOC).unwrap();
        let cache = dir.join(".schemview_data").join("hierarchy.json.rkyv");

        // First load misses → parses JSON and writes the archive beside it.
        let d1 = from_path(&model).unwrap();
        assert!(cache.exists(), "cache archive written on miss");
        assert_eq!(d1.nodes().len(), 2);
        assert_eq!(d1.nodes_at_path("t.a"), &[1]);

        // Second load: the archive is fresh → served from cache, identical design.
        let d2 = from_path(&model).unwrap();
        assert_eq!(d2.nodes().len(), 2);
        assert_eq!(d2.nodes_at_path("t.a"), &[1]);
    }

    #[test]
    fn corrupt_cache_falls_back_to_json() {
        let dir = scratch("corrupt");
        let model = dir.join("hierarchy.json");
        std::fs::write(&model, DOC).unwrap();

        // Build a valid cache, then clobber it with garbage.
        let cache = build_cache(&model).unwrap();
        std::fs::write(&cache, b"not a valid rkyv archive at all").unwrap();

        // Load still succeeds via the JSON — corruption is a miss, never an error.
        let d = from_path(&model).unwrap();
        assert_eq!(d.nodes().len(), 2);
    }

    #[test]
    fn stale_source_forces_rebuild() {
        let dir = scratch("stale");
        let model = dir.join("hierarchy.json");
        std::fs::write(&model, DOC).unwrap();
        let d1 = from_path(&model).unwrap();
        assert_eq!(d1.nodes().len(), 2);

        // Rewrite the JSON with a different design of a different byte length; the
        // src_len mismatch invalidates the cache, so the reload reflects the new
        // content instead of the archived one.
        let three = r#"{
            "schema_version": 1,
            "design": "t",
            "files": [{"id": 0, "path": "t.sv"}],
            "nodes": [
                {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
                 "children":[1,2],"symbol_key":"t"},
                {"id":1,"kind":"Var","name":"a","path":"t.a","parent":0,
                 "children":[],"symbol_key":"t.a","type":"logic"},
                {"id":2,"kind":"Var","name":"b","path":"t.b","parent":0,
                 "children":[],"symbol_key":"t.b","type":"logic"}
            ]
        }"#;
        assert_ne!(three.len(), DOC.len(), "new content differs in length");
        std::fs::write(&model, three).unwrap();
        let d2 = from_path(&model).unwrap();
        assert_eq!(d2.nodes().len(), 3, "stale cache rebuilt from new JSON");
        assert_eq!(d2.nodes_at_path("t.b"), &[2]);
    }

    // --- wave_index cache (#153) ---

    /// Write a model + a stand-in trace file (the cache only stats the trace) and
    /// return their paths.
    fn model_and_trace(dir: &Path) -> (PathBuf, PathBuf) {
        let model = dir.join("hierarchy.json");
        std::fs::write(&model, DOC).unwrap();
        let trace = dir.join("dump.vcd");
        std::fs::write(&trace, b"$version fake $end").unwrap();
        (model, trace)
    }

    #[test]
    fn wave_index_cache_miss_then_hit() {
        let dir = scratch("wi_hit");
        let (model, trace) = model_and_trace(&dir);
        let pairs = vec![(1u32, 10u64), (0u32, 20u64)];

        // Fresh: nothing written yet.
        assert!(try_load_wave_index(&model, &trace, 42).is_none());

        write_wave_index(&model, &trace, 42, &pairs).unwrap();

        // Hit: identical pairs come back.
        let got = try_load_wave_index(&model, &trace, 42).expect("cache hit");
        assert_eq!(got, pairs);
    }

    #[test]
    fn wave_index_cache_stale_on_opts_change() {
        let dir = scratch("wi_opts");
        let (model, trace) = model_and_trace(&dir);
        write_wave_index(&model, &trace, 1, &[(0u32, 7u64)]).unwrap();
        // A different match-options hash must not be served the old pairs.
        assert!(try_load_wave_index(&model, &trace, 2).is_none());
    }

    #[test]
    fn wave_index_cache_stale_on_model_change() {
        let dir = scratch("wi_model");
        let (model, trace) = model_and_trace(&dir);
        write_wave_index(&model, &trace, 1, &[(0u32, 7u64)]).unwrap();

        // Rewriting the model (different length ⇒ node ids may renumber) must bust
        // the wave_index cache, since it maps var_refs to model NodeIds.
        std::fs::write(&model, format!("{DOC}   ")).unwrap();
        assert!(try_load_wave_index(&model, &trace, 1).is_none());
    }

    #[test]
    fn wave_index_cache_stale_on_trace_change() {
        let dir = scratch("wi_trace");
        let (model, trace) = model_and_trace(&dir);
        write_wave_index(&model, &trace, 1, &[(0u32, 7u64)]).unwrap();

        std::fs::write(&trace, b"$version different $end padding").unwrap();
        assert!(try_load_wave_index(&model, &trace, 1).is_none());
    }

    #[test]
    fn wave_index_cache_corrupt_is_miss() {
        let dir = scratch("wi_corrupt");
        let (model, trace) = model_and_trace(&dir);
        write_wave_index(&model, &trace, 1, &[(0u32, 7u64)]).unwrap();

        let cache = wave_index_cache_path(&model, &trace);
        std::fs::write(&cache, b"garbage, not an rkyv archive").unwrap();
        // Corruption is a miss, never a panic or error.
        assert!(try_load_wave_index(&model, &trace, 1).is_none());
    }

    #[test]
    fn cache_loaded_document_matches_json_parsed() {
        // #154: a cache hit must reproduce the JSON-parsed Document field-for-field,
        // or a warm launch would silently diverge from a cold parse. Compared via
        // serde_json::Value so HashMap key ordering (e.g. `enums`) doesn't matter.
        let dir = scratch("fidelity");
        let model = dir.join("hierarchy.json");
        let golden = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/picorv32_soc/golden/hierarchy.json");
        let bytes = std::fs::read(&golden).unwrap();
        std::fs::write(&model, &bytes).unwrap();

        // Cold JSON parse vs. warm rkyv-cache load of the same source.
        let from_json = from_slice(&bytes).unwrap();
        build_cache(&model).unwrap();
        let from_cache = from_path(&model).unwrap();

        let v_json = serde_json::to_value(&from_json.doc).unwrap();
        let v_cache = serde_json::to_value(&from_cache.doc).unwrap();
        assert_eq!(
            v_json, v_cache,
            "cache-loaded Document diverges from the JSON parse"
        );
    }
}
