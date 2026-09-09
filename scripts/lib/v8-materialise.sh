# shellcheck shell=bash
# Materialising a verified V8 archive under a content-addressed path.
# Location: scripts/lib/v8-materialise.sh
#
# Two defects, one mechanism.
#
# **The shipping Android library was built against unverified bytes.**
# `build-android-so.sh` selected the archive with `[[ -f "$v8_archive" ]]` -- existence,
# not identity -- so whatever sat at `engine/third_party/rusty_v8/<arch>/librusty_v8.a`
# went into `libmigo.so`. The SDK scripts already ran `verify-v8-component` first; the
# script that produces the AAR's native library did not.
#
# **Cargo cannot see an archive replaced in place.** The `v8` crate bundles
# `librusty_v8.a` into its rlib, and cargo reruns that build script when the *value* of
# `RUSTY_V8_ARCHIVE` changes -- not when the file at that path does. `build-linux-sdk.sh`
# carried a sha256 stamp plus `cargo clean -p v8` to force it, after a full debugging
# cycle spent on a staged `libmigo.so` that still contained an allocator shim a V8 rebuild
# had removed. A path that *contains* the hash makes that impossible instead of detected:
# different bytes are a different path, so cargo's own staleness rule is correct, and the
# stamp and the clean are gone rather than maintained.
#
# **A CI cache that keeps build-script fingerprints must not keep this crate's
# build-script outputs.** Stated here because it is a property of what the `v8`
# crate does, and until now it was stated only in comments on three workflow
# jobs -- which is why a fourth job was written without it and failed exactly the
# way those comments describe.
#
# Given `RUSTY_V8_ARCHIVE`, rusty_v8's build script COPIES the archive to
# `target/<triple>/<profile>/gn_out/obj/librusty_v8.a` and records
# `cargo:rustc-link-search` pointing there. `Swatinem/rust-cache` prunes large
# files out of build-script output directories to shrink the cache while keeping
# the fingerprint that says that script is fresh. On the next run cargo replays
# the recorded search path without re-running the copy, and the link fails with
# "could not find native static library `rusty_v8`" -- reliably on the SECOND
# run, which is why it reads as flakiness rather than as a configuration error.
#
# So a job that links V8 either does not cache the target directory, or it is
# broken on every warm run. `pr-ci.yml`'s Android build, `release.yml`'s Android
# build, `build-snapshot.yml` and `apple-sdk.yml`'s `macos-v8` row all say so at
# their cache step; if you are adding a fifth, this is the reason.
#
# The path is not made read-only, deliberately. A hard link shares its inode with
# `third_party`, so `chmod 444` here would make the *producer* fail its next `cp` -- and it
# would buy nothing, because the hash is re-checked on every call, which detects an
# in-place edit that a permission bit would only discourage.

# `${BASH_SOURCE[0]%/*}` rather than a `$(cd ... && pwd)` substitution, so that a
# static reader can follow it. scripts/test-macos-bash32-contract.sh walks the
# source graph of every macOS-facing script, and a path it cannot expand is a file
# it cannot check -- which it reports rather than silently skipping. This helper
# entered that graph when build-apple-sdk.sh began sourcing it, and the two forms
# name the same directory for every way this file is actually sourced.
# shellcheck source=scripts/lib/python-cmd.sh
source "${BASH_SOURCE[0]%/*}/python-cmd.sh"

_v8_mat_err() { printf '  ✗ %s\n' "$*" >&2; }

# Prints the recorded hash for a key out of a component manifest, or fails.
_v8_mat_recorded() {
    local manifest="$1" key="$2"
    "$(python_cmd)" - "$manifest" "$key" <<'PY'
import json, sys

manifest = json.load(open(sys.argv[1]))
try:
    print(manifest["hashes"][sys.argv[2]])
except KeyError:
    sys.exit(f"component manifest has no hashes.{sys.argv[2]}")
PY
}

_v8_mat_sha256() { sha256sum "$1" | cut -d' ' -f1; }

