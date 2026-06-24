// soc_pkg -- shared types/parameters for the picorv32_soc tier-1 fixture.
// Exists so the fixture exercises SystemVerilog *package* elaboration
// (one of the four constructs the cross-probe matcher must handle).
package soc_pkg;

  // Machine word width, reused by the interface and the wrapper.
  parameter int unsigned XLEN = 32;

  // Words of memory per core lane (kept small so the golden hierarchy and
  // the trace stay hand-verifiable).
  parameter int unsigned MEM_WORDS = 512;

  // Coarse lane state -- packed enum, so a package-typed Var shows up in the
  // elaborated model and in the waveform.
  typedef enum logic [1:0] {
    LANE_RESET = 2'd0,
    LANE_RUN   = 2'd1,
    LANE_TRAP  = 2'd2
  } lane_state_e;

  // Packed struct carried on the bus -- another package-typed signal.
  typedef struct packed {
    logic [XLEN-1:0] addr;
    logic [XLEN-1:0] wdata;
    logic [3:0]      wstrb;
  } mem_req_t;

endpackage : soc_pkg
