// mem_if -- SystemVerilog interface bundling the picorv32 native memory bus.
// Exists so the fixture exercises *interface* elaboration (one of the four
// constructs the cross-probe matcher must handle). Used as a plain signal
// bundle plus modports; one instance is created per generated core lane.
interface mem_if #(
  parameter int unsigned XLEN = 32
) (
  input logic clk
);
  logic            valid;
  logic            instr;
  logic            ready;
  logic [XLEN-1:0] addr;
  logic [XLEN-1:0] wdata;
  logic [3:0]      wstrb;
  logic [XLEN-1:0] rdata;

  // Driven by the CPU core.
  modport core (
    input  clk, ready, rdata,
    output valid, instr, addr, wdata, wstrb
  );

  // Driven by the memory.
  modport mem (
    input  clk, valid, instr, addr, wdata, wstrb,
    output ready, rdata
  );
endinterface : mem_if
