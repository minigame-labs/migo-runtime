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
# ## What it costs, measured
#
# A real source build, roughly 8-9 minutes per target on a `macos-15` runner
# and 8m42s measured on an Intel Mac, where an Apple build previously downloaded
# a prebuilt and compiled no Skia at all. That is not a guess: the artifact
# shapes say which happened. A downloaded `out/skia` holds `key.txt`,
# `libskia-bindings.a` and `bindings.rs`; a built one holds `args.gn` and
# `build.ninja`. Before this change every Apple target had the first shape, with
# keys like
#
#     319323662b1685a112f5-aarch64-apple-ios-gl-jpegd-jpege-pdf-textlayout
#     319323662b1685a112f5-x86_64-apple-darwin-gl-jpegd-jpege-pdf-textlayout
#
# -- so rust-skia publishes binaries for exactly this feature set on exactly
# these triples, and a download would have succeeded. Which is precisely why
# `FORCE_SKIA_BUILD` is load-bearing rather than belt-and-braces: without it the
# download wins, prints SUCCEEDED, and `skia_gl_standard` never reaches GN.
# (Linux is the opposite case -- a cold build there 404s and source-builds on its
# own -- so the two platforms would have disagreed about whether this file does
# anything.)
#
# Paid EVERY run, on every lane, and that correction is measured: the cache does
# not keep it. The diagnostic lane's job on 2026-09-11 reported
#
#     Cache hit for: v0-rust-apple-sdk-external-frames-diagnostic-...
#     Cache Size: ~549 MB
#     Cache restored successfully
#
# and then compiled skia-bindings four times anyway, with `Build` and
# `ANGLE bring-up` taking 15m57s and 7m58s against 15m41s and 9m06s on the cold
# run before it. `Swatinem/rust-cache` prunes large files out of each crate's
# build-script OUT_DIR, which is exactly what `.github/workflows/apple-sdk.yml`
# already says about `librusty_v8.a` and the `macos-v8` row's missing cache --
# `libskia-bindings.a` is the same family, and that note was in the repository
# before this file was written.
#
# So the number is roughly 8-9 minutes per (target x Skia feature set) per job
# per run, which is what makes publishing our own corrected macOS archives and
# pointing `SKIA_BINARIES_URL` at them the next thing worth doing rather than a
# someday optimisation.
#
# The way to get the speed back without giving up the correction is to publish
# our own corrected macOS archives and point `SKIA_BINARIES_URL` at them -- the
# same shape already used for the pinned ANGLE and V8 archives, component
# manifest included. That is an optimisation, and it is not this change.

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
export SKIA_GN_ARGS="${SKIA_GN_ARGS:+$SKIA_GN_ARGS }skia_gl_standard=\"\""

# ## Downloading the correction instead of rebuilding it
#
# The paragraphs above measured what the source build costs -- 8-9 minutes per
# (target x feature set) per job per RUN, not amortised, because `rust-cache`
# prunes build-script output directories. That is what this section removes.
#
# `contracts/artifact-manifest/apple-skia.lock.json` names archives this project
# built with `skia_gl_standard=""` and published, the same shape already used for
# the pinned ANGLE and V8 archives: a recipe anybody can re-run
# (`scripts/package-apple-skia-binaries.sh`), a machine that runs it
# (`.github/workflows/apple-skia-binaries.yml`), and a lock with sizes and
# sha256s that `scripts/test-apple-skia-pin-contract.sh` checks against what is
# actually published.
#
# `SKIA_BINARIES_URL` is a TEMPLATE -- skia-bindings expands `{tag}` and `{key}`,
# and the key is its own construction from (commit, triple, features). So one
# line serves both darwin triples, and a key it cannot find simply misses and
# source-builds: slower, never wrong.
#
# THE SCOPE IS THIS FILE'S CALLERS, AND THAT IS LOAD-BEARING. iOS must keep
# downloading upstream's prebuilts -- its default `"gles"` is what ANGLE actually
# is, so upstream's iOS archives are correct and this release has none. Pointing
# every Apple target at this URL would 404 on iOS keys and quietly turn a
# download into an 8-minute build. What keeps that from happening is that
# `build-apple-sdk.sh` sources this file only under `--platform macos`, and the
# iOS legs of `apple-sdk.yml` say in their own comments that they deliberately do
# not.
#
# `FORCE_SKIA_BUILD` is no longer set by default, because it is exactly the
# switch that takes the download out of the path. `MIGO_SKIA_FROM_SOURCE=1`
# restores it, and two callers need that: the packaging workflow, which must
# build what it is going to publish, and anybody bisecting Skia itself.
if [ -n "${MIGO_SKIA_FROM_SOURCE:-}" ]; then
    export FORCE_SKIA_BUILD=1
else
    # Read from the lock rather than repeated here: a URL in two places is a URL
    # that gets updated in one of them.
    _migo_skia_lock="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)/contracts/artifact-manifest/apple-skia.lock.json"
    if [ -f "$_migo_skia_lock" ]; then
        SKIA_BINARIES_URL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["url_template"])' "$_migo_skia_lock")"
        export SKIA_BINARIES_URL
    else
        # Fail loud rather than fall back to upstream's archives: those are the
        # ones with SK_ASSUME_GL compiled in, and taking them silently is the
        # original defect returning with a green build.
        echo "apple-skia-gl-env.sh: $_migo_skia_lock is missing, so the corrected" >&2
        echo "  macOS Skia cannot be located. Building from source instead, which is" >&2
        echo "  slow and correct. Set MIGO_SKIA_FROM_SOURCE=1 to silence this." >&2
        export FORCE_SKIA_BUILD=1
    fi
    unset _migo_skia_lock
fi
