#!/usr/bin/env bash
# The API x platform matrix is what its sources say, and nothing else.
#
# contracts/runtime/api-platform-matrix.json answers, for every API of the
# common mini-game platform and every platform Migo ships, whether it works and
# how: the engine alone, the platform's host layer by default, a host interface
# with no default, missing, or deliberately unsupported. It is the status every
# piece of platform work is planned from, so a stale cell is a plan built on a
# false fact -- which is how "C ABI hosts get every service" went unnoticed
# while the C ABI carried seven of thirty.
#
# So it is generated (scripts/gen-api-platform-matrix.py) from measured and
# read facts -- what each API reaches when called (minigame-api-reach.json,
# kept true by the runtime's api_reach test), the C ABI's host-service channel
# and typed callbacks, what each platform's host layer installs and feeds --
# plus the few decisions that cannot be measured, each with its reason
# (api-platform-decisions.json). This gate regenerates it and fails on any
# difference, then proves it would fail on each drift it exists to catch.
#
# Host-only: reads sources, runs no engine.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/scripts/gen-api-platform-matrix.py"
fail() { echo "FAIL: $*" >&2; exit 1; }

python3 "$GEN" --root "$ROOT" --check \
    || fail "the API x platform matrix does not match its sources"

# ---- each drift turns the gate red ---------------------------------------------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Every file the generator reads, copied where it expects it.
inputs=(
    contracts/runtime/minigame-api.json
    contracts/runtime/minigame-api-spec.json
    contracts/runtime/minigame-api-reach.json
    contracts/runtime/api-platform-decisions.json
    contracts/runtime/api-platform-matrix.json
    contracts/runtime/host-services.json
    contracts/runtime/op-boundary.json
    engine/crates/capi/src/host_kit.rs
    engine/crates/capi/src/device.rs
    engine/crates/capi/src/callbacks.rs
    engine/crates/platform/src/android/services/mod.rs
)
# What each platform's run of the API spec bundle measured, where one was imported.
for record in "$ROOT"/contracts/runtime/api-conformance-*.json; do
    [[ -e "$record" ]] && inputs+=("contracts/runtime/$(basename "$record")")
done
while IFS= read -r path; do inputs+=("$path"); done < <(
    cd "$ROOT" && git ls-files include/migo platforms/apple/Sources/MigoAppleRenderer \
        platforms/apple/Sources/MigoMacV8 platforms/apple/Sources/MigoApplePerformancePlus \
        platforms/apple/Sources/MigoAppleLocation platforms/apple/Sources/MigoAppleBluetooth \
        platforms/linux/host-kit/src platforms/openharmony/entry/src/main \
        platforms/android/library/src/main/java
)

fresh() {
    rm -rf "$TMP/root"
    mkdir -p "$TMP/root"
    (cd "$ROOT" && tar cf - "${inputs[@]}") | (cd "$TMP/root" && tar xf -)
}

# Applies a Python edit to one copied file, then expects the generator to refuse
# or the committed matrix to be stale.
red() {
    local what="$1" file="$2" edit="$3"
    fresh
    python3 - "$TMP/root/$file" "$edit" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
changed = eval(sys.argv[2], {"text": text, "json": __import__("json")})
if changed == text:
    sys.exit(f"the injection for {path} changed nothing")
path.write_text(changed, encoding="utf-8")
PY
    if python3 "$GEN" --root "$TMP/root" --check >"$TMP/out" 2>&1; then
        cat "$TMP/out" >&2
        fail "red proof: $what was not caught"
    fi
    echo "  red: $what -- $(tail -1 "$TMP/out")"
}

echo "api-platform-matrix: proving each drift turns it red"
fresh
python3 "$GEN" --root "$TMP/root" --check >/dev/null || fail "the copied inputs do not reproduce the matrix"

red "an API the probe has no record of" contracts/runtime/minigame-api-reach.json \
    'text.replace("    \"vibrateShort\": ", "    \"vibrateShortGone\": ", 1)'
red "an API that reaches nothing, undecided" contracts/runtime/api-platform-decisions.json \
    'text.replace("    \"createPath2D\": \"a path is the engine'"'"'s own geometry\",\n", "", 1)'
red "a decision about an API the platform does not have" contracts/runtime/api-platform-decisions.json \
    'text.replace("\"via\": {", "\"via\": {\n    \"notAnApi\": \"loadSubpackage\",", 1)'
red "an API that needs what it is via, in a cycle" contracts/runtime/api-platform-decisions.json \
    'text.replace("\"via\": {", "\"via\": {\n    \"loadSubpackage\": \"preDownloadSubpackage\",", 1)'
red "an API that starts reaching the host" contracts/runtime/minigame-api-reach.json \
    'text.replace("\"getStorageSync\": {\"ops\":[", "\"getStorageSync\": {\"ops\":[\"op_get_location\",", 1)'
red "a host layer that stops installing a callback" platforms/apple/Sources/MigoAppleRenderer/MigoEngineSession.swift \
    'text.replace("callbacks.on_keep_screen_on = ", "// ", 1)'
red "a channel method moving to another op" contracts/runtime/host-services.json \
    'text.replace("\"op\": \"op_set_clipboard_data\"", "\"op\": \"op_set_clipboard_data_moved\"", 1)'
# What is owed is what the reference offers on each kind of client: a desktop-only call that a page stops calling
# desktop-only is owed on the phones, and a phones-only one a desktop client starts offering is owed there.
red "a desktop-only call the reference starts offering on its phones" contracts/runtime/minigame-api-spec.json \
    'text.replace("\"mobile\": false", "\"mobile\": true", 1)'
red "a phones-only call the reference's Windows client starts offering" contracts/runtime/minigame-api-spec.json \
    'text.replace("\"createVideoDecoder\": {", "\"createVideoDecoder\": {\"clients\": {\"windows\": true}, ", 1)'
