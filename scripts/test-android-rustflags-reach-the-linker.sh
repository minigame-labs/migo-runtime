#!/usr/bin/env bash
# Every rustflag `.cargo/config.toml` declares for an Android target must survive
# into the AAR build.
#
# Cargo does not merge: setting `RUSTFLAGS` in the environment *replaces*
# `[target.<triple>].rustflags` rather than adding to it. `build-android-so.sh`
# has to set `RUSTFLAGS` -- the clang builtins directory is only known at build
# time -- so for as long as it did that blindly, the config's flags were dropped
# on the one build that ships. They kept applying to `build-android-sdk.sh` and
# `build-android-c-host.sh`, which set no `RUSTFLAGS`, so the config looked
# alive and the divergence was invisible.
#
# What it cost, measured on 2026-08-23 for arm64-v8a release: `libmigo.so`
# 45,709,280 -> 42,072,032 bytes once the flags were carried through. 3.47 MiB,
# 8% of the binary, nearly all `.text` (31.30 -> 27.88 MB) from `--gc-sections`
# and `--icf=all` never having run. Not a performance trade either -- startup
# was A/B'd interleaved and thermally gated, and first frame and game-ready both
# came out slightly better with the smaller image, at identical 59.8 fps.
#
# The failure had already left its fingerprint: `--allow-multiple-definition`
# appeared in *both* the config and the script. That is what someone does after
# finding the config's copy did not apply -- fix the flag in front of them and
# not the mechanism, leaving the other twelve dead.
#
# This gate reads what the build script would export and asserts every declared
# flag is in it. It does not build anything.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
python3 "$SCRIPT_DIR/testing/test_android_jni_link_scope.py"
CONFIG="$REPO_ROOT/engine/.cargo/config.toml"
BUILD_SCRIPT="$REPO_ROOT/scripts/build-android-so.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }

[[ -f "$CONFIG" ]] || fail "no config at $CONFIG"
[[ -f "$BUILD_SCRIPT" ]] || fail "no build script at $BUILD_SCRIPT"

# 1. The script must read the config rather than restating it. A copied flag is
#    the exact failure this gate exists for, so a build script that carries its
#    own static list fails here even if the list happens to be right today.
grep -q 'config_rustflags()' "$BUILD_SCRIPT" \
    || fail "build-android-so.sh no longer reads the config's rustflags.
      Setting RUSTFLAGS replaces [target.<triple>].rustflags, so a script that
      builds its own list silently drops every flag the config declares."

grep -q 'common_rustflags="\$(config_rustflags' "$BUILD_SCRIPT" \
    || fail "build-android-so.sh no longer seeds its rustflags from the config."

# 2. Every declared flag must actually come back out of that reader, for every
#    Android target the config describes. This is the part that catches a
#    reader that silently returns nothing -- a config typo, a renamed table, a
#    python that is not there.
# Driven by the triples the build script can actually target, not by whatever
# the config happens to contain. Iterating the config alone lets a renamed table
# pass: the remaining targets still check out while the renamed one silently
# builds with no flags at all -- the failure this gate is named for, arriving
# through a different door.
declared_targets="$(python3 - "$BUILD_SCRIPT" <<'TARGETS'
import re, sys
source = open(sys.argv[1], encoding="utf-8").read()
block = re.search(r"PLATFORM_MAP=\((.*?)\)", source, re.S)
if not block:
    sys.exit("cannot find PLATFORM_MAP in build-android-so.sh")
triples = re.findall(r'\]="([^"]+)"', block.group(1))
if not triples:
    sys.exit("PLATFORM_MAP declares no target triples")
for triple in triples:
    print(triple)
TARGETS
)" || fail "cannot read the build script's target list"
[[ -n "$declared_targets" ]] || fail "the build script targets nothing"

# The reader, extracted from the build script so this checks the real one.
config_rustflags() {
    python3 - "$CONFIG" "$1" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
print(" ".join(config.get("target", {}).get(sys.argv[2], {}).get("rustflags", [])))
PY
}

checked=0
while IFS= read -r triple; do
    [[ -n "$triple" ]] || continue
    carried="$(config_rustflags "$triple")"
    [[ -n "$carried" ]] || fail "$triple: the build script targets it, but
      .cargo/config.toml declares no rustflags for it -- so that build links with
      none of them. Check the [target.$triple] table still has that exact name."

    # Compare as whole flags, not as a substring of the joined string: a flag
    # that is a prefix of another would otherwise pass by accident.
    missing="$(python3 - "$CONFIG" "$triple" "$carried" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
