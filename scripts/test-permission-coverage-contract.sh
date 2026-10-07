#!/usr/bin/env bash
# Every permission-sensitive op must be explicitly classified as gated or cleanup.
#
# A permission system that covers most of its surface is not most of a
# permission system. It advertises a guarantee it does not hold, to a customer
# who is making a compliance decision on the strength of it -- the same defect
# shape as an ad bridge that lets content mint its own rewards, and the reason
# this gate exists before the enforcement it checks.
#
# Service-wide capabilities are still discovered from their accessors, but a
# matched op must appear in exactly one policy table. This makes cleanup
# exemptions reviewable instead of silently weakening an entire accessor, and
# also covers APIs such as album writes and user info that share an accessor
# with unscoped operations.
#
# `PERMISSION_GATED_OPS` entries must contain the matching guard;
# `PERMISSION_CLEANUP_OPS` entries must not contain one.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" "$@" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys
import xml.etree.ElementTree as ET

root = pathlib.Path(sys.argv[1]).resolve()
self_test = len(sys.argv) > 2 and sys.argv[2] == "--self-test"

mapping_rs = root / "engine/crates/shared/src/services/permission.rs"
helper_rs = root / "engine/crates/runtime-v8/src/permission.rs"
runtime_src = root / "engine/crates/runtime-v8/src"
android_services_rs = root / "engine/crates/platform/src/android/services/mod.rs"
full_manifest = root / "platforms/android/library/src/full/AndroidManifest.xml"
slim_manifest = root / "platforms/android/library/src/main/AndroidManifest.xml"
native_exports_java = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/NativeExports.java"
)
location_provider_java = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/platform/LocationProvider.java"
)

for required in (
    mapping_rs, helper_rs, runtime_src, android_services_rs, full_manifest, slim_manifest,
    native_exports_java, location_provider_java
):
    if not required.exists():
        print(f"ERROR: {required} not found; this gate cannot check anything", file=sys.stderr)
        sys.exit(1)


def strip_comments(text: str) -> str:
    """Commented-out code is not an implementation.

    Applied before every match below, because a `// require_scope(...)` reads as
    protection to a grep and as nothing at all to a compiler.
    """
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())


failures: list[str] = []


def unique_entries(table: str, entries: list[tuple[str, str]]) -> dict[str, str]:
    unique: dict[str, str] = {}
    for name, scope in entries:
        if name in unique:
            failures.append(f"`{table}` contains duplicate name `{name}`")
            continue
        unique[name] = scope
    return unique

mapping_source = strip_comments(mapping_rs.read_text(encoding="utf-8"))
helper_source = strip_comments(helper_rs.read_text(encoding="utf-8"))
android_services_source = strip_comments(android_services_rs.read_text(encoding="utf-8"))
full_manifest_source = full_manifest.read_text(encoding="utf-8")
slim_manifest_source = slim_manifest.read_text(encoding="utf-8")
native_exports_source = strip_comments(native_exports_java.read_text(encoding="utf-8"))
location_provider_source = strip_comments(location_provider_java.read_text(encoding="utf-8"))


def read_table(source: str, table: str, source_path: pathlib.Path) -> dict[str, str]:
    block = re.search(
        rf"{table}\s*:\s*&\[\(&str,\s*Scope\)\]\s*=\s*&\[(?P<body>.*?)\];",
        source,
        re.DOTALL,
    )
    if not block:
        failures.append(f"{source_path.relative_to(root)}: missing `{table}` table")
        return {}
    entries = re.findall(
        r'\(\s*"([A-Za-z0-9_:]+)"\s*,\s*Scope::([A-Za-z]+)\s*,?\s*\)',
        block.group("body"),
    )
    if not entries:
        failures.append(f"{source_path.relative_to(root)}: `{table}` is empty")
        return {}
    return unique_entries(table, entries)


