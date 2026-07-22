// Generated-style RTL for the mac() kernel in foo.cpp (#159 fixture).
// Each generated statement carries the C line it came from, mimicking the
// line-annotated provenance comments HLS tools embed in their output.
module mac(
  input  [31:0] a,
  input  [31:0] b,
  input  [31:0] c,
  output [31:0] sum
);
  wire [31:0] prod;
  assign prod = a * b;     // foo.cpp:5
  assign sum  = prod + c;  // foo.cpp:6
endmodule
