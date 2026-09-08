#!/usr/bin/env bash
# The shipping Apple package must actually consume the artifact it declares.
#
# THE DRIFT THIS EXISTS TO CATCH was live and green for the whole life of the
# skeleton. `platforms/apple/Package.swift` declares a `.binaryTarget` for
# MigoEngine.xcframework, and .github/workflows/apple-sdk.yml builds that
# xcframework on a macOS runner and then runs `swift build` against it -- a lane
# whose entire purpose is to check that the artifact is consumable. But no
# target named the binary target, so SwiftPM never had to find a slice matching
# the platform it was building for, and `swift build` passed on an xcframework
# that contained only an iOS slice while building for macOS. The lane was green
# and it was checking nothing about the artifact.
#
# That is the same shape as every other silent failure recorded in this
# repository: a check that cannot report the thing it is named after. So the
# property is now stated here rather than assumed -- at least one target depends
# on MigoEngine, which is what makes a missing or mismatched slice a build
# failure instead of a no-op.
#
# The second property is the opposite one, and it becomes load-bearing the
# moment the first is true. `MigoAppleWebKit` is the compatibility lane: WebKit
# runs the JavaScript and WebKit renders, and its manifest comment says it
# deliberately does not depend on MigoAppleRenderer, "linking a renderer it
# never drives would put ANGLE and Skia into an app that asked for the
# opposite". While nothing linked the engine that was a comment about intent.
# Now it is a claim about bytes, and one added dependency anywhere in its
# closure would pull the whole engine into the lane whose selling point is not
# having it.
#
# The third is the one `scripts/test-apple-swift-core-engine-free.sh` already
# applies to the engine-free package next door: SwiftPM compiles what a target's
# path names and silently ignores every other directory, so a source directory
# no target names is a set of files that never reaches a compiler while the
# build stays green. That package had the check and this one did not, which
# mattered as soon as this one grew a Tests/ directory.
#
# Every check carries a control that must fire. A detector that reports nothing
# because it parsed nothing reads exactly like a clean result, and this
# repository has drawn two confident wrong conclusions that way.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "${MIGO_APPLE_PACKAGE_CONTRACT_ROOT:-$ROOT}" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()
package_dir = root / "platforms" / "apple"
manifest_path = package_dir / "Package.swift"

if not manifest_path.is_file():
    print(f"ERROR: {manifest_path.relative_to(root)} not found", file=sys.stderr)
    sys.exit(1)

manifest = manifest_path.read_text()

# The module the xcframework vends, from the modulemap scripts/build-apple-sdk.sh
# writes. Depending on this name is what makes SwiftPM resolve a slice.
ENGINE_TARGET = "MigoEngine"
# The lane whose product claim is that it does not carry the engine.
ENGINE_FREE_LANE = "MigoAppleWebKit"

problems: list[str] = []
notes: list[str] = []


def balanced(text: str, start: int, opening: str, closing: str) -> str:
    """The substring from `start` (at `opening`) through its matching close.

    Written rather than regexed because the thing being read is nested: a
    dependencies array holds `.product(name:package:)` calls, and a regex that
    stops at the first `]` or `)` would take a target's `resources:` list for
    part of its dependencies.
    """
    depth = 0
    for index in range(start, len(text)):
        char = text[index]
        if char == opening:
            depth += 1
        elif char == closing:
            depth -= 1
            if depth == 0:
                return text[start : index + 1]
    return ""


# --- parse the manifest's targets --------------------------------------------

targets: dict[str, dict] = {}
covered_until = -1
for match in re.finditer(r"\.(binaryTarget|target|testTarget|executableTarget)\(", manifest):
    # A conditional dependency also spells .target(...). It is inside an
    # already parsed declaration, not a second declaration that may overwrite
    # the real binary target and erase its artifact path.
    if match.start() < covered_until:
        continue
    kind = match.group(1)
    block = balanced(manifest, match.end() - 1, "(", ")")
    if not block:
        problems.append(
            f"a .{kind}( in Package.swift has unbalanced parentheses; the parser stopped there"
        )
        continue
    covered_until = match.end() - 1 + len(block)

    name_match = re.search(r'name:\s*"([^"]+)"', block)
    if not name_match:
        problems.append(f"a .{kind}( in Package.swift declares no name")
        continue
    name = name_match.group(1)

    path_match = re.search(r'path:\s*"([^"]+)"', block)

    dependencies: list[str] = []
    deps_match = re.search(r"dependencies:\s*\[", block)
    if deps_match:
        array = balanced(block, deps_match.end() - 1, "[", "]")
        # Every quoted string in the array. `.product(name: "A", package: "B")`
        # contributes both "A" and "B"; "B" is a package name that matches no
        # target here and falls out as a leaf, which is the correct answer for
        # edge-walking within this manifest.
        dependencies = re.findall(r'"([^"]+)"', array)

    targets[name] = {
        "kind": kind,
        "path": path_match.group(1) if path_match else None,
        "dependencies": dependencies,
    }