def read_all_tables(mapping: str, helper: str, android: str) -> tuple[
    dict[str, str], dict[str, str], dict[str, str], dict[str, str],
    dict[str, str], dict[str, str]
]:
    return (
        read_table(mapping, "GATED_ACCESSORS", mapping_rs),
        read_table(mapping, "GATED_SERVICE_METHODS", mapping_rs),
        read_table(helper, "PERMISSION_GATED_OPS", helper_rs),
        read_table(helper, "PERMISSION_CLEANUP_OPS", helper_rs),
        read_table(android, "ANDROID_PERMISSION_GATED_METHODS", android_services_rs),
        read_table(android, "ANDROID_PERMISSION_CLEANUP_METHODS", android_services_rs),
    )


def inject_first_entry_duplicate(source: str, table: str) -> str:
    block = re.search(
        rf"{table}\s*:\s*&\[\(&str,\s*Scope\)\]\s*=\s*&\[(?P<body>.*?)\];",
        source,
        re.DOTALL,
    )
    if not block:
        return source
    entry = re.search(
        r'\(\s*"[A-Za-z0-9_:]+"\s*,\s*Scope::[A-Za-z]+\s*,?\s*\)',
        block.group("body"),
    )
    if not entry:
        return source
    insertion = block.start("body")
    return source[:insertion] + entry.group(0) + ",\n" + source[insertion:]


def matching_brace(source: str, opening: int) -> int:
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    raise ValueError(f"unclosed brace at offset {opening}")


ANDROID_IMPL = re.compile(r"\bimpl\s+([A-Za-z]+Service)\s+for\s+[A-Za-z0-9_]+\s*\{")
ANDROID_METHOD = re.compile(r"(?:^|\n)\s*fn\s+([a-z_0-9]+)\s*\(")


def parse_android_methods(source: str) -> tuple[dict[str, tuple[int, int, str]], list[str]]:
    parsed: dict[str, tuple[int, int, str]] = {}
    problems: list[str] = []
    for impl_match in ANDROID_IMPL.finditer(source):
        service = impl_match.group(1)
        impl_start = source.find("{", impl_match.start())
        try:
            impl_end = matching_brace(source, impl_start)
        except ValueError as error:
            problems.append(str(error))
            continue
        impl_body = source[impl_start:impl_end]
        for method_match in ANDROID_METHOD.finditer(impl_body):
            name = method_match.group(1)
            absolute_start = impl_start + method_match.start()
            opening = source.find("{", impl_start + method_match.end(), impl_end)
            if opening < 0:
                problems.append(f"{service}::{name} has no body")
                continue
            try:
                end = matching_brace(source, opening)
            except ValueError as error:
                problems.append(str(error))
                continue
            identifier = f"{service}::{name}"
            if identifier in parsed:
                problems.append(f"duplicate Android service method `{identifier}`")
                continue
            parsed[identifier] = (absolute_start, end, source[absolute_start:end])
    return parsed, problems


# The first argument is the session's permission-gate handle, resolved once when
# its device services are built. It used to be `self.host_id`, which made every
# gated call look the session up in the gate's process-wide live-host map -- the
# cross-session lock on a per-event path that its ownership rule forbids.
ANDROID_GATE_CALL = re.compile(
    r"\bpermission_jni_call\s*\(\s*&self\.session\s*,\s*"
    r"(?P<policy>None|Some\(Scope::(?P<scope>[A-Za-z]+)\))\s*,"
)


