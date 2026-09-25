#!/usr/bin/env bash
# =============================================================================
# Contract: the Apple products ship a privacy manifest, and what it declares is
# what the engine does.
#
# Since 2024 an app that links a third-party SDK using a "required reason" API
# is rejected at review unless the SDK's PrivacyInfo.xcprivacy declares the
# category with an approved reason. The failure is at review, weeks after the
# change that caused it, in an integrator's app rather than here -- so the
# declaration has to be checked against the source on every change, in both
# directions:
#
#   1. Both products carry a manifest, it parses, and they are identical. The
#      two lanes link the same engine; two manifests that differ are two
#      answers to one question.
#   2. Package.swift ships each one as a resource of its product. A manifest
#      in the tree and not in the bundle is a manifest nobody reads.
#   3. Tracking is off and no data is collected. Migo sends nothing anywhere.
#   4. Every required-reason category the sources use is declared -- the check
#      that catches the NEXT change, the one that adds `UserDefaults` or a disk
#      space query, which is the failure mode.
#   5. Every declared category is still used, or is declared unconditionally
#      for a stated reason (system boot time: V8, Skia and ANGLE read
#      `mach_absolute_time` in code this tree does not contain as source). A
#      stale declaration is a claim about the app's behaviour that is false.
#
# The sweep is the engine's Rust and the Apple Swift and Objective-C, derived
# from the tree rather than listed here.
# =============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPLE="$ROOT/platforms/apple"
MANIFESTS=(
  "$APPLE/Sources/MigoApplePerformancePlus/PrivacyInfo.xcprivacy"
  "$APPLE/Sources/MigoMacV8/PrivacyInfo.xcprivacy"
)

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# 1. Present, parseable, identical.
for manifest in "${MANIFESTS[@]}"; do
  [[ -f "$manifest" ]] || fail "missing privacy manifest: ${manifest#"$ROOT"/}"
done
cmp -s "${MANIFESTS[0]}" "${MANIFESTS[1]}" \
  || fail "the two products' privacy manifests differ; they link the same engine and must declare the same thing"

declared="$(python3 - "${MANIFESTS[0]}" <<'PY'
import plistlib, sys
with open(sys.argv[1], "rb") as handle:
    try:
        plist = plistlib.load(handle)
    except Exception as error:  # a manifest review cannot parse is a rejection
        print(f"PARSE-ERROR {error}")
        sys.exit(0)
if plist.get("NSPrivacyTracking") is not False:
    print("TRACKING-NOT-FALSE")
if plist.get("NSPrivacyTrackingDomains") not in ([],):
    print("TRACKING-DOMAINS")
if plist.get("NSPrivacyCollectedDataTypes") not in ([],):
    print("COLLECTED-DATA")
for entry in plist.get("NSPrivacyAccessedAPITypes", []):
    reasons = entry.get("NSPrivacyAccessedAPITypeReasons") or []
    print(f"{entry.get('NSPrivacyAccessedAPIType')} {','.join(reasons) or 'NO-REASON'}")
PY
)"
grep -q '^PARSE-ERROR' <<<"$declared" && fail "the privacy manifest does not parse: $declared"

# 3. Nothing tracked, nothing collected.
grep -q '^TRACKING-NOT-FALSE' <<<"$declared" && fail "NSPrivacyTracking must be false: Migo tracks nothing"
grep -q '^TRACKING-DOMAINS' <<<"$declared" && fail "NSPrivacyTrackingDomains must be empty"
grep -q '^COLLECTED-DATA' <<<"$declared" && fail "NSPrivacyCollectedDataTypes must be empty: the engine collects nothing"
if grep -q 'NO-REASON' <<<"$declared"; then
  fail "a declared API category has no reason: $(grep NO-REASON <<<"$declared" | tr '\n' ' ')"
fi

# 2. Shipped as a resource of each product.
for target in MigoApplePerformancePlus MigoMacV8; do
  python3 - "$APPLE/Package.swift" "$target" <<'PY' || fail "Package.swift does not ship $target's PrivacyInfo.xcprivacy as a resource"
import re, sys
text = open(sys.argv[1]).read()
# The target, not the product of the same name that lists it.
match = re.search(r'\.target\(\s*name:\s*"' + re.escape(sys.argv[2]) + '"', text)
if not match:
    sys.exit(1)
start = match.end()
# To the next target declaration: a target's own arguments contain "),".
ends = [i for i in (text.find(".target(", start), text.find(".testTarget(", start),
                    text.find(".binaryTarget(", start)) if i >= 0]
block = text[start:min(ends) if ends else len(text)]
sys.exit(0 if re.search(r'\.process\("PrivacyInfo\.xcprivacy"\)', block) else 1)
PY
done

# 4 and 5. What the sources use against what is declared.
#
# The sweep excludes tests: a test that reads a timestamp is not shipped.
sources() {
  find "$ROOT/engine/crates" -name '*.rs' -not -path '*/tests/*' -not -name '*_tests.rs' -print0
  find "$APPLE/Sources" "$APPLE/core/Sources" \( -name '*.swift' -o -name '*.m' -o -name '*.mm' \) -print0
}
uses() {
  sources | xargs -0 grep -lE "$1" 2>/dev/null | head -1
}

declares() {
  grep -q "^$1 " <<<"$declared"
}

check_category() {
  local category="$1" pattern="$2" always="$3"
  local where
  where="$(uses "$pattern" || true)"
  if [[ -n "$where" ]] && ! declares "$category"; then
    fail "$category is used (${where#"$ROOT"/}) and not declared in the privacy manifest; add it with an approved reason"
  fi
  if [[ -z "$where" && "$always" != "always" ]] && declares "$category"; then
    fail "$category is declared and nothing in the sources uses it any more; a stale declaration is a false claim"
  fi
  if [[ "$always" == "always" ]] && ! declares "$category"; then
    fail "$category must be declared: V8, Skia and ANGLE use it in code that is not in this tree as source"
  fi
}

check_category NSPrivacyAccessedAPICategoryFileTimestamp \
  '\.(modified|accessed|created)\(\)|st_mtime|st_atime|creationDate|modificationDate|NSFileModificationDate' ""
check_category NSPrivacyAccessedAPICategorySystemBootTime \
  'mach_absolute_time|systemUptime|kern\.boottime' always
check_category NSPrivacyAccessedAPICategoryDiskSpace \
  'statvfs|statfs|volumeAvailableCapacity|NSFileSystemFreeSize|NSFileSystemSize|available_space\(' ""
check_category NSPrivacyAccessedAPICategoryUserDefaults \
  'UserDefaults|NSUserDefaults|CFPreferences' ""
check_category NSPrivacyAccessedAPICategoryActiveKeyboards \
  'activeInputModes' ""

echo "PASS: the Apple privacy manifests are shipped, identical, and declare exactly what the sources use"
