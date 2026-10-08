#!/usr/bin/env python3
"""Which target builds a set of changed files needs before it can be called verified.

The rule is simple: a platform-conditional path is unverified until its own
target compiles. Host `cargo check`, `cargo test` and `cargo clippy` skip
`cfg(target_os = "android")` code entirely, so a green host run says nothing
about it. That is not hypothetical -- compile errors can otherwise ride a
branch for several sessions while every host run stays green.

Answering "which targets does this change need" from the changed paths alone is not
enough, for two reasons found in the tree rather than imagined:

  * OpenHarmony is `target_os = "linux"` with `target_env = "ohos"`
    (crates/platform/src/lib.rs), so a rule keyed on `target_os` reads all 17 ohos
    conditionals as host code.
  * A file selected by a conditional need not contain one.
    crates/capi/src/platform/windows.rs is plain Rust; the `cfg` that admits it sits
    on its parent's `mod` declaration. So conditions are inherited down the module
    tree, and this module walks it.

Polarity is deliberately ignored: a condition that *mentions* a non-host platform
needs that platform's build whether it selects for it or against it. Editing a
`cfg(not(windows))` block cannot break the Windows compile on its own, but removing
something the Windows branch referenced can, and only the Windows build sees that.

Known limits, since a limit that is not written down reads as coverage:

  * A file the module walk never reaches has unknown conditions. It is reported as
    undetermined rather than assumed portable -- the parser's failure mode has to be
    loud, or a `mod` form it does not recognise turns into a silent "needs nothing".
  * A *deleted* file is judged by its path alone; its conditions are gone with it.
  * Attribution is per file, not per hunk. Any edit to a file carrying an Android
    conditional asks for the Android build, even an edit elsewhere in the file.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import json
import pathlib
import re
import subprocess
import sys


# This module walks the tree on a Linux host. `unix` and `target_os = "linux"` are
# therefore satisfied by an ordinary host build and name no extra target.
HOST_TOKENS = frozenset({"unix", "linux"})

# `target_env` values that name a platform of their own. OpenHarmony shares Linux's
# `target_os`, so it is only distinguishable here; msvc only exists on Windows.
ENV_PLATFORMS = {"ohos": "ohos", "msvc": "windows"}

# Directories under `engine/` that hold crates. `crates` is the engine, `testing`
# holds what measures it; `tools` is excluded because it drives the engine rather
# than being part of any shipped target.
CRATE_GROUPS = ("crates", "testing")

# Crates whose Android build is their only compile gate: pr-ci.yml excludes them
# from its host `cargo test`/`clippy` lines, and each one carries an Android-only
# subtree. Listed by crate directory name under engine/crates.
ANDROID_GATED_CRATES = frozenset({"core", "graphics", "platform", "capi"})

# Crate directories that emit a cdylib. Compiling their dependencies proves nothing
# about the final link, which is where a missing symbol or an ABI mismatch appears,
# so these ask for the link tier instead.
CDYLIB_CRATES = {"android-jni": "android"}

COMPILE = "compile"
LINK = "link"
_TIER_ORDER = {COMPILE: 0, LINK: 1}

# The Android SDK's Java half, which is a shipped artifact and not a Rust target.
#
# Named as a platform of its own rather than a tier on `android`, because tiers
# replace each other -- the highest wins -- and a change touching both halves needs
# both builds, not the later one. Calling it a platform stretches the word; the
# field really means "lane that must be run", and the alternative was a second
# dimension for one case.
ANDROID_JAVA = "android-java"

# Everything under the Android platform directory asks for it, deliberately
# without trying to be clever about which files matter. Gradle's inputs are the
# sources, the manifests, the resources, the product-flavour configuration and the
# build scripts themselves, and a rule that enumerated them would be a list to
# forget an entry from. Over-running this lane costs a Gradle run against a warm
# cache; under-running it is the silent gap this module exists to close.
_ANDROID_PLATFORM_PATH = re.compile(r"^platforms/android/")

_CFG_OPENER = re.compile(r"\bcfg(?:_attr)?!?\s*\(")
_TARGET_OS = re.compile(r'target_os\s*=\s*"([A-Za-z0-9_]+)"')
_TARGET_ENV = re.compile(r'target_env\s*=\s*"([A-Za-z0-9_]+)"')
_TARGET_FAMILY = re.compile(r'target_family\s*=\s*"([A-Za-z0-9_]+)"')
# A bare `windows` / `unix` predicate. The `"` in the lookarounds is what keeps this
# from also matching the value inside `target_os = "windows"`.
_BARE_PREDICATE = re.compile(r'(?<![A-Za-z0-9_"])(windows|unix)(?![A-Za-z0-9_"])')

_MOD_DECLARATION = re.compile(
    r"^(?:pub\s*(?:\([^)]*\)\s*)?)?(?:unsafe\s+)?mod\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<terminator>[;{])"
)
_PATH_ATTRIBUTE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]')

_MODULE_ROOTS = ("lib.rs", "mod.rs", "main.rs")


@dataclasses.dataclass(frozen=True)
class Requirement:
    platform: str
    tier: str
    reasons: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class Plan:
    requirements: tuple[Requirement, ...]
    undetermined: tuple[str, ...]
    # The packages whose host suites this change needs, or `None` for "every suite",
    # which is what anything unreasonable-about resolves to. Never an empty tuple by
    # accident: an empty tuple means "no Rust package is implicated", which a
    # Java-only or documentation-only change legitimately is.
    host_packages: tuple[str, ...] | None = None
    host_reasons: tuple[str, ...] = ()


# Paths outside `engine/` that cannot change a host suite's outcome. Everything else
# outside `engine/` widens to every suite, because guessing is how a gap reopens:
# `scripts/dev-test-host.sh` decides how the suites run at all, and a workspace
# manifest or lock file can change what they compile.
_NO_HOST_EFFECT = (
    re.compile(r"^docs/"),
    re.compile(r"^platforms/"),
    re.compile(r"^\.github/"),
)


def _workspace_graph(root: pathlib.Path):
    """Workspace members and the edges between them, from `cargo metadata`.

    `--no-deps` keeps this to the workspace, and dev-dependencies are included on
    purpose: a crate whose *tests* use another crate is a crate whose suite a change
    to that other crate can break, which is exactly the closure being computed.

    Returns `None` when cargo cannot answer, so the caller can widen to every suite
    rather than compute a closure from half a graph.
    """
    manifest = root / "engine" / "Cargo.toml"
    if not manifest.is_file():
        return None
    try:
        completed = subprocess.run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
                "--manifest-path",
                str(manifest),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None
    if completed.returncode != 0:
        return None
    try:
        data = json.loads(completed.stdout)
    except json.JSONDecodeError:
        return None

    directories: dict[str, pathlib.Path] = {}
    dependencies: dict[str, set[str]] = {}
    for package in data.get("packages", ()):
        name = package.get("name")
        manifest_path = package.get("manifest_path")
        if not name or not manifest_path:
            return None
        directories[name] = pathlib.Path(manifest_path).parent
        dependencies[name] = {
            dependency.get("name")
            for dependency in package.get("dependencies", ())
            if dependency.get("name")
        }
    if not directories:
        return None
    members = set(directories)
    for name in dependencies:
        dependencies[name] &= members
    return directories, dependencies


def _owning_package(path: pathlib.Path, directories: dict[str, pathlib.Path]) -> str | None:
    """The workspace member a file belongs to, by longest matching manifest directory."""
    best: tuple[int, str] | None = None
    for name, directory in directories.items():
        try:
            path.relative_to(directory)
        except ValueError:
            continue
        depth = len(directory.parts)
        if best is None or depth > best[0]:
            best = (depth, name)
    return None if best is None else best[1]


def host_suites(root: pathlib.Path, changed) -> tuple[tuple[str, ...] | None, tuple[str, ...]]:
    """The packages whose host suites a change needs, and why.

    `verify-change.sh` ran every host suite on every invocation, which made a
    Java-only change pay for sixteen of them. The selection here is the changed
    packages plus their reverse-dependency closure: a change to a leaf crate still
    runs the suites of everything that depends on it, because that is where its
    behaviour is observed.

    It fails closed in every direction. `None` -- every suite -- is the answer for a
    tree cargo cannot describe, for a file under `engine/` that belongs to no member,
    and for any path outside `engine/` that is not provably irrelevant. Under-running
    is a silent gap, and a silent gap is worse than a slow run.
    """
    root = pathlib.Path(root)
    graph = _workspace_graph(root)
    if graph is None:
        return None, ("cargo metadata could not describe the workspace",)
    directories, dependencies = graph

    dependents: dict[str, set[str]] = collections.defaultdict(set)
    for name, deps in dependencies.items():
        for dependency in deps:
            dependents[dependency].add(name)

    seeds: dict[str, str] = {}
    reasons: list[str] = []
    for entry in changed:
        path = pathlib.Path(entry)
        if not entry.startswith("engine/"):
            if any(pattern.match(entry) for pattern in _NO_HOST_EFFECT):
                continue
            return None, (f"{entry} is outside engine/ and could change any suite",)
        owner = _owning_package((root / path).resolve(), directories)
        if owner is None:
            return None, (f"{entry} belongs to no workspace member",)
        seeds.setdefault(owner, entry)

    if not seeds:
        return (), ("no workspace member changed",)

    required: set[str] = set()
    queue = collections.deque(seeds)
    while queue:
        name = queue.popleft()
        if name in required:
            continue
        required.add(name)
        queue.extend(dependents.get(name, ()))

    for name in sorted(seeds):
        reasons.append(f"{seeds[name]} [{name}]")
    inherited = sorted(required - set(seeds))
    if inherited:
        reasons.append("dependents: " + ", ".join(inherited))
    return tuple(sorted(required)), tuple(reasons)


def platforms_in(condition: str) -> frozenset[str]:
    """The non-host platforms a single `cfg(...)` condition mentions."""
    found = set()
    for value in _TARGET_OS.findall(condition):
        if value not in HOST_TOKENS:
            found.add(value)
    for value in _TARGET_ENV.findall(condition):
        platform = ENV_PLATFORMS.get(value)
        if platform is not None:
            found.add(platform)
    for value in _TARGET_FAMILY.findall(condition):
        if value not in HOST_TOKENS:
            found.add(value)
    for value in _BARE_PREDICATE.findall(condition):
        if value not in HOST_TOKENS:
            found.add(value)
    return frozenset(found)


def _conditions_in(text: str) -> list[str]:
    """Every `cfg(...)` / `cfg_attr(...)` body in a source text, by bracket matching.

    A regex cannot do this: the bodies nest (`any(all(...), not(...))`).
    """
    bodies = []
    for opener in _CFG_OPENER.finditer(text):
        depth = 1
        index = opener.end()
        while index < len(text) and depth:
            character = text[index]
            if character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
            index += 1
        bodies.append(text[opener.end() : index - 1])
    return bodies


def platforms_in_source(text: str) -> frozenset[str]:
    """The non-host platforms a whole source file mentions in its conditions."""
    found: set[str] = set()
    for condition in _conditions_in(text):
        found |= platforms_in(condition)
    return frozenset(found)


def _iter_mod_declarations(text: str):
    """Yield `(attributes, module_name, is_inline)` for each `mod` item in a file.

    Line-oriented rather than one regex because an attribute can span lines
    (`#[cfg(any(\\n  target_os = "android",\\n  test\\n))]`) and because doc comments
    sit between an attribute and the item it decorates, where `\\s*` would not reach.

    The scan is flat, so a `mod` declared inside an inline module body resolves to
    the wrong directory. That does not produce a wrong answer: the real file then
    goes unreached and is reported, which is why unreachability is a first-class
    result rather than a warning.
    """
    pending: list[str] = []
    partial: str | None = None
    depth = 0

    for line in text.splitlines():
        if partial is not None:
            partial += "\n" + line
            depth += line.count("[") - line.count("]")
            if depth <= 0:
                pending.append(partial)
                partial = None
                depth = 0
            continue

        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue

        if stripped.startswith("#["):
            depth = stripped.count("[") - stripped.count("]")
            if depth <= 0:
                pending.append(stripped)
            else:
                partial = stripped
            continue

        declaration = _MOD_DECLARATION.match(stripped)
        if declaration is not None:
            yield (
                "\n".join(pending),
                declaration.group("name"),
                declaration.group("terminator") == "{",
            )
        pending = []


def _child_directory(module_file: pathlib.Path) -> pathlib.Path:
    """Where a module's children live, given the file that declares them."""
    if module_file.name in _MODULE_ROOTS:
        return module_file.parent
    return module_file.parent / module_file.stem


