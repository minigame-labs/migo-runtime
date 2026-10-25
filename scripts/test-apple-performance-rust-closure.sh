#!/usr/bin/env bash
# The Performance+ product must contain no embedded JavaScript engine.
#
# This is the one claim the whole iOS fast lane rests on. Content JavaScript runs
# inside WebKit's WebContent process to get the system's JIT; the renderer runs
# here, in the app process, and it is allowed to be here *because* it is not a
# second JavaScript engine. An iOS build that quietly linked V8 would be
# shipping two engines, paying twice for working set and package size, and
# claiming a memory advantage it had just spent.
#
# THE DRIFT THIS EXISTS TO CATCH is not somebody adding V8 on purpose. It is a
# convenience dependency three levels down: `migo-capi -> migo-core ->
# migo-runtime-v8 -> deno_core` was the shape when this gate was written, and
# nothing about `migo-core`'s public surface says so. The dependency is a fact
# about the resolved graph, so only the resolved graph can answer it.
#
# It measures the SHIPPING PRODUCT, not a proxy. An earlier plan for this gate
# had it check a small probe crate that depended on the external-frame feature.
# A probe's closure being clean says nothing about the product's closure -- the
# question is what `migo-capi` resolves to when built the way the iOS product is
# built, and that is what is asked here.
#
# Host-only for the Cargo half. The archive half needs macOS tooling and runs
# when --artifact points at a built static library; it does not silently pass
# when it cannot run.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Package identities that must not appear. Matched against the package NAME
# from `cargo tree --prefix none --format {p}`, not against a line of tree art:
# a substring search over the rendered tree matches the comment characters and
# the version string too, and `v8` as a substring matches nothing useful at all.
FORBIDDEN=(
    migo-runtime-v8
    migo-runtime-jsc
    deno_core
    deno_ops
    deno_v8
    v8
    v8x
    rusty_v8
    v8_valueserializer
)

# The closures that must be free of an engine, as `package:features`.
#
# `migo-capi` is the one that matters: it is what the Apple SDK links, so it is
# what the product claim is about. `migo-core` is kept alongside it because a
# violation there is the more likely one and the message names the smaller
# graph, which is easier to read than the same package appearing three levels
# down in the larger one.
CLEAN_CLOSURES=(
    "migo-capi:external-frames"
    "migo-core:external-frames"
)

# And the control. `migo-core` built the ordinary way *does* embed V8, so this
# gate must be able to see it there.
#
# Without this, every way of breaking the detection -- a renamed package, a
# cargo-tree format change, an awk field that moved, a typo in the forbidden
# list -- reads as a pass. This repository has been bitten by exactly that
# shape often enough to have a note about it: a guard nobody has seen fail is a
# guard nobody should trust, and the cheapest way to see it fail is to keep a
# case in the gate that must fail.
POSITIVE_CONTROL="migo-capi:profile-full"
POSITIVE_CONTROL_EXPECTS=(migo-runtime-v8 deno_core deno_v8 v8)

artifact=""
while (( $# > 0 )); do
    case "$1" in
        --artifact) artifact="${2:?--artifact needs a path}"; shift 2 ;;
        --artifact=*) artifact="${1#*=}"; shift ;;
        *) echo "usage: $0 [--artifact <path to a built static library>]" >&2; exit 2 ;;
    esac
done

problems=()
notes=()

# --- half one: the resolved dependency graph ---------------------------------
#
# `-e normal` on purpose: a build-dependency or a dev-dependency on V8 does not
# put V8 in the shipped archive, and treating it as a violation would make the
# gate wrong in the direction that gets gates disabled.
resolve() {
    local package="${1%%:*}" features="${1#*:}"
    (cd engine && cargo tree \
        -p "$package" \
        --no-default-features \
        --features "$features" \
        -e normal \
        --prefix none \
        --color never \
        --format '{p}' 2>&1)
}

# The detector, and there is exactly one of it.
#
# Both the clean closures and the positive control go through this function, so
# the control exercises the same comparison the verdict rests on. An earlier
# version checked the control against the resolved tree directly, and deleting
# an entry from FORBIDDEN then went unnoticed: the control confirmed V8 was in
# the embedded build and never asked whether this gate's list would have caught
# it. A control on a parallel path is a control of nothing.
#
# Prints one banned package name per line, or nothing.
violations_in() {
    local tree="$1" package banned
    while read -r package; do
        [[ -n "$package" ]] || continue
        for banned in ${FORBIDDEN[@]+"${FORBIDDEN[@]}"}; do
            [[ "$package" == "$banned" ]] && echo "$package"
        done
    done < <(printf '%s\n' "$tree" | awk 'NF {print $1}' | sort -u)
}

