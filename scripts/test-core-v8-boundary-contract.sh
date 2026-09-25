#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" <<'PY'
from __future__ import annotations

import re
import pathlib
import sys
import tomllib


root = pathlib.Path(sys.argv[1]).resolve()
errors: list[str] = []


def require(condition: bool, message: str) -> None:
    if not condition:
        errors.append(message)


core_manifest_path = root / "engine/crates/core/Cargo.toml"
core_manifest = tomllib.loads(core_manifest_path.read_text(encoding="utf-8"))


def dependency_names(node: object) -> set[str]:
    names: set[str] = set()
    if not isinstance(node, dict):
        return names
    for key, value in node.items():
        if key in {"dependencies", "dev-dependencies", "build-dependencies"}:
            if isinstance(value, dict):
                for alias, specification in value.items():
                    names.add(alias)
                    if isinstance(specification, dict):
                        package = specification.get("package")
                        if isinstance(package, str):
                            names.add(package)
            continue
        names.update(dependency_names(value))
    return names


direct_dependencies = dependency_names(core_manifest)
aliased_runtime_fixture = {
    "dependencies": {
        "runtime_backend": {
            "package": "deno_core",
            "version": "0.385.0",
        }
    }
}
require(
    "deno_core" in dependency_names(aliased_runtime_fixture),
    "boundary checker must resolve Cargo dependency aliases to package identity",
)
for forbidden in ("deno_core", "deno_error"):
    require(
        forbidden not in direct_dependencies,
        f"core Cargo.toml must not directly depend on {forbidden}",
    )
require(
    "serde_json" in core_manifest.get("dependencies", {}),
    "core Cargo.toml must declare serde_json directly",
)

# What the code says, not what its comments say. The rule is that no core
# source *uses* the V8 backend's crates; the files that describe the boundary
# name them to say what the other side does -- "arguments converted by
# deno_core's rule for its parameter" is the sentence a reader of
# `service_args.rs` needs, and blanking it would cost more than it protects.
# Comments and string bodies are masked exactly as the op-surface reader masks
# them, so a `deno_core::` that is code still fails here.
sys.path.insert(0, str(root / "scripts/lib"))
import runtime_ops  # noqa: E402

core_root = root / "engine/crates/core"
for source_path in sorted(core_root.rglob("*.rs")):
    source = runtime_ops.mask_rust(source_path.read_text(encoding="utf-8"))
    relative = source_path.relative_to(root)
    require(
        "deno_core" not in source,
        f"core source must not name deno_core: {relative}",
    )
    require(
        "deno_error" not in source,
        f"core source must not name deno_error: {relative}",
    )
    # A path *starting* at `v8`, not the substring: the V8 backend crate is
    # named `runtime-v8`, so `runtime_v8::HostJsRuntime` contains "v8::" while
    # naming no V8 type at all. Matching the substring made this gate fail on a
    # crate rename and would have pushed someone to weaken it outright.
    require(
        re.search(r"(?<![A-Za-z0-9_])v8::", source) is None,
        f"core source must not name v8:: types: {relative}",
    )

for relative in (
    "engine/crates/core/src/runtime/loader.rs",
    "engine/crates/core/src/runtime/code_cache.rs",
    "engine/crates/core/src/runtime/isolate_pool.rs",
):
    require(
        not (root / relative).exists(),
        f"module must move out of core into the V8 backend: {relative}",
    )

for relative in (
    "engine/crates/runtime-v8/src/loader.rs",
    "engine/crates/runtime-v8/src/code_cache.rs",
    "engine/crates/runtime-v8/src/isolate_pool.rs",
):
    require(
        (root / relative).exists(),
        f"moved module must live in the V8 backend: {relative}",
    )

contract_step_name = "Core/V8 dependency boundary contract"
contract_command = "bash scripts/test-core-v8-boundary-contract.sh"


def has_active_workflow_step(workflow: str) -> bool:
    lines = workflow.splitlines()
    expected_name = f"- name: {contract_step_name}"
    expected_run = f"run: {contract_command}"

    for index, line in enumerate(lines):
        if line.strip() != expected_name:
            continue

        step_indent = len(line) - len(line.lstrip(" "))
        for candidate in lines[index + 1 :]:
            stripped = candidate.strip()
            if not stripped or stripped.startswith("#"):
                continue

            candidate_indent = len(candidate) - len(candidate.lstrip(" "))
            if candidate_indent <= step_indent and stripped.startswith("- "):
                break
            if candidate_indent == step_indent + 2 and stripped == expected_run:
                return True

    return False


active_workflow_fixture = f"""
steps:
  - name: {contract_step_name}
    run: {contract_command}
"""
require(
    has_active_workflow_step(active_workflow_fixture),
    "workflow checker must recognize an active Core/V8 contract step",
)

commented_workflow_fixture = f"""
steps:
  - name: {contract_step_name}
    # run: {contract_command}
"""
require(
    not has_active_workflow_step(commented_workflow_fixture),
    "workflow checker must reject a commented-out Core/V8 contract command",
)
for relative in (".github/workflows/pr-ci.yml", ".github/workflows/release.yml"):
    workflow = (root / relative).read_text(encoding="utf-8")
    require(
        has_active_workflow_step(workflow),
        f"{relative} must contain an active Core/V8 boundary contract step",
    )

if errors:
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    raise SystemExit(1)

print("Core/V8 dependency boundary contract: PASS")
PY
