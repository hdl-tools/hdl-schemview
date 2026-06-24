#!/usr/bin/env bash
# Regenerate the frozen Verilator traces (VCD + FST) for a fixture.
#
# Produces BOTH formats from the SAME deterministic testbench by verilating
# twice (--trace -> VCD, --trace-fst -> FST). Identical stimulus makes the two
# traces equivalent, so the matcher is exercised against both from the start.
#
# Usage:  fixtures/regen.sh [fixture_name]   (default: picorv32_soc)
#
# Requires Verilator 5.x. A pinned Verilator is provided via flake.nix
# (`nix develop`); the apt package (5.020) is the verified fallback.
set -euo pipefail

FIX="${1:-picorv32_soc}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIR="$HERE/$FIX"
BUILD="$DIR/build"
TRACES="$DIR/traces"

RTL=(
  "$DIR/rtl/picorv32.v"
  "$DIR/rtl/soc_pkg.sv"
  "$DIR/rtl/mem_if.sv"
  "$DIR/rtl/soc_mem.sv"
  "$DIR/rtl/picorv32_soc.sv"
)
TB="$DIR/tb/tb_picorv32_soc.sv"

rm -rf "$BUILD"
mkdir -p "$BUILD" "$TRACES"

echo ">> generating firmware"
python3 "$DIR/tb/gen_firmware.py" -o "$BUILD/firmware.hex"

VFLAGS=(
  --binary --timing
  --top-module tb
  --timescale 1ns/1ps
  --trace-structs --trace-params --trace-max-array 1024
  -Wno-fatal
  -j 0
)

build_one() {
  local fmt="$1" traceflag="$2"
  local odir="$BUILD/obj_${fmt}"
  echo ">> verilating ($fmt)"
  verilator "${VFLAGS[@]}" "$traceflag" --Mdir "$odir" "${RTL[@]}" "$TB"
  echo ">> simulating ($fmt)"
  ( cd "$BUILD" && "$odir/Vtb" "+dumpfile=${FIX}.${fmt}" )
  mv "$BUILD/${FIX}.${fmt}" "$TRACES/"
  echo ">> wrote $TRACES/${FIX}.${fmt}"
}

build_one vcd "--trace"
build_one fst "--trace-fst"

echo ">> done"
ls -l "$TRACES"
