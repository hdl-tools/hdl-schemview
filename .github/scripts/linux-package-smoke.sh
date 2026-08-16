#!/usr/bin/env bash
#
# Install a built Linux package in a CLEAN CONTAINER and prove it runs.
#
#   usage: linux-package-smoke.sh <deb|rpm>
#
# Why a container and not the runner: the *Install Tauri Linux deps* step in
# app.yml puts WebKitGTK and GTK on ubuntu-latest, so installing there would
# succeed no matter what `bundle.linux.<fmt>.depends` declares. The declaration
# is only load-bearing on a machine that does not already have the libraries —
# which is exactly the machine a user installs on.
#
# The two formats are one script because they assert the same four things and
# differ only in the package manager's spelling. Keeping them apart invites the
# drift #246 documents: two collectors over one matrix diverge.
#
# What is asserted, and why each is worth a line:
#
#   1. The package declares WebKitGTK.  Unset `depends` makes Tauri emit an RPM
#      with NO dependencies at all — it installs cleanly, then cannot launch.
#      Silent, and the worst outcome available here.
#   2. It installs, resolving those names against a current distro.  This is
#      what catches a rename (the WebKitGTK 4.0 -> 4.1 churn already happened).
#   3. The launcher actually runs — a real headless `--bench` — which catches
#      anything needed at load time but undeclared. The binary links WebKitGTK
#      as a NEEDED library, so the loader resolves it even though `--bench`
#      never opens a window.
#   4. The desktop entry and icons install where a launcher will find them.
#
# The launcher is read back out of the payload rather than assumed. It is now
# `hdl-schemview` — `mainBinaryName` renames the crate binary at bundle time, so
# the installed command finally matches what every doc says (#275). Reading it
# back is kept anyway: this script's job is to check what the package actually
# ships, and asserting the name we hoped for is how the mismatch went unseen
# through two releases.
set -euo pipefail

fmt=${1:?usage: linux-package-smoke.sh <deb|rpm>}
bundle="$PWD/app/src-tauri/target/release/bundle"

case "$fmt" in
  # Matched to the build host's glibc. The binary is compiled on ubuntu-latest,
  # so an older base (Debian 12) could fail on glibc for reasons that have
  # nothing to do with packaging, and report a packaging bug that is not there.
  deb) dir="$bundle/deb"; image="ubuntu:24.04" ;;
  # Deliberately unpinned: a Fedora package rename should surface here rather
  # than in a user's install.
  rpm) dir="$bundle/rpm"; image="fedora:latest" ;;
  *)   echo "::error::unknown package format '$fmt' (expected deb or rpm)" >&2; exit 2 ;;
esac

pkg=$(find "$dir" -name "*.$fmt" 2>/dev/null | head -1)
test -n "$pkg" || { echo "::error::no .$fmt was produced in $dir"; exit 1; }
echo "testing $(basename "$pkg") in $image"

# Read-only mount: the container runs as root, and anything it wrote into the
# workspace would land root-owned and break the upload-artifact step.
docker run --rm -i -v "$dir:/pkg:ro" -e FMT="$fmt" "$image" bash -s <<'CONTAINER'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

pkg=$(find /pkg -name "*.$FMT" | head -1)

case "$FMT" in
  deb)
    apt-get update -qq
    # Tauri writes the tar entries as `usr/bin/x`, not the `./usr/bin/x` dpkg
    # normally emits — so strip a leading `.` if present and then force exactly
    # one leading slash, rather than assuming either spelling. Getting this
    # wrong makes every path miss `^/usr/...` and the payload look empty.
    payload() { dpkg-deb -c "$pkg" | awk '{print $NF}' | sed -e 's|^\.||' -e 's|^/*|/|'; }
    requires() { dpkg-deb -f "$pkg" Depends; }
    # apt resolves dependencies for a local file only when given a PATH, hence
    # the explicit "$pkg" rather than a bare package name.
    install_pkg() { apt-get install -y -qq "$pkg"; }
    validator() { apt-get install -y -qq desktop-file-utils; }
    ;;
  rpm)
    payload() { rpm -qpl "$pkg"; }
    requires() { rpm -qp --requires "$pkg"; }
    # --nogpgcheck: the package is unsigned (ADR 0009).
    install_pkg() { dnf install -y --nogpgcheck "$pkg"; }
    validator() { dnf install -y -q desktop-file-utils; }
    ;;
esac

echo "--- declared dependencies ---"
requires
requires | grep -qi 'webkit' || {
  echo "::error::the .$FMT declares no WebKitGTK dependency — check bundle.linux.$FMT.depends"
  exit 1
}

echo "--- payload ---"
payload
# `|| true` on every capture below: `set -e` with `pipefail` aborts the whole
# script when a grep matches nothing, which would skip the explicit guards and
# fail with no message at all — the exact way the first .deb run died.
bin=$(payload | grep -E '^/usr/bin/[^/]+$' | head -1 || true)
test -n "$bin" || { echo "::error::the .$FMT installs no /usr/bin launcher"; exit 1; }

desktop=$(payload | grep -E '^/usr/share/applications/.+\.desktop$' | head -1 || true)
test -n "$desktop" || { echo "::error::the .$FMT installs no .desktop entry"; exit 1; }

echo "--- install ---"
install_pkg

echo "--- run ---"
test -x "$bin" || { echo "::error::$bin missing after install"; exit 1; }
"$bin" --bench --bases golden --out /tmp/pkg-bench.md
grep -q 'Single-shot run' /tmp/pkg-bench.md
grep -q 'hdl-schemview --bench' /tmp/pkg-bench.md

echo "--- desktop integration ---"
validator >/dev/null
desktop-file-validate "$desktop"

# Exec= must name something that exists. A .desktop entry pointing at a binary
# under a different name installs fine and then does nothing when clicked —
# invisible to every other check here.
exec_cmd=$(awk -F= '/^Exec=/{print $2; exit}' "$desktop" | awk '{print $1}' || true)
command -v "$exec_cmd" >/dev/null || {
  echo "::error::$desktop has Exec=$exec_cmd, which is not on PATH (installed launcher is $bin)"
  exit 1
}

# Icon= is a bare theme name, so at least one hicolor size must carry it.
icon=$(awk -F= '/^Icon=/{print $2; exit}' "$desktop")
test -n "$icon" || { echo "::error::$desktop declares no Icon="; exit 1; }
found=$(find /usr/share/icons/hicolor -name "$icon.png" | sort || true)
test -n "$found" || { echo "::error::no installed hicolor icon matches Icon=$icon"; exit 1; }
echo "installed icons for '$icon':"
echo "$found"

# hicolor theme directories are plain sizes. Tauri maps `128x128@2x.png` to a
# `256x256@2` directory, which no icon lookup will ever read — the file is
# installed but dead. Reported, not fatal: it costs a size, not the launcher.
if echo "$found" | grep -q '@'; then
  echo "::warning::icon installed into a non-standard hicolor directory (see #261):"
  echo "$found" | grep '@'
fi
CONTAINER
