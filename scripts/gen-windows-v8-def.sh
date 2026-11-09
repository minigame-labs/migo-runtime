#!/usr/bin/env bash
# Regenerate the Windows rusty_v8 DLL export list.
#
# The exports are read from the symbols rusty_v8 actually defines rather than
# transcribed from binding.cc: a hand-kept list silently drifts when upstream
# adds or renames a binding, and the failure mode is a missing export that only
# shows up as an unresolved symbol in a downstream link. GNU nm reads the COFF
# archive directly, so this runs on Linux and needs no Windows round-trip.
#
# The selection is "everything the binding translation unit defines that is not
# C++-mangled", NOT a name prefix. An earlier version filtered on `v8__` and
# looked right -- 646 symbols -- but silently dropped 68 more the Rust side
# calls: v8_inspector__*, cppgc__*, std__shared_ptr__*, icu_*, and
# {Serialize,Deserialize}InternalFields, which carry no shared prefix at all.
# That only surfaced as 50 unresolved externals when a consumer finally linked
# the DLL. Restricting to binding.obj is what makes "not mangled" sufficient:
# the rest of the archive is V8's own C++, whose symbols are all mangled anyway,
# but scoping to the one translation unit that defines the C surface says what
# is meant instead of relying on that.
#
# Usage: bash scripts/gen-windows-v8-def.sh [path/to/rusty_v8.lib]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB="${1:-$ROOT/engine/third_party/rusty_v8/x86_64-pc-windows-msvc/rusty_v8.lib}"
OUT="$ROOT/engine/third_party/v8-patches/rusty_v8-windows-exports.def"

[[ -f "$LIB" ]] || { printf 'no rusty_v8.lib at %s\n' "$LIB" >&2; exit 1; }
command -v nm >/dev/null || { printf 'nm not found\n' >&2; exit 1; }

symbols="$(nm -g --defined-only "$LIB" 2>/dev/null \
  | awk '
      /^.*\.obj:$/ { in_binding = ($0 ~ /binding\.obj:$/); next }
      in_binding && $2 == "T" && $3 !~ /^\?/ { print $3 }
    ' | LC_ALL=C sort -u)"
count="$(printf '%s\n' "$symbols" | grep -c .)"

# Named with ICU's major version, so read from the vendored crate rather than
# written here: an ICU bump renames it.
icu_sources=("$ROOT"/engine/third_party/v8-*/src/icu.rs)
(( ${#icu_sources[@]} == 1 )) && [[ -f "${icu_sources[0]}" ]] \
  || { printf 'expected one vendored v8 crate, found: %s\n' "${icu_sources[*]}" >&2; exit 1; }
icu_entry="$(grep -oE 'fn udata_setCommonData_[0-9]+' "${icu_sources[0]}" | sed 's/^fn //' | sort -u)"
[[ "$icu_entry" =~ ^udata_setCommonData_[0-9]+$ ]] \
  || { printf 'cannot read the ICU data entry point from %s\n' "${icu_sources[0]}" >&2; exit 1; }
# A plausibility floor, not an exact pin: upstream may add bindings, but a jump
# to near-zero means nm read the wrong file or the filter stopped matching.
(( count >= 700 )) || { printf 'only %s exports found; refusing to write\n' "$count" >&2; exit 1; }

{
  printf '; Export surface of the Windows rusty_v8 DLL.\n'
  printf '; Generated -- do not hand-edit. Regenerate with:\n'
  printf ';   bash scripts/gen-windows-v8-def.sh\n'
  printf '; The list is derived from the symbols rusty_v8 actually defines, so it\n'
  printf '; cannot drift from binding.cc the way a hand-kept list would.\n'
  printf 'LIBRARY rusty_v8\n'
  printf 'EXPORTS\n'
  printf '%s\n' "$symbols" | sed 's/^/    /'
  # Added by the migo host-callback-registration patch, so neither is in the
  # archive the list above is read from.
  printf '    v8__register_host_callback\n'
  printf '    v8__host_callbacks_ready\n'
  # The ICU data entry point the v8 crate calls itself (src/icu.rs, behind
  # set_common_data_NN): ICU's own function, not binding.cc's, so the scan above
  # does not see it. V8 is built with ICU's stub data (icu_use_stub_data), so
  # every isolate needs it, and a DLL without it fails the consumer's link with
  # one unresolved udata_setCommonData_NN.
  printf '    %s\n' "$icu_entry"
} > "$OUT"

printf 'wrote %s (%s exports + 2 registration entry points + %s)\n' "$OUT" "$count" "$icu_entry"