# v8_materialise <v8-dir> <materialise-root>
#
# Verifies `<v8-dir>`'s archive and binding against the `component-manifest.json` beside
# them, then makes them available under `<materialise-root>/<archive-sha256>/` and sets
# `V8_MATERIALISED_ARCHIVE` and `V8_MATERIALISED_BINDING` to those paths.
#
# A target with no committed manifest is refused rather than materialised: without one
# there is nothing to say what the right bytes are, which is the same rule
# `scripts/fetch-v8-archives.sh` states for downloading them.
v8_materialise() {
    local v8_dir="$1" root="$2"
    local archive="$v8_dir/librusty_v8.a"
    local binding="$v8_dir/src_binding.rs"
    local manifest="$v8_dir/component-manifest.json"

    local path
    for path in "$archive" "$binding"; do
        [[ -f "$path" ]] || { _v8_mat_err "missing V8 input: $path"; return 1; }
    done
    if [[ ! -f "$manifest" ]]; then
        _v8_mat_err "no component manifest beside $archive"
        _v8_mat_err "a target with no committed manifest cannot be verified, so it is not built against"
        return 1
    fi

    local want_archive want_binding got_archive got_binding
    want_archive="$(_v8_mat_recorded "$manifest" archive)" || {
        _v8_mat_err "cannot read hashes.archive from $manifest"
        return 1
    }
    want_binding="$(_v8_mat_recorded "$manifest" rust_binding)" || {
        _v8_mat_err "cannot read hashes.rust_binding from $manifest"
        return 1
    }
    got_archive="$(_v8_mat_sha256 "$archive")"
    got_binding="$(_v8_mat_sha256 "$binding")"
    if [[ "$got_archive" != "$want_archive" ]]; then
        _v8_mat_err "$archive does not match its manifest"
        _v8_mat_err "  recorded $want_archive"
        _v8_mat_err "  actual   $got_archive"
        return 1
    fi
    if [[ "$got_binding" != "$want_binding" ]]; then
        _v8_mat_err "$binding does not match its manifest"
        _v8_mat_err "  recorded $want_binding"
        _v8_mat_err "  actual   $got_binding"
        return 1
    fi

    # Keyed on *both* hashes. Keying on the archive alone collides when a binding changes
    # while the archive does not -- two different components would map to one directory, the
    # stale binding there would be refused rather than replaced, and because cargo watches
    # the *value* of RUSTY_V8_SRC_BINDING_PATH it could go on using a v8 crate compiled
    # against the previous binding. The path has to name everything it addresses.
    local dest="$root/$got_archive-$got_binding"
    mkdir -p "$dest" || { _v8_mat_err "cannot create $dest"; return 1; }

    # Hard links rather than copies: the archive is ~120 MB per architecture, and a
    # symlink is not usable because consumers stat the path (a `stat -c %s` LFS-pointer
    # check reads a symlink's own size and rejects it). A copy is the fallback for the
    # one case a link cannot serve, a different filesystem.
    local name
    for name in librusty_v8.a src_binding.rs; do
        local source="$v8_dir/$name" target="$dest/$name"
        if [[ -f "$target" ]]; then
            # Re-checked rather than trusted: the path asserts both hashes, so a file that
            # no longer matches the directory naming it is the one thing this mechanism must
            # not pass on.
            local expected="$got_archive"
            [[ "$name" == "src_binding.rs" ]] && expected="$got_binding"
            if [[ "$(_v8_mat_sha256 "$target")" != "$expected" ]]; then
                _v8_mat_err "$target does not hash to the directory that names it; refusing to reuse it"
                return 1
            fi
            continue
        fi
        ln "$source" "$target" 2>/dev/null || cp "$source" "$target" || {
            _v8_mat_err "cannot materialise $source at $target"
            return 1
        }
    done

    V8_MATERIALISED_ARCHIVE="$dest/librusty_v8.a"
    V8_MATERIALISED_BINDING="$dest/src_binding.rs"
    export V8_MATERIALISED_ARCHIVE V8_MATERIALISED_BINDING
}