package_count() {
    printf '%s\n' "$1" | awk 'NF {print $1}' | sort -u | wc -l
}

# Read a command's lines into a named array, without `mapfile`.
#
# That builtin arrived in bash 4.0 and macOS ships 3.2 -- it has since 2007, for
# licensing reasons, and GitHub's macOS images are no different. This gate did
# not run there until there was an Apple archive to audit, and its first run on
# the macOS lane died with `command not found`, exit 127. The workflow reported
# that as a failed gate; it was a gate that could not run at all.
collect_lines() {
    local __target="$1"; shift
    local __line
    eval "$__target=()"
    while IFS= read -r __line; do
        [ -n "$__line" ] || continue
        eval "$__target+=(\"\$__line\")"
    done
}

# New core releases route engines through a facade and optional alternate
# backend. Neither currently occurs in the old positive control, so inject
# their package identities through the same detector instead of waiting for a
# product to accidentally acquire one. A removed FORBIDDEN entry must go red.
for engine_alias in deno_v8 v8x; do
    if [[ "$(violations_in "$engine_alias v0.0.0")" != "$engine_alias" ]]; then
        problems+=("the detector misses the injected engine package $engine_alias")
    fi
done

for target in ${CLEAN_CLOSURES[@]+"${CLEAN_CLOSURES[@]}"}; do
    if ! tree="$(resolve "$target")"; then
        problems+=("cargo tree could not resolve $target:
$(printf '%s\n' "$tree" | sed 's/^/      /')")
        continue
    fi
    count="$(package_count "$tree")"
    notes+=("$target resolves $count packages")
    if (( count < 20 )); then
        problems+=("$target resolved only $count packages; an almost-empty closure is not a clean one")
    fi
    collect_lines found < <(violations_in "$tree")
    for banned in ${found[@]+"${found[@]}"}; do
        [[ -n "$banned" ]] || continue
        why="$(cd engine && cargo tree \
            -p "${target%%:*}" --no-default-features \
            --features "${target#*:}" -e normal \
            --invert "$banned" 2>/dev/null | head -20 || true)"
        problems+=("$banned is in the $target closure:
$(printf '%s\n' "$why" | sed 's/^/      /')")
    done
done

# The control: the embedded product must still be reported as containing the
# engine, BY THE DETECTOR ABOVE. If it is not, either the detector broke or the
# embedded product stopped embedding, and both are things to learn from a
# failure here rather than from a clean run that means nothing.
if ! control_tree="$(resolve "$POSITIVE_CONTROL")"; then
    problems+=("the positive control $POSITIVE_CONTROL does not resolve, so the detector is unverified:
$(printf '%s\n' "$control_tree" | sed 's/^/      /')")
else
    collect_lines detected < <(violations_in "$control_tree")
    missing=()
    for expected in ${POSITIVE_CONTROL_EXPECTS[@]+"${POSITIVE_CONTROL_EXPECTS[@]}"}; do
        found_one=no
        for package in ${detected[@]+"${detected[@]}"}; do
            [[ "$package" == "$expected" ]] && { found_one=yes; break; }
        done
        [[ "$found_one" == yes ]] || missing+=("$expected")
    done
    if (( ${#missing[@]} > 0 )); then
        problems+=("the detector did not report ${missing[*]} in $POSITIVE_CONTROL, which does contain them.
      Either FORBIDDEN lost an entry or the comparison broke; either way a clean
      result above proves nothing.")
    else
        notes+=("positive control: the detector reports ${POSITIVE_CONTROL_EXPECTS[*]} in $POSITIVE_CONTROL")
    fi
fi

# --- half one and a half: the build script's product selection ---------------
#
# A clean dependency graph for `migo-capi:external-frames` proves nothing if the
# thing that builds the archive asks for different features. The script used to
# build default `migo-capi` for every Apple slice, which is `profile-full`,
# which is an embedded V8 -- so the graph was clean and the artefact was not.
#
# Source-level, and that is the honest limit: the artefact half below needs a
# Mac. What is checked here is that the script cannot silently go back to one
# build for every product.
SDK_SCRIPT="scripts/build-apple-sdk.sh"
if [[ -f "$SDK_SCRIPT" ]]; then
    # The assignment, not the flag text. Matching the flags anywhere in the file
    # matched the script's own usage message, so removing them from the code
    # left this green -- the "a description is not a check" mistake, found by
    # injecting exactly that.
    if ! grep -q -- 'cargo_feature_flags=(--no-default-features --features external-frames)' \
        "$SDK_SCRIPT"; then
        problems+=("$SDK_SCRIPT does not build the Performance+ product with
      --no-default-features --features external-frames. Default features mean
      profile-full, which means an embedded engine.")
    fi
    # `cargo build -p migo-capi` must pass the product's feature flags rather
    # than relying on whatever the default is.
    if grep -qE 'cargo build -p migo-capi[^\\]*--target' "$SDK_SCRIPT" \
        && ! grep -q 'cargo_feature_flags\[@\]' "$SDK_SCRIPT"; then
        problems+=("$SDK_SCRIPT builds migo-capi without passing per-product features")
    fi
    notes+=("build script selects features per product")
fi

# --- half two: the built archive ---------------------------------------------
#
# The graph is necessary and not sufficient: a vendored object, a static
# library committed as a binary, or a linker script could put engine symbols in
# an archive whose Cargo graph is clean. Only the artifact answers that, and
# only on a host with the tooling.
if [[ -n "$artifact" ]]; then
    if [[ ! -f "$artifact" ]]; then
        problems+=("--artifact $artifact does not exist")
    else
        nm_tool=""
        for candidate in llvm-nm llvm-nm-18 nm; do
            if command -v "$candidate" >/dev/null 2>&1; then nm_tool="$candidate"; break; fi
        done
        if [[ -z "$nm_tool" ]]; then
            problems+=("--artifact was given but no nm is available; this is not a pass")
        else
            # A file, not a shell variable piped into grep.
            #
            # The obvious `printf '%s' "$symbols" | grep -q PATTERN` is a false
            # negative under `set -o pipefail`: `grep -q` exits the moment it
            # matches, `printf` then dies of SIGPIPE, and the pipeline's status
            # is the failure -- so the `if` reads "not found" precisely when it
            # was found. This gate shipped with that bug and reported a clean
            # archive for one containing seventy-six thousand `_ZN2v8` symbols;
            # it was caught by auditing the embedded archive on purpose, which
            # is the only reason to keep a positive control at all.
            symbol_dump="$(mktemp)"
            trap 'rm -f "$symbol_dump"' RETURN
            "$nm_tool" --defined-only "$artifact" >"$symbol_dump" 2>/dev/null || true
            symbol_count="$(wc -l <"$symbol_dump")"
            if (( symbol_count == 0 )); then
                problems+=("$nm_tool listed no defined symbols in $artifact; this is not a pass")
            else
                # Mangled names, because that is what `nm` prints. Searching for
                # `deno_core::` -- the demangled form -- finds nothing in a
                # symbol table full of `_ZN9deno_core`, which is the other half
                # of how this check used to pass on an archive full of V8.
                for pattern in '_ZN2v8' '_ZN9deno_core' 'v8::internal::' 'deno_core::' \
                    'JSGlobalContextCreate' 'JSEvaluateScript'; do
                    if grep -qF -- "$pattern" "$symbol_dump"; then
                        found="$(grep -cF -- "$pattern" "$symbol_dump")"
                        problems+=("$artifact defines $found symbol(s) matching $pattern, so it embeds a JavaScript engine")
                    fi
                done
                notes+=("archive audited with $nm_tool: $symbol_count defined symbols")
            fi
        fi
    fi
else
    notes+=("archive audit skipped: no --artifact given (the Cargo half still ran)")
fi

printf '\n'
for note in ${notes[@]+"${notes[@]}"}; do echo "  - $note"; done
printf '\n'

if (( ${#problems[@]} > 0 )); then
    echo "FAIL: the Performance+ product is not free of an embedded JavaScript engine." >&2
    printf '\n' >&2
    for problem in ${problems[@]+"${problems[@]}"}; do echo "  * $problem" >&2; done
    printf '\n' >&2
    cat >&2 <<'WHY'
  Why this matters: MigoApplePerformancePlus exists because content JavaScript
  runs in WebKit's WebContent process, where the system grants a JIT this
  process cannot have. A build that also links an engine here pays for two,
  gets a JIT for neither, and cannot make the memory claim the lane is for.
WHY
    exit 1
fi

echo "PASS: no JavaScript engine in the Performance+ closure."
