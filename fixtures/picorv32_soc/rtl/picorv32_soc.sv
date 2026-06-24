// picorv32_soc -- tier-1 reference fixture top.
//
// Deliberately exercises the four SystemVerilog constructs the cross-probe
// matcher exists to handle:
//   * package           -> soc_pkg (imported below)
//   * interface         -> mem_if, one instance per lane
//   * parameterized inst -> N picorv32 cores via a parameter
//   * generate block     -> the `g_lane` generate-for (produces genblk/array
//                           scope naming, the matcher's hardest case)
module picorv32_soc
  import soc_pkg::*;
#(
  parameter int unsigned N_CORES   = 2,
  parameter int unsigned MEM_WORDS = soc_pkg::MEM_WORDS,
  parameter string       INIT_FILE = "firmware.hex"
) (
  input  logic                 clk,
  input  logic                 resetn,
  output logic [N_CORES-1:0]   core_trap
);

  genvar gi;
  generate
    for (gi = 0; gi < int'(N_CORES); gi++) begin : g_lane
      // Per-lane bus (interface instance) + a package-typed state register.
      mem_if #(.XLEN(XLEN)) bus (.clk(clk));
      lane_state_e          lane_state;

      always_ff @(posedge clk) begin
        if (!resetn)            lane_state <= LANE_RESET;
        else if (core_trap[gi]) lane_state <= LANE_TRAP;
        else                    lane_state <= LANE_RUN;
      end

      picorv32 #(
        .ENABLE_COUNTERS  (0),
        .ENABLE_COUNTERS64(0),
        .COMPRESSED_ISA   (0),
        .ENABLE_MUL       (0),
        .ENABLE_DIV       (0),
        .ENABLE_IRQ       (0),
        .PROGADDR_RESET   (32'h0000_0000),
        .STACKADDR        (32'h0000_07fc)
      ) core (
        .clk      (clk),
        .resetn   (resetn),
        .trap     (core_trap[gi]),
        .mem_valid(bus.valid),
        .mem_instr(bus.instr),
        .mem_ready(bus.ready),
        .mem_addr (bus.addr),
        .mem_wdata(bus.wdata),
        .mem_wstrb(bus.wstrb),
        .mem_rdata(bus.rdata),
        // Unused optional interfaces tied off / left open.
        .irq      (32'b0),
        .pcpi_wr  (1'b0),
        .pcpi_rd  (32'b0),
        .pcpi_wait(1'b0),
        .pcpi_ready(1'b0)
      );

      soc_mem #(
        .WORDS    (MEM_WORDS),
        .INIT_FILE(INIT_FILE)
      ) memory (
        .bus   (bus.mem),
        .resetn(resetn)
      );
    end
  endgenerate

endmodule : picorv32_soc
