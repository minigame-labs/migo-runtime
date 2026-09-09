#!/usr/bin/env bash
# The probe app must not need an exemption the product cannot ship.
#
# G0.3 is a row in the probe matrix, not an afterthought: "ATS/Info.plist, no
# NSAllowsArbitraryLoads, no Bonjour, only public API to stay alive". Its
# device half needs a signed phone. Its static half is a property of a file in
# this repository and can be checked today.
#
# THE DRIFT THIS EXISTS TO CATCH is the shortest path out of a real problem.
# The loopback origin is plain HTTP, App Transport Security blocks cleartext,
# and the one-line way to make the page load is NSAllowsArbitraryLoads. It
# works, it is invisible in a green run, and it changes what the gate measured:
# a transport that only loads with ATS disabled is a transport the shipping
# product cannot use, and G0 would have selected it on numbers gathered under a
# configuration that will not pass review. NSAllowsLocalNetworking permits
# cleartext to loopback and link-local and nothing else, which is the exemption
# the product would actually ship.
#
# Bonjour is the same shape one layer down: `NSBonjourServices` plus a local
# network usage description turns the permission prompt into something the
# operator taps past, and G0.3 exists partly to find out whether a prompt
# appears at all. An app that pre-arranges the prompt cannot answer that.
#
# Host-only: python3 reads the plist. No macOS, no Xcode.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PLIST="platforms/apple/ProbeApp/Info.plist"

if [[ ! -f "$PLIST" ]]; then
    echo "FAIL: $PLIST is missing; the probe app's App Transport Security posture is unchecked." >&2
    exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "FAIL: python3 is not available, so the probe app's Info.plist is unverified." >&2
    exit 1
fi

python3 - "$PLIST" <<'PY'
import plistlib
import sys

path = sys.argv[1]
with open(path, "rb") as handle:
    plist = plistlib.load(handle)

problems: list[str] = []
notes: list[str] = []

ats = plist.get("NSAppTransportSecurity")
if not isinstance(ats, dict):
    problems.append(
        "there is no NSAppTransportSecurity dictionary. The loopback origin is plain HTTP and "
        "will not load, so the loopback arm of gate 1 would report every capability as "
        "unavailable for a reason that is in this file."
    )
else:
    # The control. A checker that read nothing would report a clean plist, and
    # this repository has produced two decisive false conclusions that way.
    if "NSAllowsLocalNetworking" not in ats:
        problems.append(
            "NSAllowsLocalNetworking is absent. It is the narrow exemption -- cleartext to "
            "loopback and link-local only -- that lets the loopback origin load without "
            "turning App Transport Security off for everything."
        )
    elif ats["NSAllowsLocalNetworking"] is not True:
        problems.append(
            f"NSAllowsLocalNetworking is {ats['NSAllowsLocalNetworking']!r}, not true"
        )
    else:
        notes.append("the reader sees NSAllowsLocalNetworking = true, so it is reading the plist")

    for forbidden, why in (
        ("NSAllowsArbitraryLoads",
         "it disables App Transport Security for every host. A transport measured under it is a "
         "transport measured in a configuration the product cannot ship, and G0 would select on "
         "those numbers."),
        ("NSAllowsArbitraryLoadsInWebContent",
         "it disables ATS for everything the web view loads, which is the whole probe."),
        ("NSAllowsArbitraryLoadsForMedia",
         "the probe loads no media; an exemption nothing needs is an exemption nobody removes."),
    ):
        if forbidden in ats:
            problems.append(f"{forbidden} is set: {why}")

if "NSBonjourServices" in plist:
    problems.append(
        "NSBonjourServices is declared. G0.3's list names Bonjour explicitly, and part of what "
        "gate 1 asks is whether a local-network prompt appears at all -- an app that "
        "pre-arranges the prompt has answered its own question."
    )
if "NSLocalNetworkUsageDescription" in plist:
    problems.append(
        "NSLocalNetworkUsageDescription is declared. It is the string shown in the prompt the "
        "no_local_network_prompt capability exists to detect."
    )

# The records are the output of a run, and a result that can only be read on the
# bench does not leave the bench.
for key, why in (
    ("UIFileSharingEnabled",
     "without it the operator needs a debugger attached to retrieve the records a run produced"),
):
    if plist.get(key) is not True:
        problems.append(f"{key} is not true: {why}")

if not problems:
    notes.append("no arbitrary-loads exemption, no Bonjour, no pre-declared local-network prompt")

print()
for note in notes:
    print(f"  - {note}")
print()

if problems:
    print("FAIL: the probe app's Info.plist would change what gate 1 measures.", file=sys.stderr)
    print(file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    print(file=sys.stderr)
    raise SystemExit(1)

print("PASS: the probe app loads its loopback origin under the exemption the product would ship.")
PY