def _child_file(directory: pathlib.Path, name: str) -> pathlib.Path | None:
    for candidate in (directory / f"{name}.rs", directory / name / "mod.rs"):
        if candidate.is_file():
            return candidate
    return None


def resolve_crate(crate_directory: pathlib.Path):
    """Map every source file of one crate to the platforms its conditions mention.

    Returns `(conditions, unreachable)` keyed by repository-relative POSIX paths.
    `unreachable` holds the crate's `src/**.rs` files no `mod` declaration reached.
    """
    crate_directory = pathlib.Path(crate_directory).resolve()
    # engine/crates/<name> -> repository root.
    root = crate_directory.parents[2]
    source_root = crate_directory / "src"
    entry = source_root / "lib.rs"

    def relative(path: pathlib.Path) -> str:
        return path.relative_to(root).as_posix()

    inherited: dict[pathlib.Path, set[str]] = collections.defaultdict(set)
    if not entry.is_file():
        return {}, {
            relative(path) for path in sorted(source_root.rglob("*.rs"))
        }

    queue = [(entry, frozenset())]
    inherited[entry] = set()
    while queue:
        module_file, conditions = queue.pop()
        text = module_file.read_text(encoding="utf-8", errors="replace")
        directory = _child_directory(module_file)
        for attributes, name, is_inline in _iter_mod_declarations(text):
            if is_inline:
                continue
            platforms = set(conditions)
            for condition in _conditions_in(attributes):
                platforms |= platforms_in(condition)
            override = _PATH_ATTRIBUTE.search(attributes)
            if override is not None:
                # `#[path]` on a non-inline module is relative to the directory the
                # declaring *file* sits in, not to the module's own child directory
                # (Rust reference, "The path attribute"). The two differ exactly for
                # a non-`mod.rs` parent: `#[path = "damage_tracker.rs"]` in
                # graphics/src/dirty_region.rs names src/damage_tracker.rs, and
                # resolving it as src/dirty_region/damage_tracker.rs loses the file.
                child = module_file.parent / override.group(1)
                if not child.is_file():
                    continue
            else:
                child = _child_file(directory, name)
                if child is None:
                    continue
            child = child.resolve()
            known = inherited.get(child)
            if known is not None and platforms <= known:
                continue
            inherited[child].update(platforms)
            queue.append((child, frozenset(inherited[child])))

    conditions_by_file: dict[str, frozenset[str]] = {}
    for path, platforms in inherited.items():
        text = path.read_text(encoding="utf-8", errors="replace")
        conditions_by_file[relative(path)] = frozenset(
            platforms | platforms_in_source(text)
        )

    reached = set(conditions_by_file)
    unreachable = {
        relative(path)
        for path in source_root.rglob("*.rs")
        if relative(path) not in reached
    }
    return conditions_by_file, unreachable


