#!/usr/bin/env bash
# The frame-channel threshold agrees with the measurement that produced it.
#
# `MigoFrameChannelPolicy` carries a number -- the size at which Performance+
# stops using the loopback socket and starts using the custom-scheme request --
# and that number came from a measurement, not from a preference. Three things
# have to keep agreeing, and nothing else checks that they do:
#
#   * the channels the policy can name, and the transports the contract declares.
#     A channel in one and not the other is a transport that either cannot be
#     recorded or cannot be chosen.
#   * the threshold, and a payload class the contract requires. The crossover was
#     located by measuring classes; a threshold between two of them would be a
#     number no device was asked about.
#   * the threshold, and the class the contract calls a frame's worth of draw
#     commands. That correspondence is why the rule reads as "commands over the
#     socket, textures over the scheme" rather than as a tuned constant, and it
#     is the part a later reader would otherwise have to take on faith.
#
# This is a gate rather than a Swift test because it compares a source file with
# a contract file, and a Swift test would have to guess where the repository root
# is -- which is wrong in the staged package copies the SDK packager makes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="$ROOT/platforms/apple/core/Sources/MigoAppleCore/MigoFrameChannelPolicy.swift"
CONTRACT="$ROOT/contracts/apple/transport-probe.schema.json"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[[ -f "$POLICY" ]] || fail "$POLICY is missing"
[[ -f "$CONTRACT" ]] || fail "$CONTRACT is missing"

python3 - "$POLICY" "$CONTRACT" <<'PYTHON'
import json
import re
import sys

policy = open(sys.argv[1], encoding="utf-8").read()
contract = json.load(open(sys.argv[2], encoding="utf-8"))

declared = set(contract["record"]["enums"]["transport"])
classes = contract["probe_rules"]["payload_classes_required"]

# The enum's wire names, read from the source rather than restated here: a list
# in this file would be a third place to keep in agreement.
named = set(re.findall(r'case\s+\w+\s*=\s*"([a-z_]+)"', policy))
if not named:
    print("the policy declares no channels this gate can read; if the enum was "
          "rewritten, this check stopped checking anything", file=sys.stderr)
    raise SystemExit(1)

unknown = sorted(named - declared)
if unknown:
    print(f"the policy can choose {', '.join(unknown)}, which the transport "
          f"contract does not declare -- a channel that cannot be recorded",
          file=sys.stderr)
    raise SystemExit(1)

match = re.search(r"socketCeilingBytes\s*=\s*([0-9]+)\s*\*\s*([0-9]+)", policy)
if match:
    ceiling = int(match.group(1)) * int(match.group(2))
else:
    match = re.search(r"socketCeilingBytes\s*=\s*([0-9_]+)", policy)
    if not match:
        print("the policy's socket ceiling is written in a form this gate cannot "
              "read, so nothing checks it against the measured classes",
              file=sys.stderr)
        raise SystemExit(1)
    ceiling = int(match.group(1).replace("_", ""))

if ceiling not in classes:
    print(f"the socket ceiling is {ceiling} bytes and the contract measured "
          f"{classes}. A threshold between two measured classes is a number no "
          f"device was asked about.", file=sys.stderr)
    raise SystemExit(1)

# The frame-command class, named by the contract's own comment rather than by a
# constant here. It is the reason the rule is a sentence and not a tuning.
comment = " ".join(contract["probe_rules"]["_payload_comment"])
frame_class = re.search(r"draw commands \((\d+)\s*KiB\)", comment)
if not frame_class:
    print("the contract no longer says which class is a frame's worth of draw "
          "commands, so the correspondence that makes the threshold meaningful "
          "cannot be checked", file=sys.stderr)
    raise SystemExit(1)
expected = int(frame_class.group(1)) * 1024
if ceiling != expected:
    print(f"the socket ceiling is {ceiling} and the contract calls {expected} "
          f"a frame's worth of draw commands. They were the same number on "
          f"purpose: it is what makes the rule 'commands over the socket, "
          f"textures over the scheme'.", file=sys.stderr)
    raise SystemExit(1)

print(f"channels {sorted(named)} are declared; ceiling {ceiling} is a measured "
      f"class and is the frame-command class")
PYTHON

echo "PASS: the frame-channel threshold is a measured class and the channels are declared"
