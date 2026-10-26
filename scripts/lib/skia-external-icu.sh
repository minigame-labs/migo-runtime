#!/usr/bin/env bash
# Builds Skia against V8's ICU instead of a second, bundled one.
#
# Skia used to link its own ICU (unrenamed `u_strCompare`, `icu::`) beside V8's (`u_strCompare_77`,
# `icu_77::`). The names never collide, so the linker kept both: 1.36 MB of libmigo.so on arm64,
# and two Unicode versions (74 and 77) in one process. The vendored skia-bindings
# (engine/third_party/skia-bindings-0.93.1, patch in engine/third_party/skia-bindings-patches/)
# reads SKIA_EXTERNAL_ICU_INCLUDE_DIRS and then builds Skia with `skia_use_system_icu` against
# those headers; the final link resolves Skia's ICU calls against V8's. The headers are the ones
# the v8 crate itself ships, so they are V8's ICU by construction, and a version mismatch is a
# link error (`_77` symbols), never a silent one.
#
# V8 installs ICU's data before its first isolate (runtime-v8/src/icu.rs); Skia reads that data.
#
# Sourced, not executed: no `set -euo pipefail` (each caller sets its own).

# shellcheck source=scripts/lib/python-cmd.sh
source "${BASH_SOURCE[0]%/*}/python-cmd.sh"

skia_external_icu_export() { # <workspace directory> <target triple>
    local py v8_dir common i18n
    py="$(python_cmd)" || return 1
    # --locked, not --offline: a cold CARGO_HOME has nothing to answer from yet, and this runs
    # before the build that would fetch it.
    v8_dir="$(cd "$1" && cargo metadata --format-version 1 --filter-platform "$2" --locked \
        | "$py" -c 'import json, os, sys
m = json.load(sys.stdin)
print(next(os.path.dirname(p["manifest_path"]) for p in m["packages"] if p["name"] == "v8"))')" || {
        echo "skia-external-icu: cannot locate the v8 crate through cargo metadata" >&2
        return 1
    }
    common="$v8_dir/third_party/icu/source/common"
    i18n="$v8_dir/third_party/icu/source/i18n"
    if [[ ! -f "$common/unicode/uvernum.h" || ! -f "$i18n/unicode/ucol.h" ]]; then
        echo "skia-external-icu: no ICU headers under $v8_dir/third_party/icu" >&2
        return 1
    fi
    export SKIA_EXTERNAL_ICU_INCLUDE_DIRS="$common:$i18n"
}
