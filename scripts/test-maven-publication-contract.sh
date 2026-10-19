#!/usr/bin/env bash
# The Maven repository a host resolves Migo from is the release, signed, and nothing else.
#
# Until this gate the Android SDK reached a host as a file someone downloaded from a
# release page and committed into their app: no coordinates, no checksum their build
# checked, no signature. A Maven repository fixes that and opens the ways a published
# SDK goes wrong without any build noticing -- a POM a version behind release/VERSION,
# a debug or single-ABI intermediate published beside the real AAR, a checksum or a
# signature that does not cover its file, an AAR that is not the one the release
# attested because something rebuilt it, a sources jar carrying test code. So
# scripts/lib/maven_repo_check.py holds a staged repository to exactly what
# scripts/stage-maven-repo.py promises, and this gate holds the checker to catching each
# of those: it stages fixture AARs under a throwaway key, proves the result passes and
# restages byte for byte, then breaks one property per copy and requires the checker to
# name it. A checker nobody has seen fail is one nobody should trust.
#
# With --aar and --nojni-aar it also stages the real AARs into --out (dist/maven by
# default) and holds that to the same check, which is what the CI build job runs before
# a Gradle app resolves the SDK from it.
#
# The key is generated per run and lives in a temporary GNUPGHOME: it proves the signing
# path and the checker's verification, and its signatures are worth nothing to anyone
# else. A release signs with the publisher's key (F-01).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE="$ROOT/scripts/stage-maven-repo.py"
CHECK="$ROOT/scripts/lib/maven_repo_check.py"
VERSION="$(tr -d '[:space:]' < "$ROOT/release/VERSION")"
GROUP_DIR="com/minigame-labs"

REAL_AAR="" REAL_NOJNI="" REAL_OUT="$ROOT/dist/maven"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --aar) REAL_AAR="$2"; shift 2 ;;
        --nojni-aar) REAL_NOJNI="$2"; shift 2 ;;
        --out) REAL_OUT="$2"; shift 2 ;;
        *) echo "usage: $0 [--aar AAR --nojni-aar AAR [--out DIR]]" >&2; exit 2 ;;
    esac
done
if [[ -n "$REAL_AAR" && -z "$REAL_NOJNI" ]] || [[ -z "$REAL_AAR" && -n "$REAL_NOJNI" ]]; then
    echo "error: --aar and --nojni-aar go together" >&2
    exit 2
fi

