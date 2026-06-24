// Deterministic testbench for the picorv32_soc tier-1 fixture.
//
// Drives a fixed reset + a fixed number of clock cycles so the dumped trace is
// byte-reproducible. The dump file name is taken from a runtime plusarg so the
// SAME testbench produces both the VCD and the FST build (the format is chosen
// at verilation time via --trace vs --trace-fst, not by the file name).
//
// Verilate with:  verilator --binary --timing --trace[-fst] --top-module tb \
//                   <rtl...> tb_picorv32_soc.sv
// Run with:       ./Vtb +dumpfile=picorv32_soc.fst
`timescale 1ns / 1ps
module tb;
  localparam int unsigned N_CORES   = 2;
  localparam int unsigned RUN_CYCLES = 2000;

  logic                 clk = 1'b0;
  logic                 resetn = 1'b0;
  logic [N_CORES-1:0]   core_trap;

  // Device under test: the parameterized, generate-based SoC top.
  picorv32_soc #(
    .N_CORES(N_CORES)
  ) dut (
    .clk      (clk),
    .resetn   (resetn),
    .core_trap(core_trap)
  );

  // 100 MHz clock.
  always #5 clk = ~clk;

  string dumpfile;
  initial begin
    if (!$value$plusargs("dumpfile=%s", dumpfile)) begin
      dumpfile = "dump.vcd";
    end
    $dumpfile(dumpfile);
    $dumpvars(0, tb);

    repeat (4) @(posedge clk);
    resetn = 1'b1;
    repeat (RUN_CYCLES) @(posedge clk);

    $finish;
  end
endmodule : tb
