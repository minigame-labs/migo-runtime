#!/usr/bin/env bash
# GlobalAlloc counters missed std Thread's System allocations. Rust-created
# threads also hid cold callback TLS initialization, and RTSan alone cannot
# distinguish a non-waiting futex wake from a futex wait through syscall().
# Combine cold native threads, native allocator interception and a trace of
# every syscall in the exact callback interval. This is a Linux host gate;
# device scheduling and other native backends require their own evidence.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_audio_realtime.py" "$ROOT"
