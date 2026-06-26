"""Tests for the always_ff procedural-driver lint.

The lint exists because slang/pyslang (and Verilator) accept a memory that is
both ``$readmemh``-initialized and written from ``always_ff``, while VCS rejects
it with ``Error-[ICPD]``. These tests pin the tier-1 fixture as clean and prove
the lint catches the offending pattern.
"""
from __future__ import annotations

from pathlib import Path

from svxprobe_elaborate.lint import lint_files, lint_source

REPO = Path(__file__).resolve().parents[2]
RTL = REPO / "fixtures" / "picorv32_soc" / "rtl"
SOURCES = [
    str(RTL / "picorv32.v"),
    str(RTL / "soc_pkg.sv"),
    str(RTL / "mem_if.sv"),
    str(RTL / "soc_mem.sv"),
    str(RTL / "picorv32_soc.sv"),
]

# A minimal memory that reproduces the soc_mem bug: $readmemh in `initial` plus
# writes from a clocked block. Parameterize the clocked keyword per test.
_MEM = """
module mem #(parameter string INIT = "") (
  input  logic        clk,
  input  logic [3:0]  addr,
  input  logic [31:0] wdata,
  output logic [31:0] rdata
);
  logic [31:0] ram [0:15];
  initial begin
    if (INIT != "") $readmemh(INIT, ram);
  end
  {kw} @(posedge clk) begin
    ram[addr] <= wdata;
    rdata     <= ram[addr];
  end
endmodule
"""


def test_fixture_is_clean() -> None:
    """The committed tier-1 RTL must have no illegal always_ff drivers."""
    assert lint_files(SOURCES, top="picorv32_soc") == []


def test_flags_always_ff_with_readmemh() -> None:
    """A $readmemh-initialized array written from always_ff is reported."""
    findings = lint_source(_MEM.format(kw="always_ff"), top="mem")
    rams = [f for f in findings if f.path.endswith(".ram")]
    assert rams, f"expected `ram` to be flagged, got {[f.path for f in findings]}"
    assert rams[0].others == ["Initial"]
    assert "Error-[ICPD]" in rams[0].message()


def test_plain_always_is_clean() -> None:
    """The same memory written from a plain `always` block is legal."""
    assert lint_source(_MEM.format(kw="always"), top="mem") == []


def test_two_always_ff_same_var_flagged() -> None:
    """Two always_ff blocks writing one variable is also an illegal combination."""
    src = """
    module dual (input logic clk, input logic a, input logic b, output logic q);
      always_ff @(posedge clk) q <= a;
      always_ff @(posedge clk) q <= b;
    endmodule
    """
    findings = lint_source(src, top="dual")
    qs = [f for f in findings if f.path.endswith(".q")]
    assert qs, "expected `q` to be flagged for multiple always_ff drivers"
    assert qs[0].others == []
    assert "more than one always_ff" in qs[0].message()