def validate_android_services(
    source: str,
    gated: dict[str, str],
    cleanup: dict[str, str],
) -> list[str]:
    problems: list[str] = []
    parsed, parse_problems = parse_android_methods(source)
    problems.extend(parse_problems)

    for duplicate in sorted(gated.keys() & cleanup.keys()):
        problems.append(f"Android `{duplicate}` is both gated and cleanup")

    for identifier, expected_scope in sorted(gated.items()):
        method = parsed.get(identifier)
        if method is None:
            problems.append(f"Android gated `{identifier}` does not exist")
            continue
        calls = list(ANDROID_GATE_CALL.finditer(method[2]))
        if len(calls) != 1:
            problems.append(
                f"Android gated `{identifier}` has {len(calls)} permission_jni_call wrapper(s)"
            )
            continue
        actual_scope = calls[0].group("scope")
        if calls[0].group("policy") == "None" or actual_scope != expected_scope:
            problems.append(
                f"Android gated `{identifier}` expected Scope::{expected_scope}, "
                f"found {calls[0].group('policy')}"
            )

    for identifier, expected_scope in sorted(cleanup.items()):
        method = parsed.get(identifier)
        if method is None:
            problems.append(f"Android cleanup `{identifier}` does not exist")
            continue
        calls = list(ANDROID_GATE_CALL.finditer(method[2]))
        if len(calls) != 1:
            problems.append(
                f"Android cleanup `{identifier}` has {len(calls)} permission_jni_call wrapper(s)"
            )
            continue
        if calls[0].group("policy") != "None":
            problems.append(
                f"Android cleanup `{identifier}` for Scope::{expected_scope} expected None, "
                f"found {calls[0].group('policy')}"
            )

    classified = gated.keys() | cleanup.keys()
    for identifier, (_, _, body) in parsed.items():
        if ANDROID_GATE_CALL.search(body) and identifier not in classified:
            problems.append(f"Android wrapped `{identifier}` is not classified")
    return problems


def service_trait_for_accessor(accessor: str) -> str:
    return "".join(part.capitalize() for part in accessor.split("_")) + "Service"


def derive_runtime_sensitive_methods(
    gated_accessors: dict[str, str],
    android_gated_methods: dict[str, str],
    android_cleanup_methods: dict[str, str],
) -> tuple[dict[str, str], list[str]]:
    """Derive per-method scopes from production wrappers on mixed services."""
    problems: list[str] = []
    service_wide = {
        service_trait_for_accessor(accessor) for accessor in gated_accessors
    }
    derived: dict[str, str] = {}
    policies = {**android_gated_methods, **android_cleanup_methods}
    for identifier, scope in sorted(policies.items()):
        service, separator, method = identifier.partition("::")
        if not separator:
            problems.append(f"Android policy identifier `{identifier}` has no service")
            continue
        if service in service_wide:
            continue
        previous = derived.get(method)
        if previous is not None and previous != scope:
            problems.append(
                f"mixed-service method `{method}` has conflicting scopes "
                f"Scope::{previous} and Scope::{scope}"
            )
            continue
        derived[method] = scope
    return derived, problems


def validate_runtime_method_inventory(
    declared: dict[str, str], derived: dict[str, str]
) -> list[str]:
    problems: list[str] = []
    for method, scope in sorted(derived.items()):
        if declared.get(method) != scope:
            problems.append(
                f"`GATED_SERVICE_METHODS` does not match production-derived "
                f"`{method}` Scope::{scope}"
            )
    for method, scope in sorted(declared.items()):
        if derived.get(method) != scope:
            problems.append(
                f"`GATED_SERVICE_METHODS` declares `{method}` Scope::{scope} without a "
                "matching production permission wrapper"
            )
    return problems


def mutate_android_method(source: str, identifier: str, mutate) -> str:
    parsed, _ = parse_android_methods(source)
    start, end, body = parsed[identifier]
    changed = mutate(body)
    return source[:start] + changed + source[end:]


def inject_android_method(source: str, service: str, method_source: str) -> str:
    for impl_match in ANDROID_IMPL.finditer(source):
        if impl_match.group(1) != service:
            continue
        impl_start = source.find("{", impl_match.start())
        impl_end = matching_brace(source, impl_start)
        return source[:impl_end - 1] + method_source + source[impl_end - 1:]
    raise ValueError(f"Android implementation for {service} not found")


