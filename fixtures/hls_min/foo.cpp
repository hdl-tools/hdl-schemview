// A tiny synthetic HLS kernel (#159 fixture): multiply-accumulate.
// A real HLS tool would generate foo.sv from this and annotate each
// generated RTL statement with the C line it came from.
int mac(int a, int b, int c) {
  int prod = a * b;
  int sum = prod + c;
  return sum;
}
