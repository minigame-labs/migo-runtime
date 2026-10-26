#!/usr/bin/env bash
# CPAL 0.18 unconditionally compiled macOS process taps (14.2/14.6 APIs).
# Mini-game device output must not silently inherit that unrelated OS floor.
# Keep the optional-capture patch reproducible and verify the actual macOS
# product feature graph. Apple native builds/device tests prove the OS floor;
# the source/graph check alone deliberately makes no such claim.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT" cpal-patches
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$ROOT/engine"
for profile in external-frames profile-full; do
    cargo tree --locked -p migo-capi --no-default-features --features "$profile" \
        --target aarch64-apple-darwin -e normal --prefix none --color never \
        --format '{p}|{f}' > "$WORK/$profile.txt"
done
python3 - "$WORK/external-frames.txt" "$WORK/profile-full.txt" <<'PY'
from pathlib import Path
import sys
for file in sys.argv[1:]:
    rows = [line.removesuffix(' (*)').split('|') for line in Path(file).read_text().splitlines()]
    cpal = [row for row in rows if row[0].startswith('cpal v')]
    assert cpal and all(row[0].startswith('cpal v0.18.2 ') for row in cpal), 'the product must resolve exactly CPAL0.18.2'
    assert all(not row[1] for row in cpal), 'the output product enabled optional CPAL capabilities'
    core = [row for row in rows if row[0].startswith('objc2-core-audio v')]
    assert core, 'the macOS device backend was not resolved'
    # AudioToolbox independently needs the objc2 type bridge. It is not proof
    # of system-capture calls; only the capture-specific Foundation flag is
    # excluded here. Final Mach-O references still require Apple evidence.
    assert all('objc2-foundation' not in row[1].split(',') for row in core), 'unused capture Foundation bridging was enabled'
    print(f'CPAL {Path(file).stem}:0.18.2, optional capture/Foundation bridge disabled PASS')
PY

# The upstream iOS backend already owns interruptions. A second Migo hardware
# gate revoked its resume state, and upstream play() could start while the OS
# still held the session. Exercise the actual policy, including denial/pause,
# and cross-check actual production native Rust APIs with the workspace cohort.
python3 "$ROOT/scripts/testing/check_cpal_ios_playback.py" "$ROOT" "$WORK/playback"
python3 "$ROOT/scripts/testing/check_ios_audio_source.py" "$ROOT" "$WORK/source"

# WASAPI workers used to discard priority registrations. Prove scoped ownership
# and the actual Windows product feature closure; native APIs run in Windows CI.
python3 "$ROOT/scripts/testing/check_cpal_scheduling.py" "$ROOT" "$WORK/scheduling"
