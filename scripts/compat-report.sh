#!/usr/bin/env bash
# A compatibility report for one bundle, from one run on the Linux headless player.
#
# Runs the bundle with the compatibility recorder behind any adapters it needs, then judges what it
# reached for by this build's contracts for one platform -- Android unless told otherwise, because that is
# where hosts run it (scripts/lib/compat_report.py says how).
#
# A PARTIAL VIEW, AND IT SAYS SO. The Linux player has no device services: login, storage and system
# information answer "not supported" here, and a game takes its failure branches from there on, so the
# calls it makes are not all the calls it makes on a phone. scripts/prescreen-run.sh's header has the
# longer argument. The device run is the authority; this is what can be answered without one, and the
# report carries that note.
#
#   bash scripts/compat-report.sh BUNDLE [--adapter FILE]... [--secs N] [--platform P]
#                                        [--out FILE] [--json FILE] [--log FILE]
#
#   --adapter   a classic-script adapter bundle (e.g. migo-wx-adapter's IIFE), injected before the
#               recorder in the order given, as a host injects it
#   --log       keep the player's log there
#
# Exit status is the run's: a report is written either way, and a run that did not present is not a pass.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE=""; SECS=10; PLATFORM=android; OUT=""; JSON=""; LOG=""
ADAPTERS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --adapter) ADAPTERS+=("${2:?--adapter requires a path}"); shift 2 ;;
        --secs) SECS="${2:?--secs requires a value}"; shift 2 ;;
        --platform) PLATFORM="${2:?--platform requires a value}"; shift 2 ;;
        --out) OUT="${2:?--out requires a path}"; shift 2 ;;
        --json) JSON="${2:?--json requires a path}"; shift 2 ;;
        --log) LOG="${2:?--log requires a path}"; shift 2 ;;
        -*) echo "unknown option: $1" >&2; exit 2 ;;
        *) [[ -z "$BUNDLE" ]] || { echo "one bundle at a time" >&2; exit 2; }; BUNDLE="$1"; shift ;;
    esac
done
# The player's own rule for a bundle, so a directory it would refuse is refused here, before a run that
# records nothing.
[[ -n "$BUNDLE" && -f "$BUNDLE/game.json" && -f "$BUNDLE/game.js" ]] \
    || { echo "usage: compat-report.sh BUNDLE (a directory with game.json and game.js) ..." >&2; exit 2; }
[[ -f "$ROOT/contracts/runtime/api-conformance-$PLATFORM.json" ]] \
    || { echo "no conformance record for platform '$PLATFORM'" >&2; exit 2; }

bash "$ROOT/scripts/dev-test-host.sh" build -p migo-player --offline >&2
export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

LOG="${LOG:-$(mktemp /tmp/migo-compat-report.XXXXXX.log)}"
preludes=()
for adapter in "${ADAPTERS[@]}"; do
    preludes+=(--prelude "$adapter")
done
preludes+=(--prelude "$ROOT/tools/compat-recorder/recorder.js")

status=0
# No window server, as in scripts/verify-bypass-present.sh: an inherited DISPLAY makes the offscreen
# display resolve to X11.
env -u DISPLAY -u WAYLAND_DISPLAY "$ROOT/engine/target/debug/migo-player" "$BUNDLE" "$SECS" "${preludes[@]}" \
    >"$LOG" 2>&1 || status=$?

args=("$LOG" --platform "$PLATFORM"
      --note "One run of ${SECS} s on the Linux headless player, which has no device services: a call that needs one fails here, and the game's path after it is not its path on a phone. The device run (scripts/prescreen-run.sh) is the authority."
      --note "Statuses are for $PLATFORM.")
[[ $status -eq 0 ]] || args+=(--note "The player exited with status $status: the run did not end with a presented frame. The log is $LOG.")
[[ -n "$OUT" ]] && args+=(--out "$OUT")
[[ -n "$JSON" ]] && args+=(--json "$JSON")
python3 "$ROOT/scripts/lib/compat_report.py" "${args[@]}"
exit "$status"
