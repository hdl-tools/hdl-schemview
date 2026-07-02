//! Ingest the pyslang harness JSON into a [`svxprobe_model::Design`].
//!
//! Thin layer: deserialize the Node-model document and hand it to the model
//! crate to build indices. The JSON contract is `elaborate/schema/model.schema.json`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use svxprobe_model::{Design, Document, Node, NodeKind};

/// Parse a Node-model document from a byte slice and build a [`Design`].
pub fn from_slice(bytes: &[u8]) -> Result<Design> {
    let doc: Document = serde_json::from_slice(bytes).context("deserializing Node-model JSON")?;
    validate(&doc)?;
    Ok(Design::from_document(doc))
}

/// Read and parse a Node-model document from a file path.
pub fn from_path(path: impl AsRef<Path>) -> Result<Design> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).with_context(|| format!("reading model file {}", path.display()))?;
    from_slice(&bytes)
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
        let d = from_path(golden).unwrap();
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
        assert!(from_path(golden).is_ok());
    }
}