if not targets:
    problems.append(
        "no targets parsed out of Package.swift; this gate inspected nothing and its "
        "clean results below would mean nothing"
    )
else:
    notes.append(f"parsed {len(targets)} target(s) from {manifest_path.relative_to(root)}")


def path_to(start: str, goal: str) -> list[str]:
    """The shortest dependency chain from `start` to `goal`, or [] if none.

    A chain rather than a reachable set, because the actionable part of a
    violation is the edge to delete. A set printed with arrows between its
    sorted members reads like a chain and is not one, which is a worse answer
    than no answer.
    """
    from collections import deque

    queue = deque([[start]])
    seen = {start}
    while queue:
        chain = queue.popleft()
        for nxt in targets.get(chain[-1], {}).get("dependencies", []):
            if nxt == goal:
                return chain + [nxt]
            if nxt not in seen:
                seen.add(nxt)
                queue.append(chain + [nxt])
    return []


# --- 1. something depends on the engine --------------------------------------
#
# The control is that the parser found dependency edges at all: if it read every
# target's dependencies as empty, "nothing depends on MigoEngine" would be true
# and meaningless.
if targets:
    if ENGINE_TARGET not in targets:
        problems.append(
            f"Package.swift declares no target named {ENGINE_TARGET}. The xcframework the "
            f"Apple SDK lane builds is consumed through that binary target; without it the "
            f"lane builds an artifact nothing reads."
        )
    edges = sum(len(t["dependencies"]) for t in targets.values())
    if edges == 0:
        problems.append(
            "the parser found no dependency edges in any target, so every reachability "
            "result below is vacuous. The manifest does declare dependencies."
        )
    else:
        notes.append(f"control: the parser found {edges} dependency edge(s)")
        consumers = sorted(
            name
            for name, target in targets.items()
            if ENGINE_TARGET in target["dependencies"] and name != ENGINE_TARGET
        )
        if not consumers:
            problems.append(
                f"no target depends on {ENGINE_TARGET}. SwiftPM only resolves a slice of a "
                f"binary target some target actually uses, so `swift build` in "
                f".github/workflows/apple-sdk.yml passes without ever looking at the "
                f"xcframework -- including when it holds no slice for the platform being "
                f"built. That lane exists to check the artifact; this is how it stops."
            )
        else:
            notes.append(f"{ENGINE_TARGET} is consumed by: {', '.join(consumers)}")

# --- 2. the compatibility lane does not carry the engine ---------------------
#
# The control is the other lane. Performance+ drives the renderer, so the engine
# must be in its closure; if the walk reports the engine in neither lane, the
# walk is broken and lane 1's clean result proves nothing.
if targets and ENGINE_TARGET in targets:
    if ENGINE_FREE_LANE not in targets:
        problems.append(f"Package.swift declares no target named {ENGINE_FREE_LANE}")
    else:
        engine_lanes = sorted(
            name for name in targets if name != ENGINE_TARGET and path_to(name, ENGINE_TARGET)
        )
        if not engine_lanes:
            problems.append(
                f"the dependency walk found {ENGINE_TARGET} in no target's closure, so the "
                f"clean result for {ENGINE_FREE_LANE} is not a finding."
            )
        else:
            notes.append(
                f"control: the walk reaches {ENGINE_TARGET} from {', '.join(engine_lanes)}"
            )
            chain = path_to(ENGINE_FREE_LANE, ENGINE_TARGET)
            if chain:
                via = " -> ".join(chain)
                problems.append(
                    f"{ENGINE_FREE_LANE} reaches {ENGINE_TARGET} via {via}. That lane is "
                    f"the compatibility baseline: WebKit runs the JavaScript and WebKit "
                    f"renders. Linking the engine there puts ANGLE and Skia into an app that "
                    f"chose the opposite, and nothing in it would ever call them."
                )
            else:
                notes.append(f"{ENGINE_FREE_LANE} does not reach {ENGINE_TARGET}")