WORK="$(mktemp -d)"
cleanup() {
    gpgconf --homedir "$WORK/gnupg" --kill all >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

failures=0
fail() { printf '\033[0;31m[FAIL] %s\033[0m\n' "$*" >&2; failures=$((failures + 1)); }
pass() { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }

command -v gpg >/dev/null || { echo "error: gpg is required; this gate cannot check signatures" >&2; exit 1; }
export GNUPGHOME="$WORK/gnupg"
mkdir -m 700 "$GNUPGHOME"
gpg --batch --pinentry-mode loopback --passphrase '' \
    --quick-gen-key 'migo publication contract <contract@example.invalid>' ed25519 sign 1d \
    >/dev/null 2>&1
KEY="$(gpg --batch --list-secret-keys --with-colons 2>/dev/null | awk -F: '/^fpr/ {print $10; exit}')"
[[ -n "$KEY" ]] || { echo "error: could not create the contract's signing key" >&2; exit 1; }

stage() { python3 "$STAGE" --aar "$1" --nojni-aar "$2" --out "$3" --gpg-key "$KEY" "${@:4}"; }
check() { python3 "$CHECK" --repo "$1" --aar "$2" --nojni-aar "$3" --gnupghome "$GNUPGHOME"; }

# Two AARs that are zips like the real ones and differ from each other, so an
# artifact staged from the wrong input is caught by the bytes check.
python3 - "$WORK" <<'PY'
import pathlib, sys, zipfile
work = pathlib.Path(sys.argv[1])
for name, extra in (("fixture.aar", {"jni/arm64-v8a/libmigo.so": b"\x7fELF engine"}), ("fixture-nojni.aar", {})):
    with zipfile.ZipFile(work / name, "w") as aar:
        aar.writestr("AndroidManifest.xml", "<manifest package=\"com.migo.runtime\"/>")
        aar.writestr("classes.jar", b"PK\x05\x06" + b"\x00" * 18)
        for entry, data in extra.items():
            aar.writestr(entry, data)
PY
AAR="$WORK/fixture.aar"
NOJNI="$WORK/fixture-nojni.aar"

# ------------------------------------------------------------
# A fresh staging passes, restages to the same bytes, and bundles to exactly its tree.
# ------------------------------------------------------------
stage "$AAR" "$NOJNI" "$WORK/repo" --bundle "$WORK/bundle.zip"
if output="$(check "$WORK/repo" "$AAR" "$NOJNI")"; then
    pass "a fresh staging is the repository the release promises"
else
    fail "a fresh staging fails its own check:"; printf '%s\n' "$output" >&2
fi
count="$(find "$WORK/repo" -type f | wc -l)"
if [[ "$count" -eq 48 ]]; then
    pass "two artifacts, four files each, four checksums and a signature apiece: 48 files"
else
    fail "a staging holds $count files, not 48"
fi

stage "$AAR" "$NOJNI" "$WORK/again"
if diff -r --exclude='*.asc' "$WORK/repo" "$WORK/again" >/dev/null; then
    pass "staging the same inputs twice gives the same bytes, signatures aside"
else
    fail "two stagings of the same inputs differ"
fi

bundled="$(python3 -c 'import sys, zipfile; print("\n".join(sorted(zipfile.ZipFile(sys.argv[1]).namelist())))' "$WORK/bundle.zip")"
tree="$(cd "$WORK/repo" && find . -type f | sed 's|^\./||' | sort)"
if [[ "$bundled" == "$tree" ]]; then
    pass "the Central bundle is the tree, rooted at the group"
else
    fail "the Central bundle does not hold exactly the staged tree"
fi

# ------------------------------------------------------------
# Each way a repository can be wrong is caught, by name.
#
# Every case starts from a copy of the passing repository and breaks one property.
# Where breaking it would also break a checksum or a signature, those are rewritten
# to match, so the case can only fail for the property it is about.
# ------------------------------------------------------------
reseal() {  # rewrite a file's checksums and signature after changing it
    local file="$1"
    for algorithm in md5 sha1 sha256 sha512; do
        python3 -c 'import hashlib, sys; print(hashlib.new(sys.argv[1], open(sys.argv[2], "rb").read()).hexdigest(), end="")' \
            "$algorithm" "$file" > "$file.$algorithm"
    done
    gpg --batch --yes --armor --detach-sign --local-user "$KEY" --output "$file.asc" "$file" 2>/dev/null
}
expect_refused() {
    local name="$1" says="$2"
    local output status=0
    output="$(check "$WORK/case" "$AAR" "$NOJNI")" || status=$?
    if [[ "$status" -ne 0 ]] && grep -qF -- "$says" <<< "$output"; then
        pass "$name is refused: $(grep -F -- "$says" <<< "$output" | head -1)"
    else
        fail "$name is not refused with '$says' (exit $status):"; printf '%s\n' "$output" >&2
    fi
}
fresh_case() { rm -rf "$WORK/case"; cp -a "$WORK/repo" "$WORK/case"; }
DIR="$WORK/case/$GROUP_DIR/migo-android/$VERSION"
BASE="$DIR/migo-android-$VERSION"

fresh_case
sed -i "s|<version>$VERSION</version>|<version>0.0.1</version>|" "$BASE.pom"; reseal "$BASE.pom"
expect_refused "a POM a version away from release/VERSION" "version is '0.0.1'"

fresh_case
python3 - "$BASE.pom" <<'PY'
import re, sys
path = sys.argv[1]
text = open(path).read()
open(path, "w").write(re.sub(r"\s*<scm>.*?</scm>", "", text, flags=re.S))
PY
reseal "$BASE.pom"
expect_refused "a POM without the SCM Central requires" "scm/connection is missing"

fresh_case
cp "$BASE.aar" "$DIR/migo-android-$VERSION-debug.aar"
expect_refused "a debug AAR published beside the real one" "unexpected file"

fresh_case
mkdir -p "$WORK/case/$GROUP_DIR/migo-android-arm64/$VERSION"
cp "$BASE.pom" "$WORK/case/$GROUP_DIR/migo-android-arm64/$VERSION/migo-android-arm64-$VERSION.pom"
expect_refused "an artifact the release does not publish" "unexpected file"

fresh_case
rm "$BASE.aar.asc"
expect_refused "an AAR without its signature" "missing"

fresh_case
cp "$BASE.pom.asc" "$BASE.aar.asc"
expect_refused "a signature over another file" "bad signature over migo-android-$VERSION.aar"

fresh_case
printf '0000' > "$BASE.aar.sha1"
expect_refused "a checksum that is not the file's" "checksum is not the sha1"

fresh_case
printf 'rebuilt' >> "$BASE.aar"; reseal "$BASE.aar"
expect_refused "an AAR that is not the release's, however well signed" "differs from the release AAR"

fresh_case
python3 - "$BASE-sources.jar" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "a") as jar:
    jar.writestr("com/migo/runtime/NetworkPolicyTest.java", "class NetworkPolicyTest {}")
PY
reseal "$BASE-sources.jar"
expect_refused "a sources jar carrying test code" "is not one of the library's main sources"

# ------------------------------------------------------------
# The POM declares no dependencies because the library ships none. The day it gains
# one, every host resolving the SDK would miss it, so that day is this failure.
# ------------------------------------------------------------
shipping="$(sed 's|//.*||' "$ROOT/platforms/android/library/build.gradle" \
    | grep -E '^\s*[A-Za-z]*([Ii]mplementation|[Aa]pi|[Rr]untimeOnly)\b' \
    | grep -vE '^\s*(test|androidTest)[A-Za-z]*' || true)"
if [[ -z "$shipping" ]]; then
    pass "the library ships no dependencies, so a POM that declares none is complete"
else
    fail "the library now ships dependencies the staged POM does not declare:"
    printf '%s\n' "$shipping" >&2
fi

# ------------------------------------------------------------
# The real AARs, when given.
# ------------------------------------------------------------
if [[ -n "$REAL_AAR" ]]; then
    stage "$REAL_AAR" "$REAL_NOJNI" "$REAL_OUT" --bundle "$REAL_OUT-central-bundle.zip"
    if output="$(check "$REAL_OUT" "$REAL_AAR" "$REAL_NOJNI")"; then
        pass "the real AARs stage into $REAL_OUT as the release promises"
    else
        fail "the real AARs stage into a repository that fails its check:"; printf '%s\n' "$output" >&2
    fi
fi

if [[ "$failures" -ne 0 ]]; then
    printf '\033[0;31mMaven publication contract: FAIL (%s)\033[0m\n' "$failures" >&2
    exit 1
fi
echo "Maven publication contract: PASS"