_CRATE_PATH = re.compile(r"^engine/crates/(?P<crate>[A-Za-z0-9_-]+)/")


def select(root: pathlib.Path, changed) -> Plan:
    """The target builds a changed file set needs, with the reason for each."""
    root = pathlib.Path(root)
    reasons: dict[tuple[str, str], set[str]] = collections.defaultdict(set)
    undetermined: set[str] = set()
    resolved: dict[str, tuple[dict[str, frozenset[str]], set[str]]] = {}

    for path in changed:
        if _ANDROID_PLATFORM_PATH.match(path):
            reasons[(ANDROID_JAVA, COMPILE)].add(f"{path} [gradle]")

        match = _CRATE_PATH.match(path)
        if match is None:
            continue
        crate = match.group("crate")

        cdylib_platform = CDYLIB_CRATES.get(crate)
        if cdylib_platform is not None:
            reasons[(cdylib_platform, LINK)].add(f"{path} [cdylib]")
        elif crate in ANDROID_GATED_CRATES:
            reasons[("android", COMPILE)].add(f"{path} [crate]")

        if not path.endswith(".rs") or f"engine/crates/{crate}/src/" not in path:
            continue
        # A deleted file is judged by its path alone -- its conditions went with it.
        if not (root / path).is_file():
            continue
        if crate not in resolved:
            resolved[crate] = resolve_crate(root / "engine" / "crates" / crate)
        conditions, unreachable = resolved[crate]
        if path in unreachable or path not in conditions:
            undetermined.add(path)
            continue
        for platform in conditions[path]:
            reasons[(platform, COMPILE)].add(f"{path} [cfg]")

    tier_by_platform: dict[str, str] = {}
    for platform, tier in reasons:
        current = tier_by_platform.get(platform)
        if current is None or _TIER_ORDER[tier] > _TIER_ORDER[current]:
            tier_by_platform[platform] = tier

    requirements = []
    for platform, tier in sorted(tier_by_platform.items()):
        collected: set[str] = set()
        for (candidate, _), entries in reasons.items():
            if candidate == platform:
                collected |= entries
        requirements.append(Requirement(platform, tier, tuple(sorted(collected))))

    host_packages, host_reasons = host_suites(root, changed)
    return Plan(
        tuple(requirements),
        tuple(sorted(undetermined)),
        host_packages,
        host_reasons,
    )


