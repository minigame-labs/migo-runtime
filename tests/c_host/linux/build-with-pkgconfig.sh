#!/usr/bin/env bash
# Build the C host example the way a third-party integrator would: plain cc,
# plain pkg-config, no cargo anywhere.
#
# This is the acceptance test for the package, not a convenience wrapper. If it
# cannot build and run, the package is not a package -- so nothing may be added
# to the link line here to make it work. A dependency a consumer has to know
# about is a packaging defect, and it gets fixed in the crate that creates it or
# in scripts/gen-linux-package-metadata.py. The one exception is Xlib: the
# example owns its window, and the engine deliberately does not link Xlib.
#
# Usage: tests/c_host/linux/build-with-pkgconfig.sh [OUTPUT_BINARY]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
PREFIX="${MIGO_PREFIX:-$REPO_ROOT/dist/migo-linux-x86_64}"
OUT="${1:-$SCRIPT_DIR/c-host}"

export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
pkg-config --exists migo || {
    echo "migo.pc not found under $PREFIX; run scripts/build-linux-sdk.sh first" >&2
    exit 1
}

# Dynamic linkage is the default because it is what an integrator does, and
# because it is the form the package's own dependency list describes:
# libmigo.so already records libGL, libEGL, libfontconfig and the rest in
# DT_NEEDED, so a consumer needs none of their development symlinks. `--static`
# pulls in Libs.private and does require every one of those -dev packages.
STATIC=0
[[ "${MIGO_STATIC:-0}" == "1" ]] && STATIC=1

if (( STATIC )); then
    MIGO_LIBS=$(pkg-config --libs --static migo)
else
    MIGO_LIBS=$(pkg-config --libs migo)
fi

echo "[c-host] cflags: $(pkg-config --cflags migo)"
echo "[c-host] libs:   $MIGO_LIBS"

# shellcheck disable=SC2046,SC2086  # word splitting of pkg-config output is intended
# X11 only. The Wayland host needs wayland-scanner plus generated xdg-shell
# protocol code, which is example plumbing rather than anything a consumer of the
# packaged SDK has to do -- and the point of this script is to prove the SDK is
# consumable with nothing but cc and pkg-config. The define is the same switch
# cargo's build.rs sets when it cannot build the Wayland half, so
# MIGO_C_HOST_BACKEND=wayland reports that clearly instead of failing to link.
"${CC:-cc}" -std=c11 -Wall -Wextra -o "$OUT" "$SCRIPT_DIR/main.c" \
    -DMIGO_C_HOST_NO_WAYLAND \
    $(pkg-config --cflags migo) \
    $MIGO_LIBS \
    -Wl,-rpath,"$PREFIX/lib" \
    -lX11

echo "[c-host] built $OUT"