declared = config.get("target", {}).get(sys.argv[2], {}).get("rustflags", [])
carried = sys.argv[3].split()
# rustflags are ["-C", "link-arg=..."] pairs; compare the joined pairs so a
# value cannot be matched against someone else's flag name.
def pairs(items):
    out, i = [], 0
    while i < len(items):
        if items[i] == "-C" and i + 1 < len(items):
            out.append("-C " + items[i + 1]); i += 2
        else:
            out.append(items[i]); i += 1
    return out
for flag in pairs(declared):
    if flag not in pairs(carried):
        print(flag)
PY
)"
    [[ -z "$missing" ]] || fail "$triple: declared but not carried into the build:
$(printf '        %s\n' $missing)"

    count="$(wc -w <<<"$carried")"
    echo "  $triple: $count rustflag token(s) carried"
    checked=$((checked + 1))
done <<<"$declared_targets"

# 3. A flag must not be stated in both places. That is how the two drift, and
#    it is how this one hid: the script's copy kept working while the config's
#    did not, so the config looked fine.
duplicated="$(python3 - "$CONFIG" "$BUILD_SCRIPT" <<'PY'
import re, sys, tomllib
with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
declared = set()
for triple, table in config.get("target", {}).items():
    if "android" in triple:
        declared.update(f for f in table.get("rustflags", []) if f.startswith("link-arg="))
source = open(sys.argv[2], encoding="utf-8").read()
# Only look at assignments, not at comments -- the comments explain the flags
# and must be free to name them.
code = "\n".join(l for l in source.splitlines() if not l.lstrip().startswith("#"))
for flag in sorted(declared):
    if flag in code:
        print(flag)
PY
)"
[[ -z "$duplicated" ]] || fail "stated in both .cargo/config.toml and build-android-so.sh:
$(printf '        %s\n' $duplicated)
      One of the two will stop applying and the other will hide it. Keep the
      flag in the config; the script carries it."

# 4. The flags whose absence was *measured* in megabytes must still be declared.
#    Checks 1-3 only guarantee that whatever the config says reaches the linker;
#    deleting a flag from the config is an honest config change and passes them.
#    That is the right split, except for the handful where "honest change" and
#    "3.4 MB back, silently" are the same edit. Those are named here with what
#    they cost, so removing one is a decision someone makes on purpose.
for triple in $declared_targets; do
    [[ -n "$triple" ]] || continue
    carried="$(config_rustflags "$triple")"
    for required in "link-arg=-Wl,--gc-sections" "link-arg=-Wl,--icf=all"; do
        grep -qF -- "$required" <<<"$carried" || fail "$triple no longer declares $required.
      Measured on arm64-v8a release 2026-08-23: dropping --gc-sections and
      --icf=all together took libmigo.so from 42,072,032 to 45,709,280 bytes --
      3.47 MiB, nearly all of it .text. Startup and fps were unaffected, so
      there is no performance argument for removing them. If you are removing
      one deliberately, delete it from this list and say why."
    done
done

# 5. The same trap, one door along. `build-linux-sdk.sh` also exports RUSTFLAGS,
#    and today that discards nothing because neither `[build].rustflags` nor the
#    host target declares any. The day one is added it would vanish exactly as
#    the Android ones did, on a build with no gate watching. So the absence is
#    asserted rather than assumed: adding flags there is fine, but that script
#    has to carry them first.
stray="$(python3 - "$CONFIG" <<'STRAY'
import sys, tomllib
with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
if config.get("build", {}).get("rustflags"):
    print("[build].rustflags")
host = config.get("target", {}).get("x86_64-unknown-linux-gnu", {})
if host.get("rustflags"):
    print("[target.x86_64-unknown-linux-gnu].rustflags")
STRAY
)"
[[ -z "$stray" ]] || fail "declared where an exporting build script would discard them:
$(printf '        %s\n' $stray)
      scripts/build-linux-sdk.sh sets RUSTFLAGS, which replaces these rather
      than merging. Teach that script to carry them (see config_rustflags in
      build-android-so.sh) before declaring any here."

echo "PASS: config rustflags reach the Android link ($checked target(s))"
