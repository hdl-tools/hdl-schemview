# The Vite/TypeScript frontend — `app/dist`, which the Tauri shell embeds.
#
# Built as its own derivation rather than from inside the Rust build, so a `tsc`
# error or an npm-fetch problem fails here instead of surfacing 20 minutes into a
# webkit link. `tauri.conf.json`'s `beforeBuildCommand: "npm run build"` is
# irrelevant to us: that is a Tauri-CLI concept, and nix/app.nix drives cargo
# directly, so nothing ever shells out to npm mid-build.
{
  lib,
  buildNpmPackage,
  version,
}:

buildNpmPackage {
  pname = "hdl-schemview-frontend";
  inherit version;

  # Only the inputs `tsc && vite build` actually reads. node_modules/, dist/ and
  # src-tauri/target/ are gitignored and so invisible to the flake anyway, but
  # naming the fileset keeps a src-tauri/ edit from invalidating this build.
  src = lib.fileset.toSource {
    root = ../app;
    fileset = lib.fileset.unions [
      ../app/package.json
      ../app/package-lock.json
      ../app/tsconfig.json
      ../app/vite.config.ts
      ../app/index.html
      ../app/src
    ];
  };

  # app/package-lock.json is committed and resolves entirely to registry.npmjs.org,
  # so fetchNpmDeps covers it with no vendoring workaround. Regenerate with:
  #   nix run nixpkgs#prefetch-npm-deps -- app/package-lock.json
  npmDepsHash = "sha256-SYns3M+u30NMAlL8ggz0gkAYKpQzyUtuWo4UipfVYF8=";

  # No ESBUILD_BINARY_PATH override. The usual nixpkgs workaround points vite at
  # the nixpkgs esbuild, but esbuild's install.js asserts the binary's version
  # equals the one package.json pins — nixpkgs ships 0.25.5 against vite 5's
  # 0.21.5, so the override fails the build outright. It is also unnecessary:
  # fetchNpmDeps vendors @esbuild/linux-x64, and esbuild's binaries are static Go,
  # so they run unpatched.

  # package.json: "build": "tsc && vite build". `npm test` (vitest) is deliberately
  # not run — app.yml gates it on every PR, and this output is not a gate
  # (see ADR 0012).
  npmBuildScript = "build";

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r dist/. $out/
    runHook postInstall
  '';

  meta = {
    description = "hdl-schemview desktop frontend (Vite bundle)";
    license = lib.licenses.mit;
  };
}
