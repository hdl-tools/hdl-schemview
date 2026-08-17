{
  description = "hdl-schemview — RTL-level SystemVerilog cross-probe tool (svxprobe CLI + reproducible dev shells)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";

    # The channel is 25.11 because slang needs fmt >= 12.1 and pybind11 >= 3.0 to
    # build hermetically: it declares every FetchContent_Declare with CMake 3.24
    # FIND_PACKAGE_ARGS, so find_package runs first and the sandbox-forbidden git
    # clone is only a fallback. 25.05 capped at fmt 11.0.2 / pybind11 2.13.6. See #280.
    #
    # nixpkgs 25.11 ships rustc 1.97.1; core/rust-toolchain.toml pins 1.94. The drift
    # is now upward (25.05 was behind at 1.86), but the hazard is unchanged: without
    # this input the dev shell silently hands out a toolchain that is not the pinned
    # one, and the flake would claim reproducibility while providing the wrong
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
      # Also rooted at ./. — see nix/svxprobe-elaborate.nix: the harness tests
      # reach fixtures/ via parents[2], so elaborate/ and fixtures/ have to stay
      # siblings at the same depth they occupy in the repo.
      elaborateSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [ ./elaborate ./fixtures ];
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

      # pyslang is not in nixpkgs, so it is built here and threaded into the
      # harness. Parameterized on pkgs for the same reason svxprobeFor is: one
      # definition serves both `packages.*` and `overlays.default`.
      pyslangFor = pkgs: pkgs.python3Packages.callPackage ./nix/pyslang.nix { };

      harnessFor = pkgs:
        pkgs.python3Packages.callPackage ./nix/svxprobe-elaborate.nix {
          pyslang = pyslangFor pkgs;
          src = elaborateSrc;
        };
    in
    {
      # System-independent, so it lives outside eachDefaultSystem. It composes
      # rust-overlay because the package needs `rust-bin` to reach the pinned
      # toolchain — a consumer applying this overlay gets both.
      overlays.default = lib.composeExtensions rust-overlay.overlays.default
        (final: _prev: {
          svxprobe = svxprobeFor final;
          svxprobe-elaborate = harnessFor final;
          # Exposed on its own because it is useful independently of this repo —
          # pyslang is absent from nixpkgs, and a downstream flake wanting the
          # slang bindings should not have to vendor this derivation.
          pyslang = pyslangFor final;
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
        harness = harnessFor pkgs;

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
          svxprobe-elaborate = harness;
          pyslang = pyslangFor pkgs;
          default = self.packages.${system}.svxprobe;
        };

        apps = {
          svxprobe = flake-utils.lib.mkApp { drv = self.packages.${system}.svxprobe; };
          svxprobe-elaborate = flake-utils.lib.mkApp {
            drv = self.packages.${system}.svxprobe-elaborate;
          };
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
            # Verilator compiles its FST writer (fstapi.c) in *our* environment, not
            # inside its own derivation, and nixpkgs does not carry zlib in
            # verilator's buildInputs. Without this, `fixtures/regen.sh` dies on
            # "fatal error: zlib.h: No such file or directory" and only the VCD leg
            # works — so `nix develop .#verilator`, which docs/fixtures.md names as
            # the pinned-Verilator path, could never actually rebuild the FST. #280.
            pkgs.zlib
            # The harness, built from source including pyslang — no `uv sync`, no
            # PyPI fetch at run time. `svxprobe-elaborate` on PATH is also what
            # Session::elaborate_and_load looks for (core/crates/gui/src/lib.rs),
            # so designlist loading works inside `nix develop`. #280 closed the
            # tier-B gap that made this impure.
            harness
            # An interpreter carrying the same dependency set, for running the
            # *working tree* (`python -m svxprobe_elaborate.validate ...`). The
            # harness above is a fixed store build and cannot see your edits; both
            # are useful and they are deliberately different things.
            (pkgs.python3.withPackages (ps: [
              (pyslangFor pkgs)
              ps.jsonschema
              ps.pytest
            ]))
            pkgs.ruff # elaborate/pyproject.toml [dependency-groups] dev
            pkgs.jq # golden-reproducibility diff, see ci.yml
          ];
          # Makes the working tree importable, so `python -m svxprobe_elaborate.x`
          # runs your edits against Nix-provided dependencies.
          shellHook = ''
            export PYTHONPATH="$PWD/elaborate''${PYTHONPATH:+:$PYTHONPATH}"
          '';
        };

        # Toolchain only, for nix.yml's rustc-pin assertion: `nix develop .#ci`.
        #
        # It exists purely for cost. `devShells.default` now carries the harness,
        # so entering it builds pyslang from source — ~11 min on a hosted runner
        # with no binary cache, which took nix.yml from 2m33s to 13m39s just to
        # read `rustc --version`.
        #
        # What is asserted is not weakened by this: `toolchain` is the same
        # binding both shells use, so the assertion still pins the toolchain
        # derivation itself. The two shells cannot disagree on rustc without
        # `toolchainFor` changing, which would move both. What CI no longer
        # exercises is that `devShells.default` *evaluates and builds* — the
        # nightly nix-harness job covers that, since it builds the same closure.
        devShells.ci = pkgs.mkShell { packages = [ toolchain ]; };

        # Minimal shell for just regenerating traces: `nix develop .#verilator`
        devShells.verilator = pkgs.mkShell {
          packages = [ pkgs.verilator pkgs.zlib ]; # zlib: see the note above
        };
      });
}
