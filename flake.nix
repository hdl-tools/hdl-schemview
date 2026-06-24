{
  description = "hdl-schemview development environment (reproducible Verilator + toolchain)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        # Full dev shell: `nix develop`
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            verilator      # pinned by nixpkgs; reproducible across machines
            rustc
            cargo
            rustfmt
            clippy
            python311
            uv
            jq
          ];
        };

        # Minimal shell for just regenerating traces: `nix develop .#verilator`
        devShells.verilator = pkgs.mkShell {
          packages = [ pkgs.verilator ];
        };
      });
}
