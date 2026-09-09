#!/usr/bin/env bash
# One command for a capability-gate run: build, install, launch, collect, admit.
#
# WHY THIS EXISTS. Gate 1's result is a file, and until now getting that file off
# a device was a sequence somebody re-derived each time -- xcodebuild with the
# right destination, an install, a launch, a poll, a copy out of the app
# container, and admit.py pointed at the right directory. A lab day spent on
# borrowed devices is the worst place to be re-deriving it, and a run whose steps
# live only in somebody's shell history is a run nobody else can reproduce.
#
# THE TWO REFUSALS ARE THE POINT. Both encode a way a run produces a folder of
# valid JSON that answers a different question:
#
#   1. A device run demands both attestations. Lockdown Mode (A24) and the
#      local-network alert are device state no API reports; unattested they are
#      recorded as `not_probed`, the loopback origin requires the alert answer,
#      and admit.py then reports every loopback candidate as unmeasured -- which
#      on the page reads as though loopback had been ruled out. That happened on
#      the first unattended run of this gate.
#
#   2. Simulator output never reaches the evidence directory, and a simulator run
#      never calls admit.py. The simulator runs the host's JavaScriptCore on the
#      host's CPU, so it answers "is JIT on" with the Mac's answer.
#      `contracts/apple/capability-probe.schema.json` says simulator records are
#      not evidence, admit.py drops them, and the way that rule fails in practice
#      is not that somebody argues with it -- it is that the files end up in the
#      directory the evidence is read from and the exclusion note scrolls past.
#
# The plan is resolved and validated before any Apple tool is looked for, so
# `--dry-run` states what a run would do on any machine. That is what
# scripts/test-apple-probe-run-contract.sh checks: the refusals are executed
# rather than read.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_ID="dev.migo.probe"
PROJECT="$ROOT/platforms/apple/ProbeApp/MigoProbe.xcodeproj"
SCHEME="MigoProbe"

# Where a run's records live. The evidence directory is the one admit.py reads;
# the harness directory is for runs that prove the machinery and never the
# platform. Both are under docs/, which is gitignored: these are lab outputs.
EVIDENCE_DIR="$ROOT/docs/performance/apple/g0/capability"
HARNESS_DIR="$ROOT/docs/performance/apple/g0/harness-runs"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
usage: run-apple-probe.sh (--device <id> | --simulator [<id>]) [options]

  --device <id>                 A signed, developer-enabled iPhone. Produces
                                evidence, and therefore requires both
                                attestations below.
  --simulator [<id>]            A simulator, by udid or name. Proves the
                                harness; never evidence.

  --lockdown off|on             A24. Whether Lockdown Mode is in force. No
                                public API reports it, so a person declares it.
  --local-network-prompt none|shown
                                Whether the system presented a local-network
                                alert during the run. Nothing in the process can
                                see it: traffic flows both when nobody was asked
                                and when the operator tapped Allow.

  --out <dir>                   Where the records land. Defaults to the evidence
                                directory for a device and the harness directory
                                for a simulator.
  --run-id <id>                 Names the run, and therefore the records file.
  --timeout <seconds>           How long to wait for the records (default 420;
                                the gate allows 120 per origin and there are two).
  --keep-derived                Leave the derived-data directory in place.
  --dry-run                     Print the resolved plan and stop. Validates
                                first, so the refusals apply.

An attestation is a person's answer carried by automation, not a default. A flag
left off records `unknown`, which is a different answer from `off`, and a value
this script does not understand is refused rather than read as `unknown`.
USAGE
}

