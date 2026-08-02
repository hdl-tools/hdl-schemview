{
  description = "hdl-schemview — RTL-level SystemVerilog cross-probe tool (svxprobe CLI + reproducible dev shells)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";

    # nixpkgs 25.05 ships rustc 1.86; core/rust-toolchain.toml pins 1.94. Without
    # this input the dev shell silently hands out a toolchain that cannot build
    # core/ — the flake would claim reproducibility while providing the wrong
    # compiler. See #243.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    let
      lib = nixpkgs.lib;

      # Nix evaluates a flake from the *git tree*, so anything untracked is
      # invisible here.
      #
      # `coreSrc` is enough to build the CLI. `fullSrc` additionally carries
      # fixtures/, which the checks need for two reasons: every crate's
      # integration tests join CARGO_MANIFEST_DIR up to fixtures/, and
      # scale-bench's `golden` feature include_bytes!'s the committed
      # hierarchy.json from there (core/crates/scale-bench/src/golden.rs).
      coreSrc = lib.fileset.toSource {
        root = ./.;
        fileset = ./core;
      };
      fullSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [ ./core ./fixtures ];
      };

      cargoCommon = {
        version = "0.0.0"; # core/Cargo.toml [workspace.package]
        cargoLock.lockFile = ./core/Cargo.lock;
        # The workspace is a subdirectory: cargoRoot places the vendored
        # .cargo/config.toml, buildAndTestSubdir is where cargo actually runs.
        cargoRoot = "core";
        buildAndTestSubdir = "core";
      };

      meta = {
        description = "RTL-level SystemVerilog cross-probe tool: source ↔ schematic ↔ waveform";
        homepage = "https://github.com/chuanseng-ng/hdl-schemview";
        license = lib.licenses.mit;
        mainProgram = "svxprobe";
      };

      # The Rust version lives in exactly one place. Parsing that file rather
      # than restating it means the flake and the rustup pin cannot drift — and
      # `fromRustupToolchainFile` reads its `components` list too, so rustfmt and
      # clippy come from the same declaration the PR gate relies on.
      #
      # It has to be the file parser, not `rust-bin.stable.${channel}`:
      # rust-toolchain.toml says `channel = "1.94"`, which is a rustup channel
      # ("newest 1.94.x"), while `rust-bin.stable` is keyed by exact releases
      # (1.94.0, 1.94.1). Indexing it with "1.94" fails.
      #
      # Parameterized on a pkgs that already carries rust-overlay, so the same
      # definition serves both `packages.*` and `overlays.default`.
      toolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./core/rust-toolchain.toml;

      rustPlatformFor = pkgs:
        let toolchain = toolchainFor pkgs;
        in pkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        };

      svxprobeFor = pkgs:
        (rustPlatformFor pkgs).buildRustPackage (cargoCommon // {
          pname = "svxprobe";
          src = coreSrc;
          # The CLI does not depend on scale-bench, so this build never reaches
          # the include_bytes! of the golden fixture — hence coreSrc suffices.
          cargoBuildFlags = [ "-p" "svxprobe" ];
          # The workspace's tests need fixtures/; they run in `checks.test`,
          # against fullSrc.
          doCheck = false;
          inherit meta;
        });
    in
    {
      # System-independent, so it lives outside eachDefaultSystem. It composes
      # rust-overlay because the package needs `rust-bin` to reach the pinned
      # toolchain — a consumer applying this overlay gets both.
      overlays.default = lib.composeExtensions rust-overlay.overlays.default
        (final: _prev: {
          svxprobe = svxprobeFor final;
        });
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        toolchain = toolchainFor pkgs;
        rustPlatform = rustPlatformFor pkgs;

        # One derivation per PR gate, each mirroring the command ci.yml runs.
        # They deliberately duplicate ci.yml: what they prove is that *the
        # flake* can still run the gate, which is the thing that rots.
        mkCargoCheck = name: command:
          rustPlatform.buildRustPackage (cargoCommon // {
            pname = "svxprobe-check-${name}";
            src = fullSrc;
            nativeBuildInputs = [ toolchain ];
            buildPhase = ''
              runHook preBuild
              pushd core
              ${command}
              popd
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              runHook preInstall
              touch $out
              runHook postInstall
            '';
          });
      in
      {
        packages = {
          svxprobe = svxprobeFor pkgs;
          default = self.packages.${system}.svxprobe;
        };

        apps = {
          svxprobe = flake-utils.lib.mkApp { drv = self.packages.${system}.svxprobe; };
          default = self.apps.${system}.svxprobe;
        };

        checks = {
          inherit (self.packages.${system}) svxprobe;
          fmt = mkCargoCheck "fmt" "cargo fmt --all --check";
          clippy = mkCargoCheck "clippy" "cargo clippy --offline --all-targets -- -D warnings";
          test = mkCargoCheck "test" "cargo test --offline --all";
        };

        # Full dev shell: `nix develop`
        devShells.default = pkgs.mkShell {
          packages = [
            # Carries rustc/cargo/rustfmt/clippy at the rust-toolchain.toml pin.
            toolchain
            pkgs.verilator # pinned by nixpkgs; reproducible across machines
            # The Python side is deliberately impure: `uv sync` fetches pyslang
            # from PyPI at run time. Packaging the harness as a derivation is
            # blocked on pyslang building offline under Nix — tracked separately
            # (tier B of #243).
            pkgs.python311
            pkgs.uv
            pkgs.jq # golden-reproducibility diff, see ci.yml
          ];
        };

        # Minimal shell for just regenerating traces: `nix develop .#verilator`
        devShells.verilator = pkgs.mkShell {
          packages = [ pkgs.verilator ];
        };
      });
}
