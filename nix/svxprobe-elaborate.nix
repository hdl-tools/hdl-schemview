# The elaboration harness: pyslang -> Node-model JSON.
#
# `src` is rooted at the *repo* root, not at elaborate/, and `sourceRoot` walks
# back down. That is deliberate: three test modules locate the fixture tree as
#
#     REPO = Path(__file__).resolve().parents[2]   # elaborate/tests/x.py -> repo
#     RTL  = REPO / "fixtures" / "picorv32_soc" / "rtl"
#
# (elaborate/tests/test_elaborate.py:11, test_gate_level.py:576, test_lint.py:14 —
# about 29 of the 97 tests). Unpacking the repo root preserves that relative depth,
# so the suite runs unmodified. Patching the tests to take a fixture path from the
# environment would work too, but it would weaken the invariant they encode: the
# harness lives two levels under a repo that has fixtures/.
{
  lib,
  buildPythonApplication,
  hatchling,
  jsonschema,
  pytestCheckHook,
  pyslang,
  src,
}:

buildPythonApplication {
  pname = "svxprobe-elaborate";
  version = "0.0.0"; # elaborate/pyproject.toml [project].version
  pyproject = true;

  inherit src;
  sourceRoot = "source/elaborate";

  build-system = [ hatchling ];

  dependencies = [
    pyslang
    jsonschema
  ];

  # elaborate/pyproject.toml keeps its dev tools in PEP 735 `[dependency-groups]`,
  # which is not an extra — neither pip nor hatchling surfaces it — so pytest is
  # named here rather than derived from the manifest.
  nativeCheckInputs = [ pytestCheckHook ];

  # Guards the #306 fix against regressing. `validate.py` used to resolve the
  # schema as a sibling of the package directory, which only ever worked from a
  # source checkout; run it from somewhere else entirely so a relapse cannot pass.
  postCheck = ''
    ( cd "$TMPDIR" && python -c 'from svxprobe_elaborate.validate import load_schema; load_schema()' )
  '';

  meta = {
    description = "hdl-schemview elaboration harness: pyslang -> Node-model JSON";
    homepage = "https://github.com/hdl-tools/hdl-schemview";
    license = lib.licenses.mit;
    mainProgram = "svxprobe-elaborate";
  };
}
