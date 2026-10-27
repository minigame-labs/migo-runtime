#!/usr/bin/env bash
# The original helper selects a standalone explicit store even when the host
# supplies networkSecurityConfig. Tie the shipped patched jar to its source,
# prove the original/derived factory choices on actual bytecode, and keep all
# target execution networkless, read-only and bounded. Device trust is separate.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MIGO_TLS_JDK="${MIGO_TLS_JDK:-${JAVA_HOME_17_X64:-${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk-amd64}}}"
VENDOR="$ROOT/engine/third_party/rustls-platform-verifier-android-0.2.0"
python3 "$ROOT/scripts/lib/android_tls_vendor.py"
python3 "$ROOT/scripts/rebuild-android-tls-verifier.py" --fetch-tools --jdk "$MIGO_TLS_JDK"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
python3 - "$VENDOR" "$scratch" <<'PY'
from pathlib import Path
import sys
import zipfile
vendor, scratch = map(Path, sys.argv[1:])
with zipfile.ZipFile(vendor / 'rustls-platform-verifier-0.2.0.aar') as source:
    data = source.read('classes.jar')
if len(data) > 128 * 1024:
    raise ValueError('unexpected upstream bytecode size')
(scratch / 'upstream.jar').write_bytes(data)
PY
python3 "$ROOT/scripts/testing/android_tls_selector_probe.py" --jdk "$MIGO_TLS_JDK" \
    --jar "$scratch/upstream.jar" --expected explicit_keystore
python3 "$ROOT/scripts/testing/android_tls_selector_probe.py" --jdk "$MIGO_TLS_JDK" \
    --jar "$VENDOR/classes.jar" --expected default_host_config --factories-per-thread
