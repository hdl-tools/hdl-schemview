# pyslang — Python bindings for the slang SystemVerilog compiler.
#
# Not in nixpkgs (the `slang` there is jedsoft's S-Lang, unrelated), so we build
# it from the PyPI sdist. That sdist *is* the slang tree: as of 11.0.0 pyslang
# lives in the slang monorepo and `[tool.scikit-build] cmake.source-dir = "."`.
#
# The build is hermetic without a single patch, which is the non-obvious part.
# slang declares every dependency as
#
#     FetchContent_Declare(<dep> GIT_REPOSITORY ... FIND_PACKAGE_ARGS <version>)
#
# and `FIND_PACKAGE_ARGS` (CMake >= 3.24) makes `find_package` run *first*, so the
# git clone the sandbox forbids is only a fallback. Supplying the dependencies is
# therefore the whole mechanism — there is no `SLANG_USE_SYSTEM_*` flag to set
# (those were removed) and nothing to vendor.
#
# Only two of the five FetchContent blocks fire for a pylib build
# (SLANG_INCLUDE_TESTS=OFF, SLANG_INCLUDE_TOOLS=OFF, SLANG_INCLUDE_PYLIB=ON, all
# set by pyproject.toml):
#
#   fmt       external/CMakeLists.txt, fmt_min_version 12.1 — FATAL_ERROR if absent
#   pybind11  bindings/CMakeLists.txt, FIND_PACKAGE_ARGS 3.0
#
# mimalloc is forced OFF under SLANG_INCLUDE_PYLIB (CMakeLists.txt:180,210),
# cpptrace defaults OFF, and Catch2 is gated on SLANG_INCLUDE_TESTS. Boost is not
# FetchContent at all: `find_package(Boost 1.87.0 CONFIG QUIET)` degrades to the
# vendored external/boost_unordered.hpp that ships in the sdist, so adding boost
# here would be a large closure for no benefit.
#
# See #280 for the spike that established all of the above.
{
  lib,
  buildPythonPackage,
  fetchPypi,
  cmake,
  ninja,
  fmt_12,
  scikit-build-core,
  pybind11,
  pybind11-stubgen,
}:

buildPythonPackage rec {
  pname = "pyslang";
  version = "11.0.0";
  pyproject = true;

  src = fetchPypi {
    inherit pname version;
    hash = "sha256-lzbm+qBIo9BsXJzXlbVA7iSmBT0zdCimamIZSzJWgp0=";
  };

  # cmake and ninja are on PATH for scikit-build-core to invoke, but it drives the
  # configure itself — nixpkgs' cmake setup hook must stand down or the two fight.
  dontUseCmakeConfigure = true;

  nativeBuildInputs = [
    cmake
    ninja
  ];

  build-system = [
    scikit-build-core
    pybind11
    pybind11-stubgen
  ];

  # fmt_12 is 12.1.0 on nixos-25.11, and also the default `fmt` attr there; named
  # explicitly so a later channel bump moving `fmt` cannot silently drop us below
  # slang's 12.1 floor.
  buildInputs = [ fmt_12 ];

  # SLANG_INCLUDE_TESTS is already OFF via pyproject.toml, so there is no C++ test
  # suite to run here; the import check is the meaningful signal.
  doCheck = false;
  pythonImportsCheck = [ "pyslang" ];

  meta = {
    description = "Python bindings for slang, a SystemVerilog compiler library";
    homepage = "https://sv-lang.com/";
    license = lib.licenses.mit;
  };
}
