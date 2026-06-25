//! Ingest the pyslang harness JSON into a [`svxprobe_model::Design`].
//!
//! Thin layer: deserialize the Node-model document and hand it to the model
//! crate to build indices. The JSON contract is `elaborate/schema/model.schema.json`.

use std::path::Path;

use anyhow::{Context, Result};
use svxprobe_model::{Design, Document};

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
}