MODE=""
TARGET=""
LOCKDOWN=""
PROMPT=""
OUT_DIR=""
RUN_ID=""
TIMEOUT=420
DRY_RUN=0
KEEP_DERIVED=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --device)
      [[ -n "$MODE" ]] && fail "--device and --simulator name two different runs; pass one"
      [[ $# -ge 2 ]] || fail "--device needs an identifier (xcrun devicectl list devices)"
      MODE="device"
      TARGET="$2"
      shift 2
      ;;
    --simulator)
      [[ -n "$MODE" ]] && fail "--device and --simulator name two different runs; pass one"
      MODE="simulator"
      if [[ $# -ge 2 && "$2" != --* ]]; then
        TARGET="$2"
        shift 2
      else
        TARGET="booted"
        shift
      fi
      ;;
    --lockdown)
      [[ $# -ge 2 ]] || fail "--lockdown needs off or on"
      LOCKDOWN="$2"
      shift 2
      ;;
    --local-network-prompt)
      [[ $# -ge 2 ]] || fail "--local-network-prompt needs none or shown"
      PROMPT="$2"
      shift 2
      ;;
    --out)
      [[ $# -ge 2 ]] || fail "--out needs a directory"
      OUT_DIR="$2"
      shift 2
      ;;
    --run-id)
      [[ $# -ge 2 ]] || fail "--run-id needs a value"
      RUN_ID="$2"
      shift 2
      ;;
    --timeout)
      [[ $# -ge 2 ]] || fail "--timeout needs seconds"
      TIMEOUT="$2"
      shift 2
      ;;
    --keep-derived)
      KEEP_DERIVED=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      fail "$1 is not an option this script takes. --help lists them"
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Validation. Everything here runs before any Apple tool is looked for, so the
# refusals are the same on any machine and can be executed by a contract gate.
# ---------------------------------------------------------------------------

[[ -n "$MODE" ]] || fail "pass --device <id> for evidence or --simulator to prove the harness"

case "$LOCKDOWN" in
  "" | off | on) ;;
  *) fail "--lockdown $LOCKDOWN is neither off nor on. It is refused rather than recorded as unknown: unknown is what a run nobody attested says, and somebody was at this bench" ;;
esac
case "$PROMPT" in
  "" | none | shown) ;;
  *) fail "--local-network-prompt $PROMPT is neither none nor shown. Refused for the same reason --lockdown is" ;;
esac

if [[ "$MODE" == "device" ]]; then
  # A string and not an array: macOS ships bash 3.2, where an empty array read
  # under `set -u` is an unbound variable rather than an empty list.
  MISSING=""
  [[ -n "$LOCKDOWN" ]] || MISSING="--lockdown off|on"
  if [[ -z "$PROMPT" ]]; then
    [[ -z "$MISSING" ]] || MISSING="$MISSING and "
    MISSING="$MISSING--local-network-prompt none|shown"
  fi
  if [[ -n "$MISSING" ]]; then
    fail "a device run needs $MISSING. Without them the record says not_probed for the two answers no API reports, admit.py reports every arm that required one as unmeasured, and the report reads as though the device had ruled those arms out"
  fi
fi

if [[ -z "$RUN_ID" ]]; then
  RUN_ID="probe-$(date -u +%Y%m%dT%H%M%SZ)-$MODE"
fi
# The same rule MigoProbeLaunchOptions applies, for the same reason: the run id
# becomes the records filename. A script that names a run the app refuses is a
# run that dies at launch with the devices already in hand.
[[ "$RUN_ID" =~ ^[A-Za-z0-9._-]{1,64}$ ]] \
  || fail "--run-id $RUN_ID cannot name a file: up to 64 of A-Z a-z 0-9 . _ -"

abspath() {
  python3 -c 'import os,sys; print(os.path.abspath(os.path.expanduser(sys.argv[1])))' "$1"
}

if [[ -z "$OUT_DIR" ]]; then
  if [[ "$MODE" == "device" ]]; then OUT_DIR="$EVIDENCE_DIR"; else OUT_DIR="$HARNESS_DIR"; fi
fi
OUT_DIR="$(abspath "$OUT_DIR")"
EVIDENCE_ABS="$(abspath "$EVIDENCE_DIR")"

if [[ "$MODE" == "simulator" && ( "$OUT_DIR" == "$EVIDENCE_ABS" || "$OUT_DIR" == "$EVIDENCE_ABS"/* ) ]]; then
  fail "$OUT_DIR is the evidence directory, and a simulator answers 'is JIT on' with this Mac's answer. The contract says simulator records are not evidence; the way that rule actually fails is a file in this directory and an exclusion note nobody read"
fi

RECORD_FILE="$OUT_DIR/capability-$RUN_ID.json"
APP_ARGS=(--migo-autorun "--migo-run-id=$RUN_ID")
[[ -n "$LOCKDOWN" ]] && APP_ARGS+=("--migo-lockdown=$LOCKDOWN")
[[ -n "$PROMPT" ]] && APP_ARGS+=("--migo-local-network-prompt=$PROMPT")

# Admission is for evidence. A simulator run resolves no admit input at all,
# rather than resolving one and hoping the exclusion rule catches it downstream.
ADMIT_INPUT=""
[[ "$MODE" == "device" ]] && ADMIT_INPUT="$OUT_DIR"

DERIVED="$OUT_DIR/derived-$RUN_ID"

if ((DRY_RUN)); then
  cat <<PLAN
mode=$MODE
target=$TARGET
bundle_id=$BUNDLE_ID
run_id=$RUN_ID
records_dir=$OUT_DIR
record_file=$RECORD_FILE
app_arguments=${APP_ARGS[*]}
admit_input=$ADMIT_INPUT
timeout=$TIMEOUT
PLAN
  exit 0
fi

# ---------------------------------------------------------------------------
# From here on it needs the Apple toolchain.
# ---------------------------------------------------------------------------

[[ "$(uname -s)" == "Darwin" ]] || fail "a run needs macOS and Xcode; --dry-run works anywhere"
command -v xcrun >/dev/null || fail "xcrun is not on PATH"
command -v xcodebuild >/dev/null || fail "xcodebuild is not on PATH"

mkdir -p "$OUT_DIR"

# ---------------------------------------------------------------------------
# A device has two names and the tools disagree about which one they take.
# `devicectl` speaks its own CoreDevice identifier (a UUID); `xcodebuild`
# destinations speak the hardware UDID. They are different strings for the same
# phone, and `--device` accepts either rather than making the operator know
# which tool is downstream of the flag.
# ---------------------------------------------------------------------------
DEVICE_UDID=""
if [[ "$MODE" == "device" ]]; then
  DEVICE_LIST="$OUT_DIR/devices-$RUN_ID.json"
  xcrun devicectl list devices --json-output "$DEVICE_LIST" >/dev/null 2>&1 \
    || fail "xcrun devicectl list devices failed; is a device paired and unlocked?"
  RESOLVED="$(python3 - "$DEVICE_LIST" "$TARGET" <<'RESOLVE'
import json, sys

wanted = sys.argv[2]
devices = json.load(open(sys.argv[1]))["result"]["devices"]
for d in devices:
    ident = d.get("identifier", "")
    hardware = d.get("hardwareProperties", {})
    udid = hardware.get("udid", "")
    name = d.get("deviceProperties", {}).get("name", "")
    if wanted in (ident, udid, name):
        print("%s\t%s" % (ident, udid))
        break
else:
    sys.stderr.write(
        "not found: %s. Paired devices: %s\n"
        % (
            wanted,
            ", ".join(
                "%s (%s, udid %s)"
                % (
                    d.get("deviceProperties", {}).get("name", "?"),
                    d.get("identifier", "?"),
                    d.get("hardwareProperties", {}).get("udid", "?"),
                )
                for d in devices
            )
            or "none",
        )
    )
    sys.exit(1)
RESOLVE
  )" || fail "--device $TARGET names no paired device"
  TARGET="${RESOLVED%%$'\t'*}"
  DEVICE_UDID="${RESOLVED##*$'\t'}"
  [[ -n "$DEVICE_UDID" ]] || fail "the device resolved to no hardware udid, which xcodebuild needs to name a destination"
fi

cleanup() {
  if ((KEEP_DERIVED == 0)) && [[ -d "$DERIVED" ]]; then
    rm -rf "$DERIVED"
  fi
  # Deliberately not a conditional: a trap whose last statement is false makes a
  # successful script exit 1, which this project has already debugged once.
  return 0
}
trap cleanup EXIT

echo "[1/5] building $SCHEME for $MODE"
BUILD_ARGS=(-project "$PROJECT" -scheme "$SCHEME" -derivedDataPath "$DERIVED")
SIGNING_HINT=""
if [[ "$MODE" == "simulator" ]]; then
  BUILD_ARGS+=(-sdk iphonesimulator -destination "generic/platform=iOS Simulator")
else
  # -allowProvisioningUpdates is not a convenience. A free personal team has no
  # profile for dev.migo.probe until one is asked for, and the device has to be
  # registered against the team the same way; without the flag xcodebuild
  # refuses with "Automatic signing is disabled and unable to generate a
  # profile", which is the first thing a lab day with a phone in hand hits and
  # reads as a signing-identity problem rather than a missing flag.
  #
  # The destination names the phone rather than `generic/platform=iOS` for the
  # other half of the same problem: a generic destination gives Xcode no device
  # to add to the team, and the portal then answers "Your team has no devices
  # from which to generate a provisioning profile" with the device sitting on
  # the desk, connected.
  BUILD_ARGS+=(-destination "id=$DEVICE_UDID" -allowProvisioningUpdates)
  SIGNING_HINT=". A device build needs a signing identity: set MIGO_PROBE_TEAM to your team id (Xcode > Settings > Accounts creates a free personal team). The identity has to be in this user's keychain -- signing runs as whoever runs this script, so an Xcode signed in as another user does not lend it one"
fi
if [[ -n "${MIGO_PROBE_TEAM:-}" ]]; then
  BUILD_ARGS+=("DEVELOPMENT_TEAM=$MIGO_PROBE_TEAM")
fi
xcodebuild "${BUILD_ARGS[@]}" build >"$OUT_DIR/build-$RUN_ID.log" 2>&1 \
  || fail "the build failed; see $OUT_DIR/build-$RUN_ID.log$SIGNING_HINT"

APP="$(find "$DERIVED/Build/Products" -maxdepth 2 -name "$SCHEME.app" -print -quit)"
[[ -n "$APP" ]] || fail "the build produced no $SCHEME.app under $DERIVED/Build/Products"

if [[ "$MODE" == "simulator" ]]; then
  echo "[2/5] installing on simulator $TARGET"
  xcrun simctl bootstatus "$TARGET" -b >/dev/null 2>&1 || true
  xcrun simctl install "$TARGET" "$APP" || fail "simctl install failed"

  echo "[3/5] launching with ${APP_ARGS[*]}"
  xcrun simctl terminate "$TARGET" "$BUNDLE_ID" >/dev/null 2>&1 || true
  xcrun simctl launch "$TARGET" "$BUNDLE_ID" "${APP_ARGS[@]}" >/dev/null \
    || fail "simctl launch failed"

  CONTAINER="$(xcrun simctl get_app_container "$TARGET" "$BUNDLE_ID" data)" \
    || fail "simctl get_app_container failed"
  SOURCE="$CONTAINER/Documents/capability-$RUN_ID.json"

  echo "[4/5] waiting up to ${TIMEOUT}s for $SOURCE"
  DEADLINE=$((SECONDS + TIMEOUT))
  while [[ ! -f "$SOURCE" ]]; do
    ((SECONDS < DEADLINE)) || fail "no records after ${TIMEOUT}s. The app writes them when the run finishes; xcrun simctl io $TARGET screenshot shows what it is stuck on, and the app reports its own state on screen"
    sleep 2
  done
  cp "$SOURCE" "$RECORD_FILE"
else
  echo "[2/5] installing on device $TARGET"
  xcrun devicectl device install app --device "$TARGET" "$APP" \
    --json-output "$OUT_DIR/install-$RUN_ID.json" \
    || fail "devicectl install failed; see $OUT_DIR/install-$RUN_ID.json"

  echo "[3/5] launching with ${APP_ARGS[*]}"
  # Not --console: it waits for the app to exit and the probe app does not.
  if ! xcrun devicectl device process launch --device "$TARGET" --terminate-existing \
    --json-output "$OUT_DIR/launch-$RUN_ID.json" \
    "$BUNDLE_ID" "${APP_ARGS[@]}"; then
    # The install succeeding and the launch being refused is one specific thing
    # on a free team, and the message iOS returns for it names three causes at
    # once ("invalid code signature, inadequate entitlements or its profile has
    # not been explicitly trusted"). On a build that just signed and installed,
    # it is always the third, and the fix is on the phone rather than on the Mac
    # -- which is worth saying, because everything else in this script is fixed
    # on the Mac.
    if grep -q "explicitly trusted" "$OUT_DIR/launch-$RUN_ID.json" 2>/dev/null; then
      fail "the device refused to launch $BUNDLE_ID because this developer certificate is not trusted on it yet. On the phone: Settings > General > VPN & Device Management > Developer App > trust the certificate, then run this again. It is once per certificate, not once per build"
    fi
    fail "devicectl launch failed; see $OUT_DIR/launch-$RUN_ID.json"
  fi

  echo "[4/5] waiting up to ${TIMEOUT}s for Documents/capability-$RUN_ID.json"
  # Copied into a directory, then located inside it. `devicectl device copy from`
  # documents --destination only as "the location to which the item should be
  # copied", which leaves open whether a non-existent path is created as the file or
  # treated as a directory to place it in. Both are handled rather than guessed,
  # because the guess would be found wrong on a bench with the device in hand and a
  # gate half measured. This path is the one thing in this script no test exercises:
  # it needs a device.
  COPY_DIR="$OUT_DIR/pull-$RUN_ID"
  rm -rf "$COPY_DIR"
  mkdir -p "$COPY_DIR"
  DEADLINE=$((SECONDS + TIMEOUT))
  until xcrun devicectl device copy from --device "$TARGET" \
    --domain-type appDataContainer --domain-identifier "$BUNDLE_ID" \
    --source "Documents/capability-$RUN_ID.json" --destination "$COPY_DIR" \
    --json-output "$OUT_DIR/copy-$RUN_ID.json" >/dev/null 2>&1; do
    ((SECONDS < DEADLINE)) || fail "no records after ${TIMEOUT}s. The app writes them when the run finishes and the screen says what it is doing; the last copy attempt is in $OUT_DIR/copy-$RUN_ID.json"
    sleep 5
  done

  PULLED="$(find "$COPY_DIR" -type f -name "capability-$RUN_ID.json" -print -quit)"
  if [[ -z "$PULLED" ]]; then
    # The other reading: the destination itself became the file.
    PULLED="$(find "$COPY_DIR" -type f -print -quit)"
  fi
  [[ -n "$PULLED" ]] || fail "devicectl reported success and left nothing under $COPY_DIR; see $OUT_DIR/copy-$RUN_ID.json"
  mv "$PULLED" "$RECORD_FILE"
  rmdir "$COPY_DIR" 2>/dev/null || true
fi

[[ -s "$RECORD_FILE" ]] || fail "$RECORD_FILE is empty"
echo "[5/5] $RECORD_FILE"

python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); r=d if isinstance(d,list) else [d]; print("records: %d" % len(r)); [print("  %s %s %s %s" % (x.get("origin"), x.get("device_class"), x.get("os_version"), x.get("lockdown_mode"))) for x in r]' \
  "$RECORD_FILE"

if [[ -n "$ADMIT_INPUT" ]]; then
  echo "--- admission ---"
  # Without --require-admission on purpose. One phone on one OS is a
  # `provisional` verdict by the contract's own coverage rule (15.0, 15.1, 15.2,
  # current), and a non-zero exit there would read as a broken run rather than as
  # a lab day that is not finished.
  python3 "$ROOT/tools/apple-probe-decision/admit.py" --input "$ADMIT_INPUT" \
    --output "$ADMIT_INPUT/admission.json"
  python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("verdict:", d["verdict"]); [print("  refusal:", x) for x in d.get("refusals", [])]; [print("  reason:", x) for x in d.get("reasons", [])]' \
    "$ADMIT_INPUT/admission.json"
else
  echo "--- no admission: a simulator proves the harness and never the platform ---"
fi
