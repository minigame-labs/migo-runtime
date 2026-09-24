#!/usr/bin/env python3
"""Generate a deterministic, artifact-bound CycloneDX SBOM."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import urllib.parse
from collections import deque
from typing import Any

from lib.supply_chain import PolicyError, load_policy, read_json, validate_metadata


def timestamp() -> str:
    raw = os.environ.get("SOURCE_DATE_EPOCH")
    if raw is None or raw == "":
        when = dt.datetime.now(dt.timezone.utc)
    else:
        if not raw.isdigit():
            raise PolicyError(f"SOURCE_DATE_EPOCH must be non-negative Unix seconds, got {raw}")
        when = dt.datetime.fromtimestamp(int(raw), dt.timezone.utc)
    return when.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def bom_ref(package: dict[str, Any]) -> str:
    name = urllib.parse.quote(str(package["name"]), safe="")
    version = urllib.parse.quote(str(package["version"]), safe="")
    return f"pkg:cargo/{name}@{version}"


def reachable_ids(metadata: dict[str, Any], root_name: str) -> tuple[str, set[str], dict[str, list[str]]]:
    packages = metadata.get("packages", [])
    roots = [item for item in packages if isinstance(item, dict) and item.get("name") == root_name]
    if len(roots) != 1:
        raise PolicyError(f"root package {root_name!r} matched {len(roots)} metadata packages")
    root_id = str(roots[0]["id"])
    resolve = metadata.get("resolve")
    nodes = resolve.get("nodes", []) if isinstance(resolve, dict) else []
    edges: dict[str, list[str]] = {}
    for node in nodes:
        if isinstance(node, dict):
            edges[str(node.get("id", ""))] = [str(item) for item in node.get("dependencies", [])]
    if root_id not in edges:
        raise PolicyError(f"cargo metadata resolve graph has no node for {root_name}")
    reached: set[str] = set()
    queue: deque[str] = deque([root_id])
    while queue:
        package_id = queue.popleft()
        if package_id in reached:
            continue
        reached.add(package_id)
        queue.extend(edges.get(package_id, []))
    return root_id, reached, edges


def source_revision(explicit: str | None, workspace_root: pathlib.Path) -> str:
    if explicit:
        return explicit
    try:
        return subprocess.check_output(
            ["git", "-C", str(workspace_root), "rev-parse", "HEAD"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        raise PolicyError("cannot determine source revision; pass --source-revision")


def merged_metadata(paths: list[pathlib.Path]) -> dict[str, Any]:
    """One graph out of several resolutions of the same workspace.

    An artifact that carries more than one build of the root -- the Apple SDK
    ships an iOS slice built without V8 and a macOS slice built with it -- has
    a component set that is the union of those builds' graphs. Package ids are
    cargo's own (name, version, source), so the same crate resolved twice is
    one record; its dependency edges are the union of what each build gave it.
    """
    if len(paths) == 1:
        return read_json(paths[0])
    packages: dict[str, dict[str, Any]] = {}
    edges: dict[str, set[str]] = {}
    members: set[str] = set()
    first: dict[str, Any] = {}
    for path in paths:
        metadata = read_json(path)
        first = first or metadata
        members.update(str(item) for item in metadata.get("workspace_members", []))
        for package in metadata.get("packages", []):
            if isinstance(package, dict):
                packages.setdefault(str(package.get("id", "")), package)
        resolve = metadata.get("resolve")
        for node in resolve.get("nodes", []) if isinstance(resolve, dict) else []:
            if isinstance(node, dict):
                edges.setdefault(str(node.get("id", "")), set()).update(
                    str(item) for item in node.get("dependencies", []))
    return {
        **{key: value for key, value in first.items() if key not in ("packages", "resolve")},
        "workspace_members": sorted(members),
        "packages": list(packages.values()),
        "resolve": {"nodes": [
            {"id": node, "dependencies": sorted(targets)} for node, targets in sorted(edges.items())
        ]},
    }


def compiled_subset(
    metadata: dict[str, Any], root_id: str, reached: set[str], trees: list[pathlib.Path]
) -> set[str]:
    """Keep only what the artifact's builds compile.

    `cargo metadata` resolves features for the whole workspace at once, so a
    crate one member enables is an edge for every member -- the iOS SDK, which
    is built without V8 precisely so it links no JavaScript engine, would list
    `v8` because the V8 runtime crate sits in the same workspace. `cargo tree`
    resolves the features of the one package being built. Each tree file is
    that command's `--prefix none --format {p}` output for one build; a package
    stays when some build compiles it, identified by name and version.
    """
    compiled: set[tuple[str, str]] = set()
    for tree in trees:
        for line in tree.read_text(encoding="utf-8").splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[1].startswith("v"):
                compiled.add((parts[0], parts[1][1:]))
    if not compiled:
        raise PolicyError("the compiled-package lists are empty; cargo tree resolved nothing")
    packages = {str(item.get("id", "")): item for item in metadata.get("packages", []) if isinstance(item, dict)}
    kept = {
        package_id
        for package_id in reached
        if package_id == root_id
        or (str(packages[package_id].get("name")), str(packages[package_id].get("version"))) in compiled
    }
    return kept


def build(args: argparse.Namespace) -> dict[str, Any]:
    metadata = merged_metadata(args.metadata)
    policy = load_policy(args.policy.resolve())
    root_id, reached, edges = reachable_ids(metadata, args.root_package)
    if args.compiled:
        reached = compiled_subset(metadata, root_id, reached, args.compiled)
    licenses, errors = validate_metadata(
        metadata, policy, args.workspace_root.resolve(), reached
    )
    if errors:
        raise PolicyError("; ".join(errors))

    packages = {
        str(item["id"]): item
        for item in metadata.get("packages", [])
        if isinstance(item, dict) and str(item.get("id", "")) in reached
    }
    if set(packages) != reached:
        missing = sorted(reached - set(packages))
        raise PolicyError(f"resolve graph references missing package records: {missing}")

    artifact = args.artifact.resolve()
    if not artifact.is_file():
        raise PolicyError(f"artifact is not a regular file: {artifact}")
    artifact_hash = hashlib.sha256(artifact.read_bytes()).hexdigest()
    revision = source_revision(args.source_revision, args.workspace_root.resolve())
    artifact_ref = f"urn:migo:artifact:{artifact_hash}"

    components: list[dict[str, Any]] = []
    refs: dict[str, str] = {root_id: artifact_ref}
    for package_id in sorted(reached - {root_id}, key=lambda item: (
        str(packages[item].get("name", "")),
        str(packages[item].get("version", "")),
        item,
    )):
        package = packages[package_id]
        ref = bom_ref(package)
        refs[package_id] = ref
        component: dict[str, Any] = {
            "type": "library",
            "bom-ref": ref,
            "name": str(package["name"]),
            "version": str(package["version"]),
            "purl": ref,
            "scope": "required",
            "licenses": [{"expression": licenses[package_id]}],
        }
        description = package.get("description")
        if isinstance(description, str) and description:
            component["description"] = description[:400]
        repository = package.get("repository")
        if isinstance(repository, str) and repository.startswith("https://"):
            component["externalReferences"] = [{"type": "vcs", "url": repository}]
        components.append(component)

    dependencies = []
    for package_id in sorted(reached, key=lambda item: refs[item]):
        dependencies.append(
            {
                "ref": refs[package_id],
                "dependsOn": sorted(
                    refs[dependency]
                    for dependency in edges.get(package_id, [])
                    if dependency in reached and dependency != root_id
                ),
            }
        )

    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "timestamp": timestamp(),
            "component": {
                "type": "file",
                "bom-ref": artifact_ref,
                "name": artifact.name,
                "version": revision,
                "hashes": [{"alg": "SHA-256", "content": artifact_hash}],
            },
            "properties": [
                {"name": "migo:artifact", "value": artifact.name},
                {"name": "migo:artifact-kind", "value": args.artifact_kind},
                {"name": "migo:components", "value": str(len(components))},
                {"name": "migo:profile", "value": args.profile},
                {"name": "migo:root-package", "value": args.root_package},
                {"name": "migo:source-revision", "value": revision},
                {"name": "migo:target", "value": args.target},
            ],
        },
        "components": components,
        "dependencies": dependencies,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    # Repeated for an artifact that carries several builds; see merged_metadata.
    result.add_argument("--metadata", type=pathlib.Path, required=True, action="append")
    # `cargo tree --prefix none --format {p}` of each build; see compiled_subset.
    result.add_argument("--compiled", type=pathlib.Path, action="append", default=[])
    result.add_argument("--artifact", type=pathlib.Path, required=True)
    result.add_argument("--artifact-kind", required=True)
    result.add_argument("--target", required=True)
    result.add_argument("--profile", required=True)
    result.add_argument("--root-package", required=True)
    result.add_argument("--policy", type=pathlib.Path, required=True)
    result.add_argument("--workspace-root", type=pathlib.Path, default=pathlib.Path.cwd())
    result.add_argument("--source-revision")
    result.add_argument("--out", type=pathlib.Path, required=True)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        sbom = build(args)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(sbom, indent=2) + "\n", encoding="utf-8")
    except (OSError, PolicyError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    print(f"SBOM -> {args.out} ({len(sbom['components'])} components)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
