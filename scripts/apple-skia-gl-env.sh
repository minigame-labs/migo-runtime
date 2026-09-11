#!/usr/bin/env bash
# Skia's macOS build assumes desktop GL, and Migo drives ANGLE, which is ES only.
#
# Source this before any cargo invocation that builds the engine for a
# `*-apple-darwin` target. It is a `source`d fragment rather than a wrapper
# because the callers are a build script, a workflow step and a developer's
# shell, and only the environment is common to all three.
#
# ## What it is for
#
# Skia's `gn/skia.gni` picks a compile-time GL standard per platform:
#
#     if (is_mac)  { skia_gl_standard = "gl"   }
#     else if (is_ios) { skia_gl_standard = "gles" }
#     ... else     { skia_gl_standard = ""     }   # runtime-detected
#
# and `skia/BUILD.gn` turns "gl" into `SK_ASSUME_GL=1`, which rewrites the
# dispatch in `GrGLTypes.h` into constants:
#
#     #define GR_IS_GR_GL(standard)    true
#     #define GR_IS_GR_GL_ES(standard) false
#     #define SK_DISABLE_GL_ES_INTERFACE 1
#
# So on macOS `GrGLMakeAssembledInterface` reads ANGLE's "OpenGL ES 3.0"
# version string, takes the `GR_IS_GR_GL` branch anyway because the macro is
# unconditionally true, and assembles a *desktop* GL interface against a driver
# that has no desktop GL entry points. It returns null. The ES assembler that
# would have worked is compiled to `return nullptr` by the same define.
#
# Migo has one GL implementation on Apple and it is ANGLE -- Metal underneath on
# both platforms -- so on macOS every Canvas2D surface fails to build its
# `GrDirectContext`, and WebGL keeps working because it never goes through Skia.
# That is the whole defect: it looks like "2D does not draw" and it is a build
# configuration two layers down.
#
# iOS is unaffected: `skia_gl_standard = "gles"` is what ANGLE actually is, so
# the shipping product's platform was always configured correctly. Android,
# Linux and Windows already get "" and dispatch at runtime. macOS is the only
# platform where the default contradicts the driver Migo supplies.
#
# Skia itself agrees this pairing is wrong -- `skia.gni` clears the assumption
# when its own ANGLE support is on:
#
#     if (skia_use_angle && skia_gl_standard != "gles") { skia_gl_standard = "" }
#
# We do not build Skia's ANGLE (we ship our own pinned one), so that line never
# fires for us and the same correction has to be made here.
#
# ## Why both variables, and why neither alone is enough
#
# `SKIA_GN_ARGS` is appended to the GN command line, where the later assignment
# wins. But it is **not part of the binary-cache key**: skia-bindings keys a
# prebuilt download on (skia commit, cargo features, debug) only. Set it alone
# and a successful download silently ignores it -- the build prints
# "DOWNLOAD AND INSTALL SUCCEEDED", the argument never reaches GN, and the
# defect survives with no diagnostic anywhere. `FORCE_SKIA_BUILD` is what makes
# the argument reachable, by taking the download out of the path.
#
# Both are read through skia-bindings' `cargo::env_var`, which emits
# `cargo:rerun-if-env-changed` for every name it reads, so adding them
# invalidates a previously cached build rather than being masked by one.
#
# ## What it costs
#
# Possibly nothing, and that is measured rather than assumed. A download is only
# attempted for a key the rust-skia project actually published, and this
# workspace's feature string does not appear to be one: a cold Linux build here
# reports
#
#     DOWNLOAD AND INSTALL FAILED: curl error code: "22"
#     curl stderr: "curl: (22) The requested URL returned error: 404"
#
# and falls through to a source build on its own. The key is
# (skia commit, cargo features, debug) with the target triple in the filename,
# so a 404 on one triple for this feature set makes a hit on another unlikely.
# `FORCE_SKIA_BUILD` therefore mostly guarantees what was already happening --
# but it guarantees it, which is the point: the failure mode it removes is a
# download that succeeds and silently discards the argument, and that is not
# something to leave to whether an upstream release happens to exist.
#
# Where a source build IS paid, `Swatinem/rust-cache` keeps
# `target/*/build/skia-bindings-*/out` across runs of a job that uses it. The
# `macos-v8` matrix row deliberately does not use it, so that row pays per run.

# ## Two more things this file has to carry, because turning the source build on
# ## is what makes them matter
#
# `engine/.cargo/config.toml` sets `CC = "clang-18"` in `[env]`, for a Linux
# reason it documents there. macOS has no `clang-18`, and until now that never
# showed: every Apple build downloaded a prebuilt Skia and compiled no C++ at
# all. The first source build on a macOS runner got as far as GN and then
# `/bin/sh: clang-18: command not found` on the first zlib object. A plain
# environment variable beats a non-forcing `[env]` entry, which is the same
# mechanism `build-apple-sdk.sh` already relies on -- and `${CC:-clang}` so a
# caller who deliberately chose a compiler keeps it.
export CC="${CC:-clang}"
export CXX="${CXX:-clang++}"

# The same `[env]` block also sets `SKIA_GN_ARGS`, for fontconfig and unwind
# tables. Exporting here REPLACES that value for this build rather than adding
# to it, and on macOS that is the intended outcome: those arguments have never
# reached a macOS Skia, because a macOS Skia has never been built from source
# here. Keeping them would be the change, not dropping them. The `:+` form
# still appends to a value a *caller* exported, which is the composition that
# should work.
export FORCE_SKIA_BUILD=1
export SKIA_GN_ARGS="${SKIA_GN_ARGS:+$SKIA_GN_ARGS }skia_gl_standard=\"\""
