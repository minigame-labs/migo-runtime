#!/usr/bin/env bash
# Run a Rust test binary built for an iOS Simulator target, on the simulator.
#
# cargo's `target.<triple>.runner` hook. Point a cargo invocation at it and
# `cargo test --target <an ios-sim triple>` stops being a cross-compile check and
# becomes a test run:
#
#   cargo test -p migo-platform --target aarch64-apple-ios-sim \
#     --config "target.aarch64-apple-ios-sim.runner='$PWD/../scripts/ios-sim-cargo-runner.sh'"
#
# ## Why this exists
#
# No engine Rust test has ever executed on iOS. The Apple lanes compile the
# engine for the device and the simulator and then test it on the *host*, which
# is macOS -- a different OS, a different Skia build, a different ANGLE. Most of
# the time that is the same answer. The case that made it not the same answer is
# on the record: Skia's macOS build assumes desktop GL and its iOS build assumes
# ES, so `Canvas2DContext::new` failed on macOS and could not fail the same way
# on iOS, and a macOS-only test could say nothing about the platform the product
# ships on. See scripts/apple-skia-gl-env.sh.
#
# ## What it does, and the two things that are not obvious
#
# `xcrun simctl spawn` runs an ordinary simulator-target Mach-O; it does not need
# an app bundle, a signature, or an Info.plist. But the child's environment is
# not this shell's: simctl passes through only variables prefixed
# `SIMCTL_CHILD_`, and strips the prefix on the way in. So a dylib search path
# for the pinned ANGLE has to be set as `SIMCTL_CHILD_DYLD_LIBRARY_PATH`, and
# setting `DYLD_LIBRARY_PATH` directly is the mistake that looks like ANGLE not
# existing.
#
# The exit status survives, and that was checked rather than assumed, because a
# runner that swallowed it would report every suite green forever: a simulator
# binary returning 42 through this script makes the script return 42, with the
# child's stdout intact. The negative control for the whole mechanism is equally
# concrete -- the same Rust test binary run WITHOUT this runner aborts with
# `dyld: DYLD_ROOT_PATH not set for simulator program`, so a lane that quietly
# stopped using it could not pass.
#
# The device is named by MIGO_IOS_SIM_UDID when the caller has one, and is
# otherwise whichever simulator is already booted. A caller that boots nothing
# gets a clear failure rather than a spawn against "booted" that resolves to
# nothing.
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <test-binary> [args...]" >&2
  exit 2
fi

BINARY="$1"
shift

if [[ -n "${MIGO_IOS_SIM_UDID:-}" ]]; then
  DEVICE="$MIGO_IOS_SIM_UDID"
else
  DEVICE="$(xcrun simctl list devices booted -j 2>/dev/null \
    | python3 -c 'import json,sys
devices = json.load(sys.stdin)["devices"]
for runtime in devices.values():
    for device in runtime:
        if device.get("state") == "Booted":
            print(device["udid"])
            raise SystemExit(0)
' || true)"
fi

if [[ -z "$DEVICE" ]]; then
  echo "ios-sim-cargo-runner: no booted simulator and no MIGO_IOS_SIM_UDID." >&2
  echo "  Boot one first (xcrun simctl boot <udid>), or name one in MIGO_IOS_SIM_UDID." >&2
  echo "  Spawning against a device that is not booted reports the binary's absence," >&2
  echo "  which reads like a build failure and is not one." >&2
  exit 1
fi

# The ANGLE the engine dlopens. Passed under the SIMCTL_CHILD_ prefix because
# that is the only channel into the child; see the header.
#
# Both variables, and the framework one is the one that does the work here: on
# iOS the engine asks for `libEGL.framework/libEGL` (see APPLE_EGL_LIBRARY in
# crates/platform/src/apple/presenter.rs), and dyld recognises a
# `Name.framework/Name` path as a framework and resolves it through
# DYLD_FRAMEWORK_PATH. DYLD_LIBRARY_PATH does not apply to it, so setting only
# that one produces "image not found" against a directory that plainly contains
# the framework -- which reads like the pin being wrong and is not.
if [[ -n "${MIGO_IOS_SIM_ANGLE:-}" ]]; then
  export SIMCTL_CHILD_DYLD_FRAMEWORK_PATH="$MIGO_IOS_SIM_ANGLE${SIMCTL_CHILD_DYLD_FRAMEWORK_PATH:+:$SIMCTL_CHILD_DYLD_FRAMEWORK_PATH}"
  export SIMCTL_CHILD_DYLD_LIBRARY_PATH="$MIGO_IOS_SIM_ANGLE${SIMCTL_CHILD_DYLD_LIBRARY_PATH:+:$SIMCTL_CHILD_DYLD_LIBRARY_PATH}"
fi

# Rust's own test harness reads these, and they are as invisible to the child as
# anything else without the prefix.
for name in RUST_BACKTRACE RUST_LOG RUST_TEST_THREADS MIGO_CAPI_LOG; do
  if [[ -n "${!name:-}" ]]; then
    export "SIMCTL_CHILD_$name=${!name}"
  fi
done

exec xcrun simctl spawn "$DEVICE" "$BINARY" "$@"
