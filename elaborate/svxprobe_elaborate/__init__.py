"""hdl-schemview elaboration harness.

Walks a slang-elaborated SystemVerilog design (via pyslang) and serializes the
structural spine of the hierarchy to the Node-model JSON consumed by the Rust
core. See ``elaborate/schema/model.schema.json`` for the contract.

Import submodules directly, e.g. ``from svxprobe_elaborate.elaborate import
build_model`` (kept lazy here to avoid a runpy double-import warning under
``python -m svxprobe_elaborate.elaborate``).
"""

__all__ = ["Elaborator", "build_model", "main"]