# v8_materialise_windows <v8-dir> <materialise-root>
#
# Windows' V8 archive has a shape v8_materialise does not know: `hashes.archive`
# covers `rusty_v8.lib` -- the static build product write-windows-v8-component-
# manifest.py deliberately hashes -- but the actual link uses the DLL's import
# library, `rusty_v8.dll.lib`, instead. See build-windows-sdk.sh's own header
# comment for why: V8 is built with its own libc++ there, and linking the
# static archive puts that libc++ into the same link as Skia's MSVC STL, which
# define std::terminate incompatibly. Widening the manifest schema to cover the
# DLL/import-lib pair too would change the shape every other platform's
# manifest is verified against, for one platform's linking detail; their
# integrity is instead covered "one level up", by the SDK package manifest's
# own artifact hashes over the final migo.dll.
#
# So this verifies rusty_v8.lib + src_binding.rs against the component
# manifest, exactly like v8_materialise, then materialises rusty_v8.dll and
# rusty_v8.dll.lib alongside them into the same content-addressed directory --
# sharing that directory's provenance (all four files come from one fetch of
# one release asset set) even though the manifest carries no independent
# per-file hash for the two it does not cover; a reused directory's copies are
# still checked for internal consistency against the source, the same
# "recorded rather than trusted" rule the manifest-covered files use, just
# against the source's own hash rather than an authoritative recorded one.
# Sets V8_MATERIALISED_ARCHIVE to the materialised rusty_v8.dll.lib -- what the
# link actually consumes -- and V8_MATERIALISED_BINDING as v8_materialise does.
v8_materialise_windows() {
    local v8_dir="$1" root="$2"
    local archive="$v8_dir/rusty_v8.lib"
    local binding="$v8_dir/src_binding.rs"
    local manifest="$v8_dir/component-manifest.json"
    local dll="$v8_dir/rusty_v8.dll"
    local dll_lib="$v8_dir/rusty_v8.dll.lib"

    local path
    for path in "$archive" "$binding" "$dll" "$dll_lib"; do
        [[ -f "$path" ]] || { _v8_mat_err "missing V8 input: $path"; return 1; }
    done
    if [[ ! -f "$manifest" ]]; then
        _v8_mat_err "no component manifest beside $archive"
        _v8_mat_err "a target with no committed manifest cannot be verified, so it is not built against"
        return 1
    fi

    local want_archive want_binding got_archive got_binding
    want_archive="$(_v8_mat_recorded "$manifest" archive)" || {
        _v8_mat_err "cannot read hashes.archive from $manifest"
        return 1
    }
    want_binding="$(_v8_mat_recorded "$manifest" rust_binding)" || {
        _v8_mat_err "cannot read hashes.rust_binding from $manifest"
        return 1
    }
    got_archive="$(_v8_mat_sha256 "$archive")"
    got_binding="$(_v8_mat_sha256 "$binding")"
    if [[ "$got_archive" != "$want_archive" ]]; then
        _v8_mat_err "$archive does not match its manifest"
        _v8_mat_err "  recorded $want_archive"
        _v8_mat_err "  actual   $got_archive"
        return 1
    fi
    if [[ "$got_binding" != "$want_binding" ]]; then
        _v8_mat_err "$binding does not match its manifest"
        _v8_mat_err "  recorded $want_binding"
        _v8_mat_err "  actual   $got_binding"
        return 1
    fi

    local dest="$root/$got_archive-$got_binding"
    mkdir -p "$dest" || { _v8_mat_err "cannot create $dest"; return 1; }

    local name source target expected
    for name in rusty_v8.lib src_binding.rs rusty_v8.dll rusty_v8.dll.lib; do
        source="$v8_dir/$name"
        target="$dest/$name"
        if [[ -f "$target" ]]; then
            expected="$(_v8_mat_sha256 "$source")"
            if [[ "$(_v8_mat_sha256 "$target")" != "$expected" ]]; then
                _v8_mat_err "$target does not hash to its source; refusing to reuse it"
                return 1
            fi
            continue
        fi
        ln "$source" "$target" 2>/dev/null || cp "$source" "$target" || {
            _v8_mat_err "cannot materialise $source at $target"
            return 1
        }
    done

    V8_MATERIALISED_ARCHIVE="$dest/rusty_v8.dll.lib"
    V8_MATERIALISED_BINDING="$dest/src_binding.rs"
    export V8_MATERIALISED_ARCHIVE V8_MATERIALISED_BINDING
}