# Ready is verified, not wired: a measured API that regresses is no longer ready, and a record that does not measure
# the platform's API list, or says what no run can, is refused.
if [[ -e "$ROOT/contracts/runtime/api-conformance-macos.json" ]]; then
    red "a verified API that a later run finds failing" contracts/runtime/api-conformance-macos.json \
        'text.replace("\"getWindowInfo\": {\"status\": \"verified\"}", "\"getWindowInfo\": {\"status\": \"failing\", \"failing\": [\"api-spec/getWindowInfo/returns\"]}", 1)'
    red "a run record that does not measure an API" contracts/runtime/api-conformance-macos.json \
        'text.replace("    \"getWindowInfo\": ", "    \"getWindowInfoGone\": ", 1)'
    red "a run record that claims a status no run produces" contracts/runtime/api-conformance-macos.json \
        'text.replace("\"getWindowInfo\": {\"status\": \"verified\"}", "\"getWindowInfo\": {\"status\": \"probably\"}", 1)'
fi
red "the Swift SDK that stops answering a service itself" platforms/apple/Sources/MigoAppleRenderer/MigoBuiltInHostServices.swift \
    'text.replace("builtIn.insert(.clipboard)", "// builtIn.insert(.clipboard)", 1)'
red "the macOS game view that stops answering the window service" platforms/apple/Sources/MigoMacV8/MigoWindowService.swift \
    'text.replace("builtIn.insert(.window)", "// builtIn.insert(.window)", 1)'
red "the Swift SDK that stops reporting the system's switches" platforms/apple/Sources/MigoAppleRenderer/MigoEngineSession.swift \
    'text.replace("migo_session_set_system_settings(session", "migo_session_set_system_settings_gone(session", 1)'
red "the Swift SDK that stops reporting the network" platforms/apple/Sources/MigoAppleRenderer/MigoEngineSession.swift \
    'text.replace("migo_session_report_network(session", "migo_session_report_network_gone(session", 1)'
red "a capability product that stops answering its service" platforms/apple/Sources/MigoAppleLocation/MigoLocation.swift \
    'text.replace("builtIn.insert(.location)", "// builtIn.insert(.location)", 1)'
red "an engine decoder compiled for another system" engine/crates/capi/src/host_kit.rs \
    'text.replace("#[cfg(target_os = \"macos\")]\n    fn video_decoder", "#[cfg(target_os = \"windows\")]\n    fn video_decoder", 1)'
# OpenHarmony's targets are `target_os = "linux"` too: an accessor that stops
# telling them apart claims a decoder for a platform that has none.
red "an engine decoder that a bare Linux cfg also builds for OpenHarmony" engine/crates/capi/src/host_kit.rs \
    'text.replace("#[cfg(all(target_os = \"linux\", not(target_env = \"ohos\")))]\n    fn video_decoder", "#[cfg(target_os = \"linux\")]\n    fn video_decoder", 1)'
red "an accessor cfg that does not say which platforms" engine/crates/capi/src/host_kit.rs \
    'text.replace("#[cfg(all(target_os = \"linux\", not(target_env = \"ohos\")))]\n    fn video_decoder", "#[cfg(all(target_os = \"linux\", feature = \"gst\"))]\n    fn video_decoder", 1)'
red "the engine's audio settings no longer built for Windows" engine/crates/capi/src/host_kit.rs \
    'text.replace("target_os = \"windows\"\n    ))]\n    fn audio_platform", "target_os = \"linux\"\n    ))]\n    fn audio_platform", 1)'
red "the Mac's game view that stops feeding the mouse" platforms/apple/Sources/MigoMacV8/MigoGameView.swift \
    'text.replace("engine.sendPointer(", "engine.sendPointerGone(")'
red "the iPad's game view that stops feeding keys the shared session can still send" platforms/apple/Sources/MigoApplePerformancePlus/MigoGameView.swift \
    'text.replace("engine.sendKey(", "engine.sendKeyGone(")'
red "the iPad's game view that stops answering the window service" platforms/apple/Sources/MigoApplePerformancePlus/MigoWindowService.swift \
    'text.replace("builtIn.insert(.window)", "// builtIn.insert(.window)", 1)'
red "the iPad's game view that stops reporting the screen's turn" platforms/apple/Sources/MigoApplePerformancePlus/MigoWindowService.swift \
    'text.replace("MigoHostServiceID.Screen.Event.deviceOrientationChange", "MigoHostServiceID.Screen.Event.recordingStateChange")'
red "a listener decided by an event no service has" contracts/runtime/api-platform-decisions.json \
    'text.replace("\"screen.device_orientation_change\"", "\"screen.turned\"", 1)'
red "the Linux Host Kit that stops answering a service itself" platforms/linux/host-kit/src/qt6/host_services.cpp \
    'text.replace("builtIn |= hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);", "// clipboard", 1)'
red "a host layer that stops feeding touch" platforms/linux/host-kit/src/qt6/x11_surface_view.cpp \
    'text.replace("migo_session_send_touch(", "migo_session_send_touch_removed(")'
red "an unsupported decision with no reason" contracts/runtime/api-platform-decisions.json \
    'text.replace("\"reason\": \"WebGPU is not implemented on any platform; content feature-detects it\"", "\"reason\": \"\"", 1)'

summary="$(python3 - "$ROOT/contracts/runtime/api-platform-matrix.json" <<'PY'
import json, sys
matrix = json.load(open(sys.argv[1]))
total = matrix["total"]
print(", ".join("%s %d/%d ready" % (p, matrix["summary"][p]["ready"], total) for p in matrix["platforms"]))
PY
)"
echo "api-platform-matrix contract: PASS ($summary)"
