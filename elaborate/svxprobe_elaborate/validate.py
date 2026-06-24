"""Validate a Node-model JSON document against the schema.

Usable as a library (``validate_model``) and as a CLI:
    python -m svxprobe_elaborate.validate out.json
"""
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import jsonschema

_SCHEMA_PATH = Path(__file__).resolve().parent.parent / "schema" / "model.schema.json"


def load_schema() -> dict[str, Any]:
    with open(_SCHEMA_PATH) as f:
        return json.load(f)


def validate_model(model: dict[str, Any]) -> None:
    """Raise jsonschema.ValidationError if `model` does not conform."""
    jsonschema.validate(instance=model, schema=load_schema())


def _check_invariants(model: dict[str, Any]) -> list[str]:
    """Structural checks beyond the schema (referential integrity)."""
    errors: list[str] = []
    ids = {n["id"] for n in model["nodes"]}
    file_ids = {f["id"] for f in model["files"]}
    for n in model["nodes"]:
        if n["parent"] is not None and n["parent"] not in ids:
            errors.append(f"node {n['id']} has dangling parent {n['parent']}")
        for c in n["children"]:
            if c not in ids:
                errors.append(f"node {n['id']} has dangling child {c}")
        for key in ("def_range", "inst_range"):
            r = n.get(key)
            if r is not None and r["file"] not in file_ids:
                errors.append(f"node {n['id']} {key} references unknown file {r['file']}")
    return errors


def main(argv: list[str] | None = None) -> int:
    argv = argv if argv is not None else sys.argv[1:]
    if not argv:
        print("usage: python -m svxprobe_elaborate.validate <model.json>", file=sys.stderr)
        return 2
    with open(argv[0]) as f:
        model = json.load(f)
    validate_model(model)
    invariants = _check_invariants(model)
    if invariants:
        for e in invariants:
            print(f"invariant error: {e}", file=sys.stderr)
        return 1
    print(f"OK: {len(model['nodes'])} nodes valid against schema", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
