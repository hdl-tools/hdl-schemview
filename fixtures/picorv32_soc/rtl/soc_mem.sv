// soc_mem -- trivial synchronous memory answering the picorv32 native bus.
// One-cycle latency: assert ready the cycle after a request. Initialized from
// a hex image via $readmemh (simulation only; ignored at elaboration).
module soc_mem
  import soc_pkg::*;
#(
  parameter int unsigned WORDS     = MEM_WORDS,
  parameter string       INIT_FILE = ""
) (
  mem_if.mem bus,
  input logic resetn
);
  localparam int unsigned AW = $clog2(WORDS);

  logic [XLEN-1:0] ram [0:WORDS-1];

  initial begin
    if (INIT_FILE != "") begin
      $readmemh(INIT_FILE, ram);
    end
  end

  // Word index into the RAM; high address bits are ignored (memory wraps),
  // which keeps the firmware's data writes inside the array.
  wire [AW-1:0] word_idx = bus.addr[AW+1:2];

  always_ff @(posedge bus.clk) begin
    bus.ready <= 1'b0;
    bus.rdata <= '0;
    if (resetn && bus.valid && !bus.ready) begin
      bus.ready <= 1'b1;
      bus.rdata <= ram[word_idx];
      if (bus.wstrb[0]) ram[word_idx][7:0]   <= bus.wdata[7:0];
      if (bus.wstrb[1]) ram[word_idx][15:8]  <= bus.wdata[15:8];
      if (bus.wstrb[2]) ram[word_idx][23:16] <= bus.wdata[23:16];
      if (bus.wstrb[3]) ram[word_idx][31:24] <= bus.wdata[31:24];
    end
  end
endmodule : soc_mem