# --- 3. every source directory is a target -----------------------------------
# The iOS runtime libraries must both travel through the native dependency
# closure. libEGL dlopens libGLESv2, so a linker cannot infer the second edge.
for runtime, artifact in (
    ("libEGL", "Frameworks/ANGLELibEGL-ios.xcframework"),
    ("libGLESv2", "Frameworks/ANGLELibGLESv2-ios.xcframework"),
):
    target = targets.get(runtime, {})
    if target.get("kind") != "binaryTarget" or target.get("path") != artifact:
        problems.append(f"ANGLE dependency {runtime} must name binary artifact '{artifact}'")
    for lane in ("MigoApplePerformancePlus", "MigoMacV8"):
        if not path_to(lane, runtime):
            problems.append(f"ANGLE dependency {runtime} is absent from {lane}'s native renderer closure")
    if path_to(ENGINE_FREE_LANE, runtime):
        problems.append(f"{ENGINE_FREE_LANE} reaches ANGLE dependency {runtime}")

# Binary targets are excluded on purpose: their path is a build output, checked
# against the script that produces it in section 4 rather than for existence.
source_paths = {
    target["path"]
    for target in targets.values()
    if target["path"] is not None and target["kind"] != "binaryTarget"
}
for kind in ("Sources", "Tests"):
    directory = package_dir / kind
    if not directory.is_dir():
        continue
    for child in sorted(p for p in directory.iterdir() if p.is_dir()):
        relative = f"{kind}/{child.name}"
        if relative not in source_paths:
            problems.append(
                f"platforms/apple/{relative} holds sources that no target in Package.swift "
                f"names, so SwiftPM never compiles them and the build stays green regardless."
            )
for declared in sorted(source_paths):
    if not (package_dir / declared).is_dir():
        problems.append(
            f"Package.swift names the target path '{declared}', which does not exist under "
            f"platforms/apple/."
        )
if source_paths:
    notes.append(f"target paths and source directories agree ({len(source_paths)} declared)")

# --- 4. the binary target names the path the build script writes -------------
#
# The one declared path that must NOT exist here: it is a build output of
# scripts/build-apple-sdk.sh, which needs macOS, Xcode and a compile of Skia. So
# the two are checked against each other instead. They are the only two places
# that name this path, they are edited by different kinds of work, and a
# disagreement between them cannot fail on this machine -- it fails on a Mac,
# after the build, as a package that does not resolve.
script_path = root / "scripts" / "build-apple-sdk.sh"
if ENGINE_TARGET in targets:
    declared_artifact = targets[ENGINE_TARGET]["path"]
    if declared_artifact is None:
        problems.append(f"the {ENGINE_TARGET} binary target declares no path")
    elif not script_path.is_file():
        problems.append(f"{script_path.relative_to(root)} not found; nothing to compare against")
    else:
        script = script_path.read_text()
        dir_match = re.search(r'^FRAMEWORKS_DIR="\$REPO_ROOT/([^"]+)"', script, re.MULTILINE)
        file_match = re.search(r'^XCFRAMEWORK="\$FRAMEWORKS_DIR/([^"]+)"', script, re.MULTILINE)
        if not dir_match or not file_match:
            problems.append(
                "could not read the xcframework path out of scripts/build-apple-sdk.sh "
                "(FRAMEWORKS_DIR / XCFRAMEWORK), so the comparison against the manifest "
                "was not performed and its silence is not a finding."
            )
        else:
            written = f"{dir_match.group(1)}/{file_match.group(1)}"
            expected_prefix = "platforms/apple/"
            if not written.startswith(expected_prefix):
                problems.append(
                    f"scripts/build-apple-sdk.sh writes the xcframework to '{written}', which is "
                    f"outside the Swift package at {expected_prefix}; the manifest cannot name it."
                )
            else:
                relative = written[len(expected_prefix) :]
                notes.append(f"control: the script writes the xcframework to {written}")
                if relative != declared_artifact:
                    problems.append(
                        f"the {ENGINE_TARGET} binary target names '{declared_artifact}' but "
                        f"scripts/build-apple-sdk.sh writes '{relative}'. Nothing on this machine "
                        f"can notice: the artifact is absent here either way, and the failure "
                        f"surfaces on a Mac as an unresolvable package after the build has run."
                    )
                else:
                    notes.append(
                        f"the binary target and the build script agree on '{declared_artifact}'"
                    )

# --- report -------------------------------------------------------------------

print()
for note in notes:
    print(f"  - {note}")
print()

if problems:
    print("FAIL: the shipping Apple package no longer consumes what it declares.", file=sys.stderr)
    print(file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    print(file=sys.stderr)
    sys.exit(1)

print("PASS: the shipping Apple package links the engine, the WebKit lane does not, and every source directory is compiled.")
PY
