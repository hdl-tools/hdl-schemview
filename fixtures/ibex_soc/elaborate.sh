#!/usr/bin/env bash
# Tier-2 stress: elaborate the real core with the pyslang harness and validate
# the emitted Node model against the schema. This exercises the elaboration
# spine on a large, heavily-parameterized production core (Ibex) — far past the
# tier-1 fixture's size.
#
# Status: EXPERIMENTAL. Ibex elaboration needs the right file list + include
# dirs + lowRISC prim dependencies; wiring that up is the first tier-2 hardening
# task (see ../../docs/fixtures.md). The nightly job runs this with
# continue-on-error until it is green.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
IBEX="$HERE/build/ibex"

if [ ! -d "$IBEX" ]; then
  echo "run fetch.sh first" >&2
  exit 1
fi

# Core RTL + packages. Ibex keeps synthesizable RTL under rtl/; packages must
# precede their users for elaboration.
mapfile -t SOURCES < <(
  ls "$IBEX"/rtl/*_pkg.sv 2>/dev/null
  ls "$IBEX"/rtl/*.sv 2>/dev/null | grep -v '_pkg\.sv$'
)

if [ "${#SOURCES[@]}" -eq 0 ]; then
  echo "no Ibex RTL found under $IBEX/rtl" >&2
  exit 1
fi

INCLUDES=()
for d in "$IBEX/rtl" "$IBEX/vendor/lowrisc_ip/ip/prim/rtl"; do
  [ -d "$d" ] && INCLUDES+=("-I" "$d")
done

OUT="$HERE/build/ibex_hierarchy.json"
uv run --project "$ROOT/elaborate" svxprobe-elaborate \
  --top ibex_top "${INCLUDES[@]}" "${SOURCES[@]}" -o "$OUT"
uv run --project "$ROOT/elaborate" python -m svxprobe_elaborate.validate "$OUT"
echo ">> tier-2 elaboration OK: $OUT"
