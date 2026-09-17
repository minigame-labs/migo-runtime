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

# The build verifies the bytes it links, and skia-bindings never fetches them.
#
# Checking the release (below) says what is published. It said nothing about
# what a build INSTALLED, and on 2026-09-17 those differed: skia-bindings caches
# downloads by file name and resumes them, our archives carry upstream's names,
# and the macOS CI row that restores a cached target directory "resumed" into
# upstream's archive and linked desktop-GL Skia -- `make_gl` returned none on
# every Canvas2D surface while the row that caches nothing passed with the same
# lock. The fix is scripts/materialise-apple-skia.py, which fetches the archives
# whole, checks them against this lock, and hands skia-bindings a file:// URL.
# Three things would bring the defect back quietly, and each is checked here:
#
#   1. The fragment exporting the lock's https template again, or anything but
#      the materialiser's output.
#   2. The materialiser accepting bytes the lock does not name -- fresh from the
#      network, or already on disk from an earlier run or a cache.
#   3. A caller sourcing the fragment without the triples it builds, which the
#      fragment refuses rather than guessing.
ENV_FRAGMENT="scripts/apple-skia-gl-env.sh"
MATERIALISER="scripts/materialise-apple-skia.py"
grep -q 'materialise-apple-skia.py' "$ENV_FRAGMENT" \
    || fail "$ENV_FRAGMENT no longer takes SKIA_BINARIES_URL from $MATERIALISER"
if grep -qE 'url_template|SKIA_BINARIES_URL="?https?:' "$ENV_FRAGMENT"; then
    fail "$ENV_FRAGMENT hands skia-bindings a network URL; its downloader resumes into stale same-named files"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

python3 - "$MATERIALISER" "$WORK/materialise" <<'PY' || fail "$MATERIALISER does not hold archives to the lock"
import hashlib, json, os, pathlib, subprocess, sys

materialiser, work = sys.argv[1], pathlib.Path(sys.argv[2])
source = work / "release"
source.mkdir(parents=True)
targets = {}
for triple in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
    asset = f"skia-binaries-0000-{triple}-gl.tar.gz"
    body = (triple * 4096).encode()
    (source / asset).write_bytes(body)
    targets[triple] = {"asset": asset, "size_bytes": len(body), "sha256": hashlib.sha256(body).hexdigest()}
lock = work / "lock.json"
lock.write_text(json.dumps({"url_template": f"file://{source}/skia-binaries-{{key}}.tar.gz", "targets": targets}))
root = work / "archives"
problems = []

def run(*triples):
    return subprocess.run([sys.executable, materialiser, "--lock", str(lock), "--root", str(root), *triples],
                          capture_output=True, text=True)

first = run("aarch64-apple-darwin", "x86_64-apple-darwin")
template = first.stdout.strip()
if first.returncode != 0 or not template.startswith("file://") or "{key}" not in template:
    problems.append(f"a lock-matching release was not materialised: {first.returncode} {first.stdout!r} {first.stderr!r}")