def format_report(plan: Plan) -> str:
    """The plan as the shell entry point reads it: one header line per requirement."""
    lines = []
    for requirement in plan.requirements:
        lines.append(f"TARGET {requirement.platform} {requirement.tier}")
        lines.extend(f"  {reason}" for reason in requirement.reasons)
    # `HOSTSUITES ALL` and `HOSTSUITES NONE` are spelled out rather than left implicit,
    # because the shell has to tell "run everything" apart from "run nothing" and an
    # empty list cannot say which it meant.
    if plan.host_packages is None:
        lines.append("HOSTSUITES ALL")
    elif not plan.host_packages:
        lines.append("HOSTSUITES NONE")
    else:
        lines.append("HOSTSUITES " + " ".join(plan.host_packages))
    lines.extend(f"  {reason}" for reason in plan.host_reasons)
    if plan.undetermined:
        lines.append("UNDETERMINED")
        lines.extend(f"  {path}" for path in plan.undetermined)
    return "\n".join(lines)


def audit(root: pathlib.Path) -> tuple[str, ...]:
    """Every crate source file no `mod` declaration reaches, across the whole tree.

    The module walk's completeness is not observable from a single change, so it is
    checked here instead: a `mod` form the parser misses shows up as an unreached
    file long before someone edits it.

    "The whole tree" means every group the workspace keeps crates in, not just
    `engine/crates`. `engine/testing` holds the crates that measure the engine, and
    a group left out of this walk is a group where a missed `mod` form stays
    invisible.
    """
    root = pathlib.Path(root)
    unreached: set[str] = set()
    for group in CRATE_GROUPS:
        directory = root / "engine" / group
        if not directory.is_dir():
            continue
        for crate in sorted(directory.iterdir()):
            if not (crate / "Cargo.toml").is_file():
                continue
            _, unreachable = resolve_crate(crate)
            unreached |= unreachable
    return tuple(sorted(unreached))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument(
        "--audit",
        action="store_true",
        help="report crate source files no mod declaration reaches, then exit",
    )
    parser.add_argument(
        "changed",
        nargs="*",
        help="repository-relative changed paths; read from stdin when absent",
    )
    arguments = parser.parse_args(argv)
    root = pathlib.Path(arguments.root)

    if arguments.audit:
        unreached = audit(root)
        for path in unreached:
            print(path)
        return 1 if unreached else 0

    changed = arguments.changed
    if not changed:
        changed = [line.strip() for line in sys.stdin if line.strip()]
    report = format_report(select(root, changed))
    if report:
        print(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