# The policy and the manifest parser are shared with
# test-android-merged-manifest-permissions.sh, which holds the *merged* manifests to
# the same table. Two copies would be two statements of one rule.
sys.path.insert(0, str(root / "scripts/lib"))
from android_permission_policy import (  # noqa: E402
    ANDROID_NS,
    FULL_PERMISSION_POLICY,
    manifest_permissions,
)


def validate_permission_manifests(full: str, slim: str) -> list[str]:
    problems: list[str] = []
    full_permissions, full_problems = manifest_permissions(full)
    slim_permissions, slim_problems = manifest_permissions(slim)
    problems.extend(full_problems)
    problems.extend(slim_problems)
    for name, expected_max in FULL_PERMISSION_POLICY.items():
        if name not in full_permissions:
            problems.append(f"Full manifest missing `{name}`")
        elif full_permissions[name] != expected_max:
            problems.append(
                f"Full manifest `{name}` maxSdkVersion is "
                f"{full_permissions[name]!r}, expected {expected_max!r}"
            )
        if name in slim_permissions:
            problems.append(f"Slim manifest must not merge dangerous permission `{name}`")
    return problems


def delete_manifest_permission(source: str, name: str) -> str:
    pattern = re.compile(
        r"\s*<uses-permission\b(?=[^>]*android:name=\"" + re.escape(name)
        + r"\")[^>]*/>"
    )
    return pattern.sub("", source, count=1)


def java_method_body(source: str, name: str) -> str | None:
    method = re.search(
        rf"\b(?:public|private)\s+static\s+[A-Za-z0-9_.<>\[\]]+\s+{name}\s*\(",
        source,
    )
    if not method:
        return None
    opening = source.find("{", method.end())
    if opening < 0:
        return None
    return source[method.start():matching_brace(source, opening)]


def validate_location_linearization(native: str, provider: str) -> list[str]:
    problems: list[str] = []
    for method_name in ("getLocation", "getFuzzyLocation"):
        body = java_method_body(native, method_name)
        if body is None:
            problems.append(f"NativeExports.{method_name} is missing")
            continue
        if not re.search(
            r'sPermissionOperations\.register\s*\(\s*sessionId\s*,\s*'
            r'"scope\.userLocation"\s*\)',
            body,
        ):
            problems.append(f"NativeExports.{method_name} does not register a location token")
        if "sPermissionOperations.enter(pending" not in body:
            problems.append(f"NativeExports.{method_name} bypasses the Java permission gate")

    request = java_method_body(provider, "requestSingleUpdateAsync")
    if request is None:
        problems.append("LocationProvider.requestSingleUpdateAsync is missing")
        return problems
    cancellation = re.search(
        r"pending\.setCancellation\s*\(\s*\(\)\s*->\s*"
        r"request\[0\]\.cancel\(fallback\)\s*\);",
        request,
    )
    if cancellation is None:
        problems.append("location denial does not synchronously cancel pending framework updates")
    for cleanup_boundary in (
        "lm.removeUpdates(listener)",
        "MAIN_HANDLER.removeCallbacks(timeout[0])",
    ):
        if cleanup_boundary not in request:
            problems.append(
                f"retained location request is missing `{cleanup_boundary}`"
            )
    for retained_boundary in (
        "removeListener = !listenerRemoved",
        "removeTimeout = !timeoutRemoved",
        "released = true",
    ):
        if retained_boundary not in provider:
            problems.append(
                f"retained location cleanup is missing `{retained_boundary}`"
            )
    deferred = re.search(
        r"gate\.enter\s*\(\s*pending\s*,\s*\(\)\s*->\s*\{(?P<body>.*?)\}\s*\);",
        request,
        re.DOTALL,
    )
    if deferred is None or "lm.requestSingleUpdate" not in deferred.group("body"):
        problems.append("deferred location framework entry bypasses the permission gate")
    return problems


