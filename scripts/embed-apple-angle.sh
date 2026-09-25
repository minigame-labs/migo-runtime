#!/usr/bin/env bash
# Embed the SDK's macOS ANGLE pair into a host app before signing the app.
# iOS framework dependencies are embedded by Xcode from Package.swift.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$SCRIPT_DIR/apple-sdk-package.py" embed-angle "$@"
