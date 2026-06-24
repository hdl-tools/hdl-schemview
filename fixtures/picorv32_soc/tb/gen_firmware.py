#!/usr/bin/env python3
"""Generate firmware.hex for the picorv32_soc fixture.

A tiny self-contained RV32I assembler for the handful of instructions the
fixture program needs -- avoids a riscv-gcc dependency and keeps the firmware
deterministic and reproducible from source. Output is $readmemh format: one
32-bit hex word per line, in instruction-memory order.

Program (an endless accumulate-and-store loop; the testbench runs a fixed
number of cycles, so a clean halt is unnecessary):

    addi x1, x0, 1        # x1 = 1   (increment)
    addi x2, x0, 0        # x2 = 0   (accumulator)
    addi x3, x0, 0x100    # x3 = data address (word index 0x40)
  loop:
    add  x2, x2, x1       # x2 += x1
    addi x1, x1, 1        # x1 += 1
    sw   x2, 0(x3)        # mem[x3] = x2
    jal  x0, loop         # repeat
"""
from __future__ import annotations

import argparse


def addi(rd: int, rs1: int, imm: int) -> int:
    imm &= 0xFFF
    return (imm << 20) | (rs1 << 15) | (0b000 << 12) | (rd << 7) | 0x13


def add(rd: int, rs1: int, rs2: int) -> int:
    return (0 << 25) | (rs2 << 20) | (rs1 << 15) | (0b000 << 12) | (rd << 7) | 0x33


def sw(rs2: int, rs1: int, imm: int) -> int:
    imm &= 0xFFF
    imm_hi = (imm >> 5) & 0x7F
    imm_lo = imm & 0x1F
    return (imm_hi << 25) | (rs2 << 20) | (rs1 << 15) | (0b010 << 12) | (imm_lo << 7) | 0x23


def jal(rd: int, offset: int) -> int:
    # offset is a byte offset relative to this instruction (bit 0 always 0).
    off = offset & 0x1FFFFF
    imm20 = (off >> 20) & 0x1
    imm10_1 = (off >> 1) & 0x3FF
    imm11 = (off >> 11) & 0x1
    imm19_12 = (off >> 12) & 0xFF
    imm = (imm20 << 19) | (imm19_12 << 0) | (imm11 << 8) | (imm10_1 << 9)
    return (imm << 12) | (rd << 7) | 0x6F


def build() -> list[int]:
    LOOP = 0x0C  # byte address of the `loop:` label
    JAL_AT = 0x18  # byte address of the `jal` instruction
    return [
        addi(1, 0, 1),
        addi(2, 0, 0),
        addi(3, 0, 0x100),
        add(2, 2, 1),
        addi(1, 1, 1),
        sw(2, 3, 0),
        jal(0, LOOP - JAL_AT),
    ]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default="firmware.hex")
    args = ap.parse_args()
    words = build()
    with open(args.out, "w") as f:
        for w in words:
            f.write(f"{w & 0xFFFFFFFF:08x}\n")
    print(f"wrote {len(words)} words to {args.out}")


if __name__ == "__main__":
    main()
