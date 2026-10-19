#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
from __future__ import annotations

import pathlib
import sys

root = pathlib.Path(sys.argv[1])
game_session = root / "platforms/android/library/src/main/java/com/migo/runtime/GameSession.java"
terminal_cleanup = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/TerminalCleanupState.java"
)
jni = root / "engine/crates/platform/src/android/jni/inbound.rs"
host_owners = root / "engine/crates/platform/src/host_owners.rs"
native_bridge = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/NativeBridge.java"
)
native_methods = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/NativeMethods.java"
)
native_exports = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/NativeExports.java"
)

java = game_session.read_text(encoding="utf-8")
terminal = terminal_cleanup.read_text(encoding="utf-8")
rust = jni.read_text(encoding="utf-8")
owners = host_owners.read_text(encoding="utf-8")
bridge = native_bridge.read_text(encoding="utf-8")
methods = native_methods.read_text(encoding="utf-8")
exports = native_exports.read_text(encoding="utf-8")

def body_after(source: str, marker: str) -> str:
    start = source.index(marker)
    open_brace = source.index("{", start)
    depth = 0
    for index in range(open_brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[open_brace:index]
    raise SystemExit(f"body is malformed after {marker}")

def matching_delimiter(source: str, start: int, opening: str, closing: str) -> int:
    depth = 0
    quote = None
    escaped = False
    index = start
    while index < len(source):
        char = source[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            index += 1
            continue
        if char in ('"', "'"):
            quote = char
        elif source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = len(source) if newline < 0 else newline + 1
            continue
        elif source.startswith("/*", index):
            end = source.find("*/", index + 2)
            if end < 0:
                raise SystemExit("unterminated block comment")
            index = end + 2
            continue
        elif char == opening:
            depth += 1
        elif char == closing:
            depth -= 1
            if depth == 0:
                return index
        index += 1
    raise SystemExit(f"unbalanced {opening}{closing} after offset {start}")

def split_top_level(source: str) -> list[str]:
    parts = []
    start = 0
    parens = braces = brackets = 0
    quote = None
    escaped = False
    index = 0
    while index < len(source):
        char = source[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            index += 1
            continue
        if char in ('"', "'"):
            quote = char
        elif source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = len(source) if newline < 0 else newline + 1
            continue
        elif source.startswith("/*", index):
            end = source.find("*/", index + 2)
            if end < 0:
                raise SystemExit("unterminated block comment")
            index = end + 2
            continue
        elif char == "(":
            parens += 1
        elif char == ")":
            parens -= 1
        elif char == "{":
            braces += 1
        elif char == "}":
            braces -= 1
        elif char == "[":
            brackets += 1
        elif char == "]":
            brackets -= 1
        elif char == "," and parens == braces == brackets == 0:
            parts.append(source[start:index].strip())
            start = index + 1
        index += 1
    parts.append(source[start:].strip())
    return parts

def invocation_arguments(source: str, marker: str) -> list[str]:
    start = source.index(marker)
    open_paren = source.index("(", start + len(marker))
    close_paren = matching_delimiter(source, open_paren, "(", ")")
    return split_top_level(source[open_paren + 1:close_paren])

def compact(source: str) -> str:
    return "".join(source.split())

close_body = body_after(java, "public void close()")
attempt_args = invocation_arguments(close_body, "terminalCleanup.attempt")
if len(attempt_args) != 6:
    raise SystemExit(
        "GameSession.close() must pass resource cleanup, native shutdown, and four "
        "ownership-release "
        f"actions to TerminalCleanupState.attempt(); found {len(attempt_args)} arguments"
    )

resource_phase = compact(attempt_args[0])
if not resource_phase.startswith("()->ResourceCleanup.runAll(") or not resource_phase.endswith(")"):
    raise SystemExit(
        "GameSession.close() resource phase must be a ResourceCleanup.runAll() action"
    )
resource_actions = [compact(action) for action in invocation_arguments(
    attempt_args[0], "ResourceCleanup.runAll"
)]
required_resource_actions = [
    "()->NativeExports.closePermissionOperations(sessionId)",
    "()->NativeExports.destroyAllManagers(sessionId)",
]
missing = [action for action in required_resource_actions if action not in resource_actions]
if missing:
    raise SystemExit(f"GameSession resource-cleanup phase is missing: {missing}")
native_shutdown = compact(attempt_args[1])
if native_shutdown != "this::shutdownNativeOnce":
    raise SystemExit(
        "GameSession native shutdown barrier must be this::shutdownNativeOnce; "
        f"found {native_shutdown}"
    )

ownership_actions = [compact(action) for action in attempt_args[2:]]
# Exact and ordered on purpose, which is why this list is maintained rather than
# derived: it forbids an action being silently dropped as much as one being added, and
# the order is load-bearing -- stop resolving the session, clean the directory it owns,
# stop its logging, and drop the native registration last. `Logger.unregisterSession`
# arrived with per-session log levels and this list was not updated with it, so the gate
# was red rather than wrong.
required_ownership_actions = [
    "()->RuntimeRegistry.unregister(sessionId)",
    "paths::cleanupTemp",
    "()->Logger.unregisterSession(sessionId)",
    "()->NativeExports.unregisterSession(sessionId)",
]
if ownership_actions != required_ownership_actions:
    raise SystemExit(
        "GameSession ownership-release phase must contain registry unregister, temporary "
        "cleanup, logger unregister, and session unregister in order; "
        f"found {ownership_actions}"
    )

compact_close = compact(close_body)
phase_anchors = [
    *required_resource_actions,
    native_shutdown,
    *required_ownership_actions,
]
for anchor in phase_anchors:
    if compact_close.count(anchor) != 1:
        raise SystemExit(
            "GameSession.close() must contain each terminal phase action exactly once; "
            f"found {compact_close.count(anchor)} copies of {anchor}"
        )

shutdown_body = body_after(java, "private void shutdownNativeOnce()")
compact_shutdown = compact(shutdown_body)
if compact_shutdown.count("NativeMethods.shutdown(sessionId)") != 1:
    raise SystemExit(
        "GameSession.shutdownNativeOnce() must own exactly one native shutdown/join attempt"
    )
if "if(!NativeMethods.shutdown(sessionId))" not in compact_shutdown:
    raise SystemExit("GameSession.shutdownNativeOnce() must treat native false as retryable failure")
if compact_shutdown.index("NativeMethods.shutdown(sessionId)") >= compact_shutdown.index(
        "nativeShutdownComplete=true"):
    raise SystemExit("GameSession must mark native shutdown complete only after JNI succeeds")
for detached_anchor in ("new Thread", ".start()", ".join("):
    if detached_anchor in shutdown_body:
        raise SystemExit(
            "GameSession.shutdownNativeOnce() must not detach native shutdown behind a worker"
        )

attempt_body = body_after(terminal, "public Result attempt(")
attempt_try = body_after(attempt_body, "try")
# Resources, then native shutdown, then ownership -- and the first two as independent actions.
# They ran one after the other, so a resource whose release threw skipped the native shutdown:
# the engine kept running the game its player had closed, for the life of the process, since
# MigoGameActivity drops the session it has closed and nothing retries.
resource_and_native = "ResourceCleanup.runAll(resourceCleanup, nativeShutdown);"
ownership_run = "ResourceCleanup.runAll(ownershipRelease);"
if resource_and_native not in attempt_try or ownership_run not in attempt_try:
    raise SystemExit(
        "TerminalCleanupState.attempt() must run the resource and native phases as independent "
        "actions, resources first, so a resource that will not let go cannot keep the engine "
        "running, and then the ownership phase"
    )
if attempt_try.index(resource_and_native) >= attempt_try.index(ownership_run):
    raise SystemExit(
        "TerminalCleanupState.attempt() must release ownership only after the resource and "
        "native phases"
    )

if "runWithTimeout" in java:
    raise SystemExit(
        "GameSession shutdown must not detach behind a timeout; close() owns the join"
    )

shutdown_rust = body_after(rust, "pub(crate) extern \"system\" fn shutdown")
for anchor in ("-> jboolean", "host_owners().shutdown_with", "host.shutdown_and_join()"):
    if anchor not in rust[rust.index("pub(crate) extern \"system\" fn shutdown"):]:
        raise SystemExit(f"Android JNI shutdown retry anchor missing: {anchor}")
success_arm = body_after(shutdown_rust, "Ok(had_owner) =>")
failure_arm = body_after(shutdown_rust, "Err(error) =>")
if "crate::android::services::clear_permissions(host_id)" not in success_arm:
    raise SystemExit("Android JNI shutdown must clear permissions after successful join")
if "clear_permissions" in failure_arm or "JNI_FALSE" not in failure_arm:
    raise SystemExit("Android JNI shutdown failure must retain permissions and return false")

shutdown_with = body_after(owners, "pub(crate) fn shutdown_with")
for anchor in ("self.take(host_id)", "self.insert(host)", "Err(error)"):
    if anchor not in shutdown_with:
        raise SystemExit(f"HostOwners retry ownership anchor missing: {anchor}")

if "static native boolean shutdown(int sessionId);" not in bridge:
    raise SystemExit("NativeBridge.shutdown must expose a boolean retry contract")
native_methods_shutdown = body_after(methods, "public static boolean shutdown(int sessionId)")
if "sSessionShutdown.shutdown(sessionId)" not in native_methods_shutdown:
    raise SystemExit("NativeMethods.shutdown must preserve the JNI boolean result")

destroy_body = body_after(exports, "public static void destroyAllManagers(int sessionId)")
destroy_actions = [compact(action) for action in invocation_arguments(
    destroy_body, "ResourceCleanup.runAll"
)]
for action in (
    "()->clearAdHandler(sessionId)",
    "()->sAdSinks.remove(sessionId)",
    "()->clearPermissionHandler(sessionId)",
    "()->sPermissionSinks.remove(sessionId)",
):
    if action not in destroy_actions:
        raise SystemExit(
            f"NativeExports.destroyAllManagers() resource phase is missing: {action}"
        )

for sink_name in ("SessionAdEventSink", "SessionPermissionSink"):
    sink_source = body_after(exports, f"class {sink_name}")
    if "isSessionTerminated(sessionId)" not in sink_source:
        raise SystemExit(f"{sink_name} does not reject callbacks after session teardown")

print("Android owned Host shutdown contract: PASS")
PY
