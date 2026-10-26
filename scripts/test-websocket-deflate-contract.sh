#!/usr/bin/env bash
# perMessageDeflate was silently ignored, upstream tungstenite rejects RSV1,
# and candidate decoders did not share Migo's message/expansion/cancel budgets.
# Keep strict negotiation, the narrow frame hook and retained async state one
# tested contract; neither a fast codec nor source-only checks prove this alone.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT" tungstenite-patches
node --experimental-vm-modules "$ROOT/scripts/testing/websocket-options.mjs"
cd "$ROOT/engine"
cargo test --locked -p migo-services network::websocket
