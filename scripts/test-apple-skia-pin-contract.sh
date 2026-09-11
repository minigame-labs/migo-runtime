#!/usr/bin/env bash
# The corrected macOS Skia is where the lock says, and is the bytes the lock names.
#
# WHY THIS GATE EXISTS. Upstream's macOS Skia prebuilts are compiled with
# `skia_gl_standard = "gl"` -> `SK_ASSUME_GL=1`, which compiles out the ES
# interface assembler. The only GL on Apple is ANGLE, which is ES, so a macOS
# build that links those prebuilts cannot build a `GrDirectContext` for any
# Canvas2D surface -- and does so SILENTLY, because WebGL never goes through
# Skia and nothing else asks. That defect shipped, was green on every lane, and
# took three machines to find.
#
# `scripts/apple-skia-gl-env.sh` now points `SKIA_BINARIES_URL` at archives this
# project built with `skia_gl_standard=""`. A download is only as good as what
# is at the other end of it, so this checks the other end: that both assets
# exist at the tag the lock names, and that their bytes are the bytes it names.
# An asset that 404s costs eight minutes; an asset that has been REPLACED costs
# the defect coming back with a green build.
#
# Host-only: curl and a hash tool. No Apple toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LOCK="contracts/artifact-manifest/apple-skia.lock.json"
c_info() { echo -e "\033[0;36m[skia-pin] $*\033[0m"; }
c_ok()   { echo -e "\033[0;32m[skia-pin] $*\033[0m"; }
fail()   { echo -e "\033[0;31m[skia-pin] FAIL $*\033[0m" >&2; exit 1; }

[[ -f "$LOCK" ]] || fail "$LOCK is missing; apple-skia-gl-env.sh reads its URL from there"

python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$LOCK" \
    || fail "$LOCK is not valid JSON"

# The env fragment must take its URL from this file and not carry a copy. A URL
# in two places is a URL that gets updated in one of them.
grep -q 'apple-skia.lock.json' scripts/apple-skia-gl-env.sh \
    || fail "scripts/apple-skia-gl-env.sh no longer reads the URL from $LOCK"
if grep -qE 'SKIA_BINARIES_URL="https' scripts/apple-skia-gl-env.sh; then
    fail "scripts/apple-skia-gl-env.sh hard-codes a URL; it must read $LOCK"
fi

# iOS must not appear. Its default is already "gles", upstream's iOS prebuilts
# are correct, and this release has none -- an iOS entry here would be a key
# that 404s and an 8-minute build that reads as a cache miss.
if python3 -c '
import json, sys
lock = json.load(open(sys.argv[1]))
sys.exit(0 if any("ios" in t for t in lock["targets"]) else 1)' "$LOCK"; then
    fail "$LOCK names an iOS target. iOS needs no correction and has no archive here."
fi

RELEASE="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["release"])' "$LOCK")"
c_info "release $RELEASE"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
    else fail "no sha256sum or shasum on this machine"
    fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

checked=0
while IFS='|' read -r triple asset size sha; do
    [[ -n "$triple" ]] || continue
    c_info "$triple"
    url="$RELEASE/$asset"
    # `--fail` so a 404 page is an error rather than a file whose hash does not
    # match, which reports the wrong problem.
    curl -sSL --fail -o "$WORK/$asset" "$url" \
        || fail "$url did not download. Re-run .github/workflows/apple-skia-binaries.yml with publish: true."
    actual_size="$(wc -c < "$WORK/$asset" | tr -d ' ')"
    [[ "$actual_size" == "$size" ]] \
        || fail "$asset is $actual_size bytes and the lock says $size"
    actual_sha="$(sha256_of "$WORK/$asset")"
    [[ "$actual_sha" == "$sha" ]] \
        || fail "$asset hashes to $actual_sha and the lock says $sha.
      A published asset that changed under a fixed tag is the defect this gate exists for:
      the macOS build downloads it without looking, and an upstream-configured Skia there
      brings back a silent Canvas2D failure on every macOS surface."
    c_ok "  $asset  $actual_size bytes  sha256 matches"
    checked=$((checked + 1))
done < <(python3 -c '
import json, sys
lock = json.load(open(sys.argv[1]))
for triple, entry in sorted(lock["targets"].items()):
    print("|".join([triple, entry["asset"], str(entry["size_bytes"]), entry["sha256"]]))' "$LOCK")

# Both darwin triples, named rather than counted from the file: a lock that lost
# an entry would otherwise pass with one.
for required in aarch64-apple-darwin x86_64-apple-darwin; do
    python3 -c '
import json, sys
sys.exit(0 if sys.argv[2] in json.load(open(sys.argv[1]))["targets"] else 1)' "$LOCK" "$required" \
        || fail "$LOCK has no entry for $required, and macOS ships both slices"
done

(( checked == 2 )) || fail "checked $checked archives, expected 2"
c_ok "both macOS Skia archives are published and match the lock"
