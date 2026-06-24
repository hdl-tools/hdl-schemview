#!/usr/bin/env bash
# Verify a fixture's committed traces are reproducible from source.
#
# Regenerates the traces with Verilator and checks the result is STRUCTURALLY
# equivalent to the committed traces (same scope/var counts via `svxprobe wave`).
# Structural (not byte) equality is used because trace bytes can vary across
# Verilator patch versions; for byte-reproducibility use the pinned Verilator in
# flake.nix. The committed traces are restored at the end, so the working tree is
# left unchanged.
#
# Usage:  fixtures/verify_reproducible.sh [fixture_name]   (default: picorv32_soc)
set -euo pipefail

FIX="${1:-picorv32_soc}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRACES="$ROOT/fixtures/$FIX/traces"
CORE="$ROOT/core"

BACKUP="$(mktemp -d)"
trap 'rm -rf "$BACKUP"' EXIT
cp "$TRACES"/* "$BACKUP"/

counts() {
  cargo run -q --manifest-path "$CORE/Cargo.toml" --bin svxprobe -- wave "$1" \
    | grep -E '^(scopes|vars):'
}

# Snapshot committed structure before regen overwrites the files.
declare -A COMMITTED
for ext in fst vcd; do
  COMMITTED[$ext]="$(counts "$BACKUP/$FIX.$ext")"
done

bash "$ROOT/fixtures/regen.sh" "$FIX" >/dev/null

rc=0
for ext in fst vcd; do
  new="$(counts "$TRACES/$FIX.$ext")"
  if [ "$new" != "${COMMITTED[$ext]}" ]; then
    echo "MISMATCH ($ext):"
    echo "  committed:   ${COMMITTED[$ext]//$'\n'/ }"
    echo "  regenerated: ${new//$'\n'/ }"
    rc=1
  else
    echo "OK $ext: ${new//$'\n'/ }"
  fi
done

# Restore committed traces so the working tree is unchanged.
cp "$BACKUP"/* "$TRACES"/

[ "$rc" -eq 0 ] && echo "reproducible (structural)"
exit "$rc"