else:
    directory = pathlib.Path(template[len("file://"):]).parent
    for triple, entry in targets.items():
        installed = directory / entry["asset"]
        if not installed.is_file() or hashlib.sha256(installed.read_bytes()).hexdigest() != entry["sha256"]:
            problems.append(f"{entry['asset']} was not installed with the lock's bytes")

    # A file already there that is a PREFIX of the right one plus someone else's
    # bytes -- the shape a resumed download leaves -- must be replaced, not kept.
    stale = directory / targets["x86_64-apple-darwin"]["asset"]
    stale.write_bytes(b"upstream" * 5000)
    again = run("x86_64-apple-darwin")
    if again.returncode != 0 or hashlib.sha256(stale.read_bytes()).hexdigest() != targets["x86_64-apple-darwin"]["sha256"]:
        problems.append("a file already on disk that did not match the lock was kept")

    # A published asset that changed under the lock is refused, and leaves nothing.
    (source / targets["aarch64-apple-darwin"]["asset"]).write_bytes(b"replaced" * 5000)
    (directory / targets["aarch64-apple-darwin"]["asset"]).unlink()
    replaced = run("aarch64-apple-darwin")
    if replaced.returncode == 0:
        problems.append("an asset whose bytes differ from the lock was accepted")
    if any(directory.iterdir()) and (directory / targets["aarch64-apple-darwin"]["asset"]).exists():
        problems.append("a refused asset was left where skia-bindings would read it")
    if any(p.name.startswith(".") for p in directory.iterdir()):
        problems.append("a refused download left a partial file behind")

    # A different lock is a different directory, so cargo reruns skia-bindings.
    # Asked for the triple whose release still matches, so the answer is a URL.
    (source / targets["aarch64-apple-darwin"]["asset"]).write_bytes(("aarch64-apple-darwin" * 4096).encode())
    other = dict(targets)
    other["x86_64-apple-darwin"] = dict(other["x86_64-apple-darwin"], sha256="0" * 64)
    lock.write_text(json.dumps({"url_template": f"file://{source}/skia-binaries-{{key}}.tar.gz", "targets": other}))
    moved = run("aarch64-apple-darwin")
    if moved.returncode != 0 or not moved.stdout.strip().startswith("file://"):
        problems.append(f"the release matching its lock entry was not materialised: {moved.stderr!r}")
    elif moved.stdout.strip() == template:
        problems.append("a lock naming different archives produced the same SKIA_BINARIES_URL")

unknown = run("aarch64-apple-ios")
if unknown.returncode == 0:
    problems.append("a triple the lock does not have was accepted")

for problem in problems:
    print("  " + problem, file=sys.stderr)
sys.exit(1 if problems else 0)
PY
c_ok "the materialiser installs only the lock's bytes, replaces stale ones and refuses replaced ones"

if bash -c '. scripts/apple-skia-gl-env.sh' >/dev/null 2>&1; then
    fail "$ENV_FRAGMENT accepted being sourced with no target triples"
fi
while IFS= read -r site; do
    fail "sources $ENV_FRAGMENT without the triples it builds: $site"
done < <(grep -nE '^[^#]*\. [^ ]*apple-skia-gl-env\.sh[" ]*($|\|\|)' \
    scripts/*.sh .github/workflows/*.yml | grep -v "^$ENV_FRAGMENT:" || true)
c_ok "every caller names the triples it builds"

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

# The tag, the release URL and the download template name one release, and the
# floor the lock records is the contract's. Three spellings of one location drift
# one at a time; a floor written here and not checked is the defect the
# `-macos<floor>` tag suffix exists to make visible.
FLOOR="$(bash scripts/build-apple-sdk.sh --print-deployment-target macos)" \
    || fail "build-apple-sdk.sh could not report the macOS deployment floor"
python3 - "$LOCK" "$FLOOR" <<'PY' || fail "$LOCK does not name one release built against the macOS $FLOOR floor"
import json, sys
lock = json.load(open(sys.argv[1]))
floor = sys.argv[2]
tag = lock["tag"]
problems = []
if not lock["release"].endswith("/" + tag):
    problems.append(f"release {lock['release']} is not the release for tag {tag}")
if not lock["url_template"].startswith(lock["release"] + "/"):
    problems.append(f"url_template {lock['url_template']} does not download from {lock['release']}")
if not tag.endswith("-macos" + floor):
    problems.append(f"tag {tag} does not carry the -macos{floor} suffix of the floor it was built for")
if lock["source"].get("macos_deployment_target") != floor:
    problems.append(f"source.macos_deployment_target is {lock['source'].get('macos_deployment_target')!r}, the contract says {floor}")
for problem in problems:
    print("  " + problem, file=sys.stderr)
sys.exit(1 if problems else 0)
PY
c_ok "the lock names one release, built against the macOS $FLOOR floor"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
    else fail "no sha256sum or shasum on this machine"
    fi
}

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
      A published asset that changed under a fixed tag breaks every macOS build: the
      materialiser refuses bytes the lock does not name, so this is found here first."
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