(
    accessors,
    methods,
    gated_ops,
    cleanup_ops,
    android_gated,
    android_cleanup,
) = read_all_tables(mapping_source, helper_source, android_services_source)
derived_methods, derived_method_problems = derive_runtime_sensitive_methods(
    accessors, android_gated, android_cleanup
)

if self_test:
    for table, owner in (
        ("GATED_ACCESSORS", "mapping"),
        ("GATED_SERVICE_METHODS", "mapping"),
        ("PERMISSION_GATED_OPS", "helper"),
        ("PERMISSION_CLEANUP_OPS", "helper"),
        ("ANDROID_PERMISSION_GATED_METHODS", "android"),
        ("ANDROID_PERMISSION_CLEANUP_METHODS", "android"),
    ):
        injected_mapping = mapping_source
        injected_helper = helper_source
        injected_android = android_services_source
        if owner == "mapping":
            injected_mapping = inject_first_entry_duplicate(mapping_source, table)
        elif owner == "helper":
            injected_helper = inject_first_entry_duplicate(helper_source, table)
        else:
            injected_android = inject_first_entry_duplicate(android_services_source, table)

        before = len(failures)
        read_all_tables(injected_mapping, injected_helper, injected_android)
        detected = failures[before:]
        del failures[before:]
        if not any(f"`{table}` contains duplicate name" in failure for failure in detected):
            failures.append(f"self-test: `{table}` production parser accepted a duplicate")

    for identifier, scope in sorted(android_gated.items()):
        deleted = mutate_android_method(
            android_services_source,
            identifier,
            lambda body: body.replace("permission_jni_call", "deleted_permission_jni_call", 1),
        )
        if not validate_android_services(deleted, android_gated, android_cleanup):
            failures.append(f"self-test: Android parser accepted wrapper deletion in `{identifier}`")

        replacement = "Record" if scope != "Record" else "Camera"
        wrong_scope = mutate_android_method(
            android_services_source,
            identifier,
            lambda body, old=f"Some(Scope::{scope})", new=f"Some(Scope::{replacement})":
                body.replace(old, new, 1),
        )
        if not validate_android_services(wrong_scope, android_gated, android_cleanup):
            failures.append(f"self-test: Android parser accepted scope mutation in `{identifier}`")

    for identifier in sorted(android_cleanup):
        deleted = mutate_android_method(
            android_services_source,
            identifier,
            lambda body: body.replace("permission_jni_call", "deleted_permission_jni_call", 1),
        )
        if not validate_android_services(deleted, android_gated, android_cleanup):
            failures.append(f"self-test: Android parser accepted wrapper deletion in `{identifier}`")

        wrongly_gated = mutate_android_method(
            android_services_source,
            identifier,
            lambda body: body.replace("None", "Some(Scope::Camera)", 1),
        )
        if not validate_android_services(wrongly_gated, android_gated, android_cleanup):
            failures.append(f"self-test: Android parser accepted None mutation in `{identifier}`")

    injected_method = """
    fn contract_new_sensitive_method(&self) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::WritePhotosAlbum), || Ok(()))
    }
"""
    injected_android = inject_android_method(
        android_services_source, "ImageApiService", injected_method
    )
    injected_identifier = "ImageApiService::contract_new_sensitive_method"
    unclassified_problems = validate_android_services(
        injected_android, android_gated, android_cleanup
    )
    if not any(
        f"Android wrapped `{injected_identifier}` is not classified" in problem
        for problem in unclassified_problems
    ):
        failures.append(
            "self-test: production parser accepted a new unclassified sensitive service method"
        )

    injected_policy = dict(android_gated)
    injected_policy[injected_identifier] = "WritePhotosAlbum"
    injected_derived, injected_derive_problems = derive_runtime_sensitive_methods(
        accessors, injected_policy, android_cleanup
    )
    if injected_derive_problems or injected_derived.get(
            "contract_new_sensitive_method") != "WritePhotosAlbum":
        failures.append(
            "self-test: runtime method inventory did not derive the new production wrapper"
        )
    if not validate_runtime_method_inventory(methods, injected_derived):
        failures.append(
            "self-test: shared method inventory accepted a missing production-derived method"
        )

    for permission in FULL_PERMISSION_POLICY:
        deleted = delete_manifest_permission(full_manifest_source, permission)
        if not validate_permission_manifests(deleted, slim_manifest_source):
            failures.append(f"self-test: manifest parser accepted deletion of `{permission}`")

    manifest_open = slim_manifest_source.find("<manifest")
    manifest_close = slim_manifest_source.find(">", manifest_open) + 1
    injected_slim = (
        slim_manifest_source[:manifest_close]
        + '\n    <uses-permission android:name="android.permission.CAMERA" />'
        + slim_manifest_source[manifest_close:]
    )
    if not validate_permission_manifests(full_manifest_source, injected_slim):
        failures.append("self-test: manifest parser accepted dangerous Slim permission")

    for needle, owner in (
        ("sPermissionOperations.register", "native"),
        ("sPermissionOperations.enter", "native"),
        ("pending.setCancellation", "provider"),
        ("request[0].cancel(fallback)", "provider"),
        ("lm.removeUpdates(listener)", "provider"),
        ("MAIN_HANDLER.removeCallbacks(timeout[0])", "provider"),
        ("removeListener = !listenerRemoved", "provider"),
        ("removeTimeout = !timeoutRemoved", "provider"),
        ("gate.enter", "provider"),
    ):
        mutated_native = native_exports_source
        mutated_provider = location_provider_source
        if owner == "native":
            mutated_native = mutated_native.replace(needle, "deletedBoundary", 1)
        else:
            mutated_provider = mutated_provider.replace(needle, "deletedBoundary", 1)
        if not validate_location_linearization(mutated_native, mutated_provider):
            failures.append(f"self-test: location parser accepted deletion of `{needle}`")
    if failures:
        print("FAIL: permission coverage contract self-test", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        sys.exit(1)
    print("PASS: permission coverage contract self-test")
    sys.exit(0)

failures.extend(validate_android_services(
    android_services_source, android_gated, android_cleanup
))
failures.extend(derived_method_problems)
failures.extend(validate_runtime_method_inventory(methods, derived_methods))
failures.extend(validate_permission_manifests(full_manifest_source, slim_manifest_source))
failures.extend(validate_location_linearization(
    native_exports_source, location_provider_source
))

# --- The helper has to exist and refuse by default ------------------------
if not re.search(r"\bfn\s+require_scope\s*\(", helper_source):
    failures.append(
        f"{helper_rs.relative_to(root)}: `require_scope` is gone; every call site "
        "below would be calling nothing"
    )
if "unwrap_or(false)" not in helper_source and "ScopeState::Granted" not in helper_source:
    failures.append(
        f"{helper_rs.relative_to(root)}: `require_scope` no longer decides on "
        "`ScopeState::Granted`; it may no longer deny by default"
    )

for duplicate in sorted(gated_ops.keys() & cleanup_ops.keys()):
    failures.append(f"`{duplicate}` is both permission-gated and a cleanup exemption")

# --- Every function using a service-wide capability must be classified ----
#
# Functions are split on top-level `fn` boundaries. Nested functions would blur
# the boundary, so a nested `fn` inside a guarded one is treated as part of it:
# that is the conservative direction, and the ops in question are flat.
FUNCTION = re.compile(r"^(?:pub(?:\([a-z()]+\))?\s+)?(?:async\s+)?fn\s+([a-z_0-9]+)", re.M)

checked_sites = 0
functions: dict[str, tuple[pathlib.Path, int, str]] = {}
helpers: dict[str, str] = {}
discovered_ops: dict[str, str] = {}
for source_path in sorted(runtime_src.rglob("*.rs")):
    if "/tests/" in str(source_path) or source_path.name == "permission.rs":
        continue
    text = strip_comments(source_path.read_text(encoding="utf-8", errors="replace"))

    starts = [(m.start(), m.group(1)) for m in FUNCTION.finditer(text)]
    if not starts:
        continue
    bounds = [
        (name, start, starts[i + 1][0] if i + 1 < len(starts) else len(text))
        for i, (start, name) in enumerate(starts)
    ]

    for name, start, end in bounds:
        body = text[start:end]
        line = text.count("\n", 0, start) + 1
        if name.startswith("op_"):
            if name in functions:
                previous_path, previous_line, _ = functions[name]
                failures.append(
                    f"duplicate runtime op `{name}` at "
                    f"{previous_path.relative_to(root)}:{previous_line} and "
                    f"{source_path.relative_to(root)}:{line}"
                )
            else:
                functions[name] = (source_path, line, body)
        for accessor, scope in accessors.items():
            if not re.search(rf"\.\s*{accessor}\s*\(\s*\)", body):
                continue
            if not name.startswith("op_"):
                helpers[name] = scope
                continue
            checked_sites += 1
            discovered_ops[name] = scope
            classified = gated_ops.get(name) or cleanup_ops.get(name)
            if classified != scope:
                rel = source_path.relative_to(root)
                failures.append(
                    f"{rel}:{line}: `{name}` reaches `services.{accessor}()` but is not "
                    f"classified for `Scope::{scope}`"
                )

for name, (source_path, line, body) in functions.items():
    for helper, scope in helpers.items():
        if not re.search(rf"\b{helper}\s*\(", body):
            continue
        checked_sites += 1
        discovered_ops[name] = scope
        classified = gated_ops.get(name) or cleanup_ops.get(name)
        if classified != scope:
            failures.append(
                f"{source_path.relative_to(root)}:{line}: `{name}` reaches `{helper}` but is "
                f"not classified for `Scope::{scope}`"
            )
    for method, scope in derived_methods.items():
        if not re.search(rf"\.\s*{method}\s*\(", body):
            continue
        checked_sites += 1
        discovered_ops[name] = scope
        classified = gated_ops.get(name) or cleanup_ops.get(name)
        if classified != scope:
            failures.append(
                f"{source_path.relative_to(root)}:{line}: `{name}` calls `{method}` but is "
                f"not classified for `Scope::{scope}`"
            )

for name, scope in sorted(gated_ops.items()):
    if name not in functions:
        failures.append(f"`{name}` is classified as gated but no runtime function exists")
        continue
    if discovered_ops.get(name) != scope:
        failures.append(
            f"`{name}` is classified for `Scope::{scope}` but no matching capability call was found"
        )
    source_path, line, body = functions[name]
    if not re.search(rf"require_scope\s*\([^)]*Scope::{scope}\s*\)", body):
        failures.append(
            f"{source_path.relative_to(root)}:{line}: gated `{name}` lacks "
            f"`require_scope(.., Scope::{scope})`"
        )

for name, scope in sorted(cleanup_ops.items()):
    if name not in functions:
        failures.append(f"`{name}` is classified as cleanup but no runtime function exists")
        continue
    if discovered_ops.get(name) != scope:
        failures.append(
            f"`{name}` is cleanup for `Scope::{scope}` but no matching capability call was found"
        )
    source_path, line, body = functions[name]
    if re.search(r"require_scope\s*\(", body):
        failures.append(
            f"{source_path.relative_to(root)}:{line}: cleanup `{name}` is still permission-gated"
        )

if checked_sites == 0:
    failures.append(
        "no call site of any gated accessor was found anywhere in the runtime. "
        "Either the accessors were renamed or this gate stopped matching them; "
        "either way it is now checking nothing."
    )

if failures:
    print("FAIL: permission coverage contract", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)

print(
    "PASS: permission coverage contract "
    f"({len(gated_ops)} gated op(s), {len(cleanup_ops)} cleanup op(s), "
    f"{len(discovered_ops)} permission-sensitive op(s))"
)
PY
