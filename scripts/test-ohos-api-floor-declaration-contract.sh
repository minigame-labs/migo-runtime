#!/usr/bin/env bash
# =============================================================================
# Contract: the OpenHarmony API floor is declared consistently, and never below
# what the symbol audit actually proved.
#
# Three statements exist about the same number and nothing tied them together:
#
#   * platforms/openharmony/build-profile.json5 -- compatibleSdkVersion, the
#     floor the HarmonyOS toolchain enforces on the app package;
#   * scripts/build-ohos-sdk.sh -- MIN_OHOS_API, the floor the C SDK package
#     declares to consumers in its manifest and README;
#   * the sysroot scripts/test-ohos-symbol-floor.sh audits against, which is the
#     only one backed by evidence.
#
# The first two are hand-written copies of one decision, so they drift silently:
# raising the product floor in one place leaves the other advertising support
# the build no longer gates. The third is a different claim and must not be
# copied into the other two -- see below.
#
# WHY THE AUDIT SYSROOT IS *NOT* THE NUMBER TO PUBLISH.
# The audit resolves imports against an API 18 sysroot and proves no import
# postdates it. That is deliberately stricter than the declared floor of 20:
# lacking per-API stub libraries, an older sysroot is the only objective version
# evidence OpenHarmony offers, and being stricter is the safe direction. It is
# not an argument for publishing 18. Nothing has ever been executed on an API 18
# device, and a symbol existing is not the same claim as behaviour being correct
# there. So the invariant is an inequality, not an equality: the published floor
# must be at least as high as the audited one, and lowering it is a decision that
# needs device evidence rather than a symbol table.
#
# Fails closed: an unreadable or unparsable declaration is an error, not a skip,
# and the run reports every value it compared so an empty run is visible.
# =============================================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PROFILE="$REPO_ROOT/platforms/openharmony/build-profile.json5"
SDK_SCRIPT="$REPO_ROOT/scripts/build-ohos-sdk.sh"

err() { echo -e "\033[0;31m[ohos-floor-decl] $*\033[0m" >&2; }
ok()  { echo -e "\033[0;32m[ohos-floor-decl] $*\033[0m"; }
info(){ echo -e "\033[0;36m[ohos-floor-decl] $*\033[0m"; }

for f in "$PROFILE" "$SDK_SCRIPT"; do
    [[ -f "$f" ]] || { err "missing declaration source: $f"; exit 1; }
done

# compatibleSdkVersion is "6.0.0(20)" -- the API level is the parenthesised part.
# The version prefix is the SDK release name and is not comparable to an API level.
PROFILE_API="$(sed -n 's/.*"compatibleSdkVersion"[[:space:]]*:[[:space:]]*"[^"(]*(\([0-9]\{1,\}\))".*/\1/p' "$PROFILE" | head -1)"
if [[ ! "$PROFILE_API" =~ ^[0-9]+$ ]]; then
    err "could not read an API level from compatibleSdkVersion in $PROFILE"
    err "expected the form \"<version>(<api>)\", e.g. \"6.0.0(20)\""
    exit 1
fi

SDK_API="$(sed -n 's/^MIN_OHOS_API="\${MIGO_OHOS_MIN_API:-\([0-9]\{1,\}\)}".*/\1/p' "$SDK_SCRIPT" | head -1)"
if [[ ! "$SDK_API" =~ ^[0-9]+$ ]]; then
    err "could not read the MIN_OHOS_API default from $SDK_SCRIPT"
    err "expected a line of the form: MIN_OHOS_API=\"\${MIGO_OHOS_MIN_API:-<api>}\""
    exit 1
fi

info "app package compatibleSdkVersion: API $PROFILE_API"
info "C SDK declared MIN_OHOS_API:      API $SDK_API"

if [[ "$PROFILE_API" -ne "$SDK_API" ]]; then
    err "the two declared OpenHarmony floors disagree: app package says API $PROFILE_API, C SDK says API $SDK_API"
    err "these are one decision written twice; raise or lower both together"
    exit 1
fi

# The audited sysroot is only present on a machine with the SDK unpacked. Its
# absence reduces the check rather than failing it -- but it is reported, because
# a reduced check that prints the same thing as a full one is how the reduced
# form becomes permanent.
FLOOR_SYSROOT="${MIGO_OHOS_FLOOR_SYSROOT:-$HOME/ohos-sdk/native/sysroot}"
FLOOR_PKG="${FLOOR_SYSROOT%/sysroot}/oh-uni-package.json"
if [[ -f "$FLOOR_PKG" ]]; then
    AUDIT_API="$(sed -n 's/.*"apiVersion"[[:space:]]*:[[:space:]]*"\{0,1\}\([0-9]\{1,\}\)"\{0,1\}.*/\1/p' "$FLOOR_PKG" | head -1)"
    if [[ ! "$AUDIT_API" =~ ^[0-9]+$ ]]; then
        err "floor SDK at $FLOOR_PKG declares no readable apiVersion"
        exit 1
    fi
    info "symbol audit resolves against:    API $AUDIT_API ($FLOOR_PKG)"
    if [[ "$PROFILE_API" -lt "$AUDIT_API" ]]; then
        err "declared floor API $PROFILE_API is BELOW the audited sysroot API $AUDIT_API"
        err "the audit then proves nothing about the range [$PROFILE_API, $AUDIT_API): it"
        err "resolved every import against a sysroot newer than what is advertised"
        exit 1
    fi
    ok "declared floor API $PROFILE_API is at or above the audited API $AUDIT_API, and both declarations agree"
    exit 0
fi

info "no floor SDK unpacked at $FLOOR_SYSROOT, so the audit-floor comparison did not run"
ok "both declared floors agree at API $PROFILE_API (audit-floor comparison skipped, reported above)"
