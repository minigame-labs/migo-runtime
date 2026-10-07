#!/usr/bin/env bash
# ============================================================
# Did removing PboPool::acquire's glClientWaitSync wait change upload throughput,
# and does it corrupt anything?
# Location: scripts/measure-pbo-stream-burst.sh
#
# Runs scripts/fixtures/pbo-stream-burst (64 x 512x512 texImage2D uploads,
# back-to-back, well past PboPool::DEFAULT_POOL_SIZE) and reports:
#   * wall time of the burst, from the fixture's own "uploaded N ... in Xms"
#     log line;
#   * whether every one of the 64 grid cells reads back the exact colour its
#     own texture was assigned -- the failure mode that would falsify the
#     "a fresh buffer's storage is always safe" reasoning in PboPool::acquire.
#
# Usage:
#   scripts/measure-pbo-stream-burst.sh [--device SERIAL] [--label L]
# ============================================================
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/device-fixture-runner.sh"

DEVICE="${ANDROID_SERIAL:-}"
LABEL="pbo-stream-burst"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --device) DEVICE="${2:?}"; shift 2 ;;
        --device=*) DEVICE="${1#*=}"; shift ;;
        --label) LABEL="${2:?}"; shift 2 ;;
        --label=*) LABEL="${1#*=}"; shift ;;
        *) shift ;;
    esac
done

if ! run_fixture_on_device pbo-stream-burst 4 "$DEVICE"; then
    echo "[$LABEL] could not run the probe on device" >&2
    exit 1
fi

burst_ms="$(grep -oE 'uploaded [0-9]+ [0-9]+x[0-9]+ textures in [0-9]+ms' "$RF_LOG" | grep -oE '[0-9]+ms' | grep -oE '[0-9]+' | tail -1)"
burst_ms="${burst_ms:-no-capture}"

echo "[$LABEL] frames=$RF_FRAMES burst_upload_ms=$burst_ms"
echo "[$LABEL] png=$RF_PNG log=$RF_LOG"

check_status=0
python3 "$SCRIPT_DIR/lib/grid_pixel_check.py" "$RF_PNG" 8 || check_status=$?

if [[ "$check_status" -ne 0 ]]; then
    echo "[$LABEL] FAIL: some grid cells did not read back the texture they were assigned -- a reused PBO's in-flight DMA landed on the wrong draw" >&2
    exit 1
fi
echo "[$LABEL] OK: burst_upload_ms=$burst_ms, all 64 cells read back their own texture correctly"
