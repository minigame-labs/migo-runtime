#!/usr/bin/env bash
# Where cargo writes a workspace's build output.
#
# A script that builds with cargo and then names engine/target/... itself reads whatever an
# earlier build left there whenever CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR or build.target-dir
# sends cargo elsewhere. Both halves succeed, so nothing says so: the player gates built a fresh
# player and ran one from two hours before, and the Android C host packaged a staticlib from
# August that no longer had the symbol its own header declares. Cargo answers instead, for every
# way a target directory can be chosen. scripts/test-cargo-target-dir-contract.sh keeps the
# literal out of scripts.
#
# Sourced, not executed: no `set -euo pipefail` (each caller sets its own).

# shellcheck source=scripts/lib/python-cmd.sh
source "${BASH_SOURCE[0]%/*}/python-cmd.sh"

# From inside the workspace, where the builds run: cargo reads .cargo/config.toml from the working
# directory, not from the manifest's.
cargo_target_dir() { # <workspace directory>
    local py
    py="$(python_cmd)" || return 1
    (cd "$1" && cargo metadata --format-version 1 --no-deps --offline) \
        | "$py" -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
}
