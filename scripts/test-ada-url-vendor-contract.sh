#!/usr/bin/env bash
# Ada upstream forced /MT and a global NODEFAULTLIB into embedding DLLs.
# Keep the narrow CRT correction reproducible; withheld patches and stray
# edits must fail the same byte-for-byte proof as the core/V8 forks.
# Public URL and networking previously used different parsers: ///redirects,
# path caret escaping and ASCII IDNA produced different requests or refusals.
# Execute the network's real adapter against the same pinned WPT constructors.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT" ada-url-patches
python3 "$ROOT/scripts/testing/test_prepare_url_wpt.py"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
python3 "$ROOT/scripts/testing/prepare_url_wpt.py" \
  "$ROOT/engine/crates/runtime-v8/testdata/url-wpt/urltestdata.json" "$WORK/constructors.json"
cd "$ROOT/engine"
cargo test --locked -p migo-services network::url::tests
MIGO_NETWORK_URL_WPT_FILE="$WORK/constructors.json" cargo test --locked -p migo-services \
  network::url::tests::network_url_wpt_constructors -- --ignored --exact --nocapture
