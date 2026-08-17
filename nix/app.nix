# The Tauri desktop shell. Linux only — Tauri's macOS/Windows bundling has no
# meaningful Nix story, so `packages.<system>` simply does not offer it elsewhere
# (see flake.nix, and ADR 0012 for why this output is not a PR gate).
{
  lib,
  rustPlatform,
  src,
  version,
  frontend,
  svxprobe-elaborate,
  pkg-config,
  wrapGAppsHook3,
  copyDesktopItems,
  makeDesktopItem,
  glib,
  glib-networking,
  gsettings-desktop-schemas,
  gtk3,
  gdk-pixbuf,
  cairo,
  pango,
  atk,
  librsvg,
  libsoup_3,
  webkitgtk_4_1,
  libayatana-appindicator,
  openssl,
}:

rustPlatform.buildRustPackage {
  pname = "hdl-schemview-app";
  inherit src version;

  # A second, independent cargo workspace with its own lockfile — deliberately
  # separate so core CI never builds webkit (app/src-tauri/Cargo.toml).
  cargoLock.lockFile = ../app/src-tauri/Cargo.lock;
  cargoRoot = "app/src-tauri";
  buildAndTestSubdir = "app/src-tauri";

  # tauri-build's generate_context! reads frontendDist ("../dist", relative to
  # app/src-tauri) at *compile* time, and app/dist is gitignored — so it is absent
  # from the flake source and has to be materialised here.
  postPatch = ''
    mkdir -p app/dist
    cp -r ${frontend}/. app/dist/
  '';

  nativeBuildInputs = [
    pkg-config
    # Must be a *native* input: it is a setup hook. In buildInputs it silently
    # does nothing, yielding a binary that runs on the builder and dies on a
    # user's machine at the first GSettings lookup.
    wrapGAppsHook3
    copyDesktopItems
  ];

  # wrapGAppsHook3 scrapes these to bake the runtime environment into the wrapper.
  # `nix shell` only puts bin/ on PATH — it sets no XDG_DATA_DIRS, no
  # GSETTINGS_SCHEMA_DIR, no GIO_MODULE_DIR — so anything the app needs at runtime
  # must be captured at build time or it is simply absent for a consumer.
  #   glib + gsettings-desktop-schemas + gtk3  -> GSETTINGS_SCHEMAS_PATH, without
  #     which the file dialog (tauri-plugin-dialog) aborts
  #   gdk-pixbuf + librsvg                     -> GDK_PIXBUF_MODULE_FILE (SVG loader)
  #   glib-networking                          -> GIO_MODULE_DIR, WebKit's TLS
  buildInputs = [
    glib
    glib-networking
    gsettings-desktop-schemas
    gtk3
    gdk-pixbuf
    cairo
    pango
    atk
    librsvg
    libsoup_3
    webkitgtk_4_1
    libayatana-appindicator
    openssl
  ];

  # The app crate is a thin shell over svxprobe-gui, which core's `checks.test`
  # already covers; there are no tests here to run.
  doCheck = false;

  # The crate declares no [[bin]], so cargo emits `hdl-schemview-app`.
  # tauri.conf.json's mainBinaryName is honoured by the *bundler*, not by cargo —
  # so without this rename the Nix output reintroduces exactly the mismatch #275
  # fixed for the .deb/.rpm.
  postInstall = ''
    mv $out/bin/hdl-schemview-app $out/bin/hdl-schemview
    for s in 32 64 128; do
      install -Dm444 app/src-tauri/icons/''${s}x''${s}.png \
        $out/share/icons/hicolor/''${s}x''${s}/apps/hdl-schemview.png
    done
  '';

  # Bake the harness in, so `nix shell .#hdl-schemview-app` can elaborate RTL
  # rather than only open a pre-built hierarchy.json. `--set` on the env var
  # rather than `--prefix PATH`: HARNESS_ENV is the app's documented first-choice
  # lookup (core/crates/gui/src/lib.rs), it is one store path instead of a whole
  # bin/, and it does not leak a python env into every subprocess the app spawns.
  # The user can still override it, and the PATH rung keeps working.
  preFixup = ''
    gappsWrapperArgs+=(--set SVXPROBE_ELABORATE "${svxprobe-elaborate}/bin/svxprobe-elaborate")
  '';

  desktopItems = [
    (makeDesktopItem {
      name = "hdl-schemview";
      desktopName = "hdl-schemview";
      comment = "RTL-level SystemVerilog cross-probe tool";
      exec = "hdl-schemview %F";
      icon = "hdl-schemview";
      categories = [ "Development" ];
      terminal = false;
    })
  ];

  meta = {
    description = "hdl-schemview desktop app (Tauri shell over svxprobe-gui)";
    homepage = "https://github.com/hdl-tools/hdl-schemview";
    license = lib.licenses.mit;
    mainProgram = "hdl-schemview";
    platforms = lib.platforms.linux;
  };
}
