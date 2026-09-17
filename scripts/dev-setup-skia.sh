#!/usr/bin/env bash
# scripts/dev-setup-skia.sh
#
# Prepares a developer workstation for building Skia (via skia-safe) without
# requiring root access.  Safe to re-run; idempotent.
#
# What it does:
#   1) Ensures `ninja` is on PATH (downloads a prebuilt binary to ~/.local/bin
#      if missing; apt would otherwise require sudo).
#   2) Fetches the Khronos EGL / GL / GLES headers into ~/.local/skia-headers.
#      These are required to compile Skia's EGL / Ganesh GL interface but are
#      not shipped in minimal Ubuntu / WSL installations.
#   3) Prints the `export` lines to append to the current shell so that
#      `cargo check -p migo-graphics` can pick them up.  Intended to be `source`-d:
#
#        source scripts/dev-setup-skia.sh
#
# On CI this script is superseded by the distro `ninja-build` +
# `libegl1-mesa-dev` packages; the Android cross-compile path uses the NDK
# sysroot directly (see scripts/build-android-so.sh, Phase 6).

set -euo pipefail

LOCAL_BIN="$HOME/.local/bin"
LOCAL_LIB="$HOME/.local/lib"
HEADERS_DIR="$HOME/.local/skia-headers"

mkdir -p "$LOCAL_BIN" "$LOCAL_LIB" "$HEADERS_DIR"/{EGL,KHR,GL,GLES2,GLES3}

# ---- runtime library symlinks -----------------------------------------
# skia-bindings unconditionally requests -lfontconfig -lfreetype -lEGL at
# link time, even when embed-freetype is on.  Minimal Ubuntu hosts ship the
# versioned .so files (libfontconfig.so.1, libfreetype.so.6, libEGL.so.1)
# but not the symlinks the linker searches for.  Create unversioned aliases
# in a user-writable path.
link_runtime_so() {
  local soname="$1" target
  for t in \
    "/usr/lib/x86_64-linux-gnu/$soname" \
    "/usr/lib/$soname" \
    "/usr/lib64/$soname"; do
    if [ -f "$t" ]; then target="$t"; break; fi
  done
  if [ -z "${target:-}" ]; then
    echo "[dev-setup-skia] warning: $soname not found; install its -dev pkg"
    return
  fi
  local link="$LOCAL_LIB/${soname%.*}"
  link="${link%.*}"            # strip trailing ".NN" version
  link="${link/%.so/}"          # e.g. libEGL.so.1 → libEGL
  link="${link}.so"
  ln -sf "$target" "$link"
}
link_runtime_so "libfontconfig.so.1"
link_runtime_so "libfreetype.so.6"
link_runtime_so "libEGL.so.1"
# Desktop GL exports the gl* entry points Skia (skia_use_gl=true) references;
# the Linux dev player links -lGL against it (see engine/tools/player/build.rs).
link_runtime_so "libGL.so.1"

# ---- ninja --------------------------------------------------------------
if ! command -v ninja >/dev/null 2>&1; then
  if [ ! -x "$LOCAL_BIN/ninja" ]; then
    echo "[dev-setup-skia] fetching ninja 1.12.1 prebuilt"
    tmp="$(mktemp -d)"
    curl -sSLf \
      https://github.com/ninja-build/ninja/releases/download/v1.12.1/ninja-linux.zip \
      -o "$tmp/ninja.zip"
    unzip -q -o "$tmp/ninja.zip" -d "$LOCAL_BIN"
    chmod +x "$LOCAL_BIN/ninja"
    rm -rf "$tmp"
  fi
fi

# ---- Khronos headers ---------------------------------------------------
fetch_header() {
  local path="$1" base="$2"
  local out="$HEADERS_DIR/$path"
  if [ -s "$out" ]; then return; fi
  # A machine with the distro's -dev packages (libegl-dev, libgles-dev,
  # libgl-dev) already has this header on the compiler's default include path,
  # and needs nothing from the network. CI installs exactly those packages and
  # still fetched every header here until registry.khronos.org began answering
  # the runners with 403 (2026-09-17), which failed the host build before it
  # started -- for files that were already installed.
  if [ -s "/usr/include/$path" ]; then return; fi
  echo "[dev-setup-skia] fetching $path"
  curl -sSLf "$base/$path" -o "$out"
}

# EGL registry
for h in EGL/egl.h EGL/eglext.h EGL/eglplatform.h KHR/khrplatform.h; do
  fetch_header "$h" "https://registry.khronos.org/EGL/api"
done

# OpenGL / GLES registry
for h in GL/glext.h GLES2/gl2.h GLES2/gl2ext.h GLES2/gl2platform.h \
         GLES3/gl3.h GLES3/gl3platform.h; do
  fetch_header "$h" "https://registry.khronos.org/OpenGL/api"
done

# ---- export lines -------------------------------------------------------
cat <<EOF

[dev-setup-skia] done.  Add the following to your shell (or re-source this
file) before running \`cargo check\`:

  export PATH="\$HOME/.local/bin:\$PATH"
  export CPATH="\$HOME/.local/skia-headers\${CPATH:+:\$CPATH}"

EOF

# If the script is *sourced*, set the vars directly in the caller.
if (return 0 2>/dev/null); then
  export PATH="$HOME/.local/bin:$PATH"
  export CPATH="$HEADERS_DIR${CPATH:+:$CPATH}"
  # Linker search path.  ~/.local/lib holds symlinks from libfontconfig.so,
  # libfreetype.so, libEGL.so to the runtime libfontconfig.so.1 etc. that
  # ship with minimal Ubuntu.  Skia-bindings links them by name even when
  # the `embed-freetype` feature is active.
  export LIBRARY_PATH="$HOME/.local/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
fi
