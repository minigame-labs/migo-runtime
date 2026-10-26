"""Fail-closed supply-chain policy shared by CI and SBOM generation."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import pathlib
import re
import tomllib
import xml.etree.ElementTree as ET
from collections.abc import Iterable
from typing import Any


class PolicyError(RuntimeError):
    """A release input violates the repository supply-chain policy."""


_SPDX_TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]*")
_SPDX_OPERATORS = {"AND", "OR", "WITH"}
_SHA = re.compile(r"[0-9a-f]{40}")
_EXACT_VERSION = re.compile(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?")
_REQUIREMENT_HASH = re.compile(r"--hash=sha256:[0-9a-f]{64}(?:\s|$)")
_CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"


def read_json(path: pathlib.Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise PolicyError(f"cannot read JSON {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise PolicyError(f"{path} must contain a JSON object")
    return value


def load_policy(path: pathlib.Path) -> dict[str, Any]:
    try:
        policy = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise PolicyError(f"cannot read policy {path}: {exc}") from exc
    if policy.get("schema") != 1:
        raise PolicyError(f"{path}: schema must be 1")

    licenses = policy.get("licenses")
    if not isinstance(licenses, dict):
        raise PolicyError(f"{path}: [licenses] table is required")
    allowed = licenses.get("allowed")
    if not isinstance(allowed, list) or not allowed or not all(
        isinstance(item, str) and item for item in allowed
    ):
        raise PolicyError(f"{path}: licenses.allowed must be a non-empty string array")
    if allowed != sorted(set(allowed), key=lambda item: item.encode("utf-8")):
        raise PolicyError(f"{path}: licenses.allowed must be sorted and unique")

    _validate_exception_records(policy, "advisory_exceptions")
    _validate_exception_records(policy, "license_file_exceptions")
    return policy


def _validate_exception_records(policy: dict[str, Any], key: str) -> None:
    records = policy.get(key, [])
    if not isinstance(records, list) or not all(isinstance(item, dict) for item in records):
        raise PolicyError(f"{key} must be an array of tables")
    seen: set[tuple[str, ...]] = set()
    identity_fields = (
        ("id", "package", "version", "kind")
        if key == "advisory_exceptions"
        else ("package", "version")
    )
    for record in records:
        identity = tuple(str(record.get(field, "")) for field in identity_fields)
        if not all(identity):
            raise PolicyError(f"{key} record lacks one of {identity_fields}: {record}")
        if identity in seen:
            raise PolicyError(f"duplicate {key} record: {identity}")
        seen.add(identity)
        reason = record.get("reason")
        tracking = record.get("tracking")
        if not isinstance(reason, str) or len(reason.strip()) < 20:
            raise PolicyError(f"{key} {identity}: reason must be at least 20 characters")
        if not isinstance(tracking, str) or not tracking.startswith("https://"):
            raise PolicyError(f"{key} {identity}: tracking must be an https URL")
        if key == "advisory_exceptions":
            _parse_date(record.get("expires"), f"{key} {identity} expires")
        else:
            expression = record.get("expression")
            digest = record.get("sha256")
            if not isinstance(expression, str) or not expression:
                raise PolicyError(f"{key} {identity}: expression is required")
            if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise PolicyError(f"{key} {identity}: sha256 must be 64 lower-case hex digits")


def _parse_date(value: object, label: str) -> dt.date:
    if isinstance(value, dt.date):
        return value
    if not isinstance(value, str):
        raise PolicyError(f"{label} must be YYYY-MM-DD")
    try:
        return dt.date.fromisoformat(value)
    except ValueError as exc:
        raise PolicyError(f"{label} must be YYYY-MM-DD") from exc


def spdx_ids(expression: str) -> set[str]:
    return {
        token
        for token in _SPDX_TOKEN.findall(expression)
        if token.upper() not in _SPDX_OPERATORS
    }


def validate_actions(
    workflows_dir: pathlib.Path, exclusions: Iterable[str] = ()
) -> tuple[int, list[str]]:
    excluded = set(exclusions)
    errors: list[str] = []
    remote_count = 0
    files = sorted((*workflows_dir.rglob("*.yml"), *workflows_dir.rglob("*.yaml")))
    if not files:
        return 0, [f"no workflow YAML found under {workflows_dir}"]

    use_line = re.compile(
        r"^\s*(?:-\s*)?(?:uses|['\"]uses['\"])\s*:\s*([^\s#]+)"
    )
    for path in files:
        relative = path.relative_to(workflows_dir).as_posix()
        if relative in excluded or path.name in excluded:
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            errors.append(f"{relative}: cannot read: {exc}")
            continue
        for number, line in enumerate(lines, start=1):
            match = use_line.match(line)
            if not match:
                continue
            target = match.group(1).strip('"\'')
            if target.startswith("./"):
                continue
            remote_count += 1
            if "@" not in target:
                errors.append(f"{relative}:{number}: remote action has no revision: {target}")
                continue
            action, revision = target.rsplit("@", 1)
            if action.count("/") < 1 or not _SHA.fullmatch(revision):
                errors.append(
                    f"{relative}:{number}: remote action must use a full 40-hex commit, "
                    f"not {target}"
                )
        errors.extend(
            _validate_workflow_installers(
                lines, relative=relative, repository_root=workflows_dir.parent
            )
        )
    if remote_count == 0:
        errors.append("no remote workflow actions were scanned; the gate would pass vacuously")
    return remote_count, errors


def _run_blocks(lines: list[str]) -> Iterable[tuple[int, str]]:
    """Yield the shell body of each YAML ``run`` scalar without a YAML parser."""

    run_line = re.compile(
        r"^(\s*)(?:-\s*)?(?:run|['\"]run['\"])\s*:\s*(.*)$"
    )
    index = 0
    while index < len(lines):
        match = run_line.match(lines[index])
        if match is None:
            index += 1
            continue
        start = index + 1
        indent = len(match.group(1))
        value = match.group(2).strip()
        if value not in {"|", "|-", "|+", ">", ">-", ">+"}:
            yield start, value
            index += 1
            continue

        body: list[str] = []
        index += 1
        while index < len(lines):
            line = lines[index]
            if line.strip() and len(line) - len(line.lstrip()) <= indent:
                break
            if not line.lstrip().startswith("#"):
                body.append(line.strip())
            index += 1
        yield start, "\n".join(body)


def _validate_requirement_file(path: pathlib.Path, label: str) -> list[str]:
    try:
        raw_lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        return [f"{label}: cannot read hash-pinned requirements {path}: {exc}"]

    logical: list[str] = []
    pending = ""
    for raw in raw_lines:
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        pending = f"{pending} {line}".strip()
        if pending.endswith("\\"):
            pending = pending[:-1].rstrip()
            continue
        logical.append(pending)
        pending = ""
    if pending:
        logical.append(pending)
    if not logical:
        return [f"{label}: requirements file {path} has no packages"]

    errors: list[str] = []
    for requirement in logical:
        package = requirement.split(None, 1)[0]
        if "==" not in package:
            errors.append(
                f"{label}: requirement must use an exact == version: {requirement}"
            )
        else:
            _, version = package.rsplit("==", 1)
            if _EXACT_VERSION.fullmatch(version) is None:
                errors.append(
                    f"{label}: requirement has a non-exact version: {requirement}"
                )
        if _REQUIREMENT_HASH.search(f"{requirement} ") is None:
            errors.append(
                f"{label}: requirement lacks an exact SHA-256: {requirement}"
            )
    return errors


def _validate_workflow_installers(
    lines: list[str], *, relative: str, repository_root: pathlib.Path
) -> list[str]:
    errors: list[str] = []
    for number, body in _run_blocks(lines):
        cargo_installs = re.findall(r"\bcargo\s+install\b[^\n;&|]*", body)
        for command in cargo_installs:
            version = re.search(r"--version(?:=|\s+)([^\s]+)", command)
            if version is None or _EXACT_VERSION.fullmatch(version.group(1)) is None:
                errors.append(
                    f"{relative}:{number}: cargo install must pin an exact --version"
                )
            if re.search(r"(?:^|\s)--locked(?:\s|$)", command) is None:
                errors.append(f"{relative}:{number}: cargo install must use --locked")

        if re.search(r"\b(?:python3?\s+-m\s+)?pip3?\s+install\b", body) is None:
            continue
        required_flags = ("--require-hashes", "--no-deps", "--only-binary=:all:")
        for flag in required_flags:
            if flag not in body:
                errors.append(
                    f"{relative}:{number}: pip install must include {flag}"
                )
        requirement = re.search(
            r"(?:--requirement|-r)(?:=|\s+)([^\s\\]+)", body
        )
        if requirement is None:
            errors.append(
                f"{relative}:{number}: pip install must use a committed requirements file"
            )
            continue
        raw_path = requirement.group(1).strip("'\"")
        requirement_path = pathlib.Path(raw_path)
        if requirement_path.is_absolute() or ".." in requirement_path.parts:
            errors.append(
                f"{relative}:{number}: requirements path must stay inside the repository"
            )
            continue
        errors.extend(
            _validate_requirement_file(
                repository_root / requirement_path, f"{relative}:{number}"
            )
        )
    return errors


def _package_key(package: dict[str, Any]) -> tuple[str, str]:
    return str(package.get("name", "")), str(package.get("version", ""))


def _resolved_license_file(package: dict[str, Any]) -> pathlib.Path | None:
    raw = package.get("license_file")
    manifest = package.get("manifest_path")
    if not isinstance(raw, str) or not raw or not isinstance(manifest, str) or not manifest:
        return None
    path = pathlib.Path(raw)
    if not path.is_absolute():
        path = pathlib.Path(manifest).parent / path
    return path.resolve()


def validate_metadata(
    metadata: dict[str, Any],
    policy: dict[str, Any],
    workspace_root: pathlib.Path,
    package_ids: set[str] | None = None,
) -> tuple[dict[str, str], list[str]]:
    packages = metadata.get("packages")
    if not isinstance(packages, list) or not packages:
        raise PolicyError("cargo metadata contains no packages")
    allowed = set(policy["licenses"]["allowed"])
    file_exceptions = {
        (str(item["package"]), str(item["version"])): item
        for item in policy.get("license_file_exceptions", [])
    }
    used_file_exceptions: set[tuple[str, str]] = set()
    license_by_id: dict[str, str] = {}
    errors: list[str] = []
    root = workspace_root.resolve()

    for package in packages:
        if not isinstance(package, dict):
            errors.append("cargo metadata package is not an object")
            continue
        package_id = str(package.get("id", ""))
        if package_ids is not None and package_id not in package_ids:
            continue
        name, version = _package_key(package)
        label = f"{name}@{version}"
        source = package.get("source")
        if isinstance(source, str) and source.startswith("git+"):
            errors.append(f"{label}: git dependency is forbidden ({source})")
        elif isinstance(source, str) and source != _CRATES_IO_SOURCE:
            errors.append(f"{label}: unapproved Cargo registry/source ({source})")
        elif source is None:
            manifest = package.get("manifest_path")
            if not isinstance(manifest, str):
                errors.append(f"{label}: path package has no manifest_path")
            else:
                try:
                    pathlib.Path(manifest).resolve().relative_to(root)
                except ValueError:
                    errors.append(f"{label}: path dependency escapes workspace root")
        elif not isinstance(source, str):
            errors.append(f"{label}: dependency source is not a string or null")

        expression = package.get("license")
        if isinstance(expression, str) and expression.strip():
            expression = expression.strip()
        else:
            exception = file_exceptions.get((name, version))
            license_path = _resolved_license_file(package)
            if exception is None or license_path is None:
                errors.append(f"{label}: no SPDX license and no exact license-file approval")
                continue
            try:
                digest = hashlib.sha256(license_path.read_bytes()).hexdigest()
            except OSError as exc:
                errors.append(f"{label}: cannot read approved license file: {exc}")
                continue
            if digest != exception["sha256"]:
                errors.append(
                    f"{label}: license file hash {digest} differs from approved "
                    f"{exception['sha256']}"
                )
                continue
            expression = str(exception["expression"])
            used_file_exceptions.add((name, version))

        unknown = sorted(spdx_ids(expression) - allowed)
        if unknown:
            errors.append(f"{label}: unapproved SPDX identifiers: {', '.join(unknown)}")
            continue
        license_by_id[package_id] = expression

    considered_keys = {
        _package_key(package)
        for package in packages
        if isinstance(package, dict)
        and (package_ids is None or str(package.get("id", "")) in package_ids)
    }
    # A complete graph can prove an exact approval is obsolete even when the
    # package (or just that version) has disappeared. Intersecting with the
    # graph first silently kept those records forever. A product-only scan
    # cannot make that claim about approvals used by other products.
    approval_scope = (
        set(file_exceptions)
        if package_ids is None
        else set(file_exceptions) & considered_keys
    )
    for key in sorted(approval_scope - used_file_exceptions):
        errors.append(f"{key[0]}@{key[1]}: license-file approval is stale or was not exercised")
    return license_by_id, errors


def validate_audit(
    audit: dict[str, Any], policy: dict[str, Any], as_of: dt.date
) -> tuple[int, list[str]]:
    errors: list[str] = []
    vulnerabilities = audit.get("vulnerabilities", {})
    vuln_list = vulnerabilities.get("list", []) if isinstance(vulnerabilities, dict) else []
    vuln_count = vulnerabilities.get("count", len(vuln_list)) if isinstance(vulnerabilities, dict) else 0
    if vuln_count or vuln_list:
        for item in vuln_list or [{}]:
            # `.get(key, {})` only substitutes the default when the key is
            # *absent* -- cargo-audit's "yanked" warning kind carries an
            # explicit `"advisory": null` (a yanked release has no RUSTSEC id
            # to report), which `.get` passes through as None and the `.get`
            # calls below then crash on. `or {}` catches both "absent" and
            # "present but null".
            advisory = (item.get("advisory") or {}) if isinstance(item, dict) else {}
            package = (item.get("package") or {}) if isinstance(item, dict) else {}
            errors.append(
                "vulnerability "
                f"{advisory.get('id', 'unknown')} affects "
                f"{package.get('name', 'unknown')}@{package.get('version', 'unknown')}"
            )

    exceptions = {
        (
            str(item["id"]),
            str(item["package"]),
            str(item["version"]),
            str(item["kind"]),
        ): item
        for item in policy.get("advisory_exceptions", [])
    }
    used: set[tuple[str, str, str, str]] = set()
    warning_count = 0
    warnings = audit.get("warnings", {})
    if not isinstance(warnings, dict):
        errors.append("cargo-audit warnings is not an object")
        warnings = {}
    for group, entries in warnings.items():
        if not isinstance(entries, list):
            errors.append(f"cargo-audit warning group {group} is not an array")
            continue
        for item in entries:
            warning_count += 1
            if not isinstance(item, dict):
                errors.append(f"cargo-audit warning group {group} contains a non-object")
                continue
            kind = str(item.get("kind") or group)
            # Same null-vs-absent trap as the vulnerability loop above: a
            # "yanked" warning's "advisory" key is present and explicitly
            # null, not absent, so `.get(key, {})` would pass None through.
            package = item.get("package") or {}
            advisory = item.get("advisory") or {}
            key = (
                str(advisory.get("id", "")),
                str(package.get("name", "")),
                str(package.get("version", "")),
                kind,
            )
            if kind == "unsound":
                errors.append(f"unsound advisory {key[0]} affects {key[1]}@{key[2]}")
                continue
            exception = exceptions.get(key)
            if exception is None:
                errors.append(f"unapproved {kind} advisory {key[0]} affects {key[1]}@{key[2]}")
                continue
            expires = _parse_date(exception["expires"], f"advisory exception {key} expires")
            if as_of > expires:
                errors.append(
                    f"expired {kind} exception {key[0]} for {key[1]}@{key[2]} "
                    f"(expired {expires.isoformat()})"
                )
                continue
            used.add(key)

    for key in sorted(set(exceptions) - used):
        errors.append(
            f"advisory exception {key[0]} for {key[1]}@{key[2]} ({key[3]}) is stale "
            "or was not exercised"
        )
    return warning_count, errors


def _gradle_properties(path: pathlib.Path) -> dict[str, str]:
    result: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise PolicyError(f"cannot read {path}: {exc}") from exc
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith(("#", "!")) or "=" not in line:
            continue
        key, value = line.split("=", 1)
        result[key.strip()] = value.strip().replace("\\:", ":")
    return result


def validate_gradle(
    project_dir: pathlib.Path, policy: dict[str, Any]
) -> tuple[int, int, list[str]]:
    errors: list[str] = []
    root = project_dir.resolve()
    gradle_policy = policy.get("gradle")
    if not isinstance(gradle_policy, dict):
        return 0, 0, ["policy has no [gradle] table"]
    for field in ("distribution_url", "distribution_sha256", "wrapper_jar_sha256"):
        if not isinstance(gradle_policy.get(field), str) or not gradle_policy[field]:
            errors.append(f"policy gradle.{field} is required")
    for field in ("distribution_sha256", "wrapper_jar_sha256"):
        value = gradle_policy.get(field)
        if isinstance(value, str) and not re.fullmatch(r"[0-9a-f]{64}", value):
            errors.append(f"policy gradle.{field} must be 64 lower-case hex digits")

    wrapper_dir = root / "gradle" / "wrapper"
    properties = _gradle_properties(wrapper_dir / "gradle-wrapper.properties")
    actual_url = properties.get("distributionUrl", "")
    actual_distribution_hash = properties.get("distributionSha256Sum", "")
    if actual_url != gradle_policy.get("distribution_url"):
        errors.append(
            f"Gradle distributionUrl {actual_url!r} differs from policy "
            f"{gradle_policy.get('distribution_url')!r}"
        )
    if not actual_url.startswith("https://"):
        errors.append("Gradle distributionUrl must use https")
    if actual_distribution_hash != gradle_policy.get("distribution_sha256"):
        errors.append("Gradle distributionSha256Sum differs from policy")
    wrapper_jar = wrapper_dir / "gradle-wrapper.jar"
    try:
        wrapper_hash = hashlib.sha256(wrapper_jar.read_bytes()).hexdigest()
    except OSError as exc:
        errors.append(f"cannot read Gradle wrapper JAR: {exc}")
    else:
        if wrapper_hash != gradle_policy.get("wrapper_jar_sha256"):
            errors.append(f"Gradle wrapper JAR hash {wrapper_hash} differs from policy")

    scripts = sorted(
        path
        for path in (*root.rglob("*.gradle"), *root.rglob("*.gradle.kts"))
        if path.is_file()
        and ".gradle" not in path.relative_to(root).parts
        and "build" not in path.relative_to(root).parts
    )
    if not scripts:
        errors.append("no Gradle scripts found")
    combined = "\n".join(path.read_text(encoding="utf-8", errors="replace") for path in scripts)
    for required in ("dependencyLocking", "lockAllConfigurations()", "LockMode.STRICT"):
        if required not in combined:
            errors.append(f"strict dependency locking is missing {required}")
    dynamic = re.compile(
        r"['\"][^'\"\n]+:[^'\"\n]+:(?:latest[^'\"\n]*|[^'\"\n]*[+*])['\"]",
        re.IGNORECASE,
    )
    for path in scripts:
        for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            code = line.split("//", 1)[0]
            if dynamic.search(code):
                errors.append(f"{path.relative_to(root)}:{number}: dynamic dependency version")

    lockfiles = sorted(
        path
        for path in root.rglob("*.lockfile")
        if ".gradle" not in path.relative_to(root).parts
    )
    locked_components: set[tuple[str, str, str]] = set()
    lock_entry = re.compile(r"^([^:#=]+):([^:=]+):([^=]+)=")
    for path in lockfiles:
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            match = lock_entry.match(line.strip())
            if match:
                locked_components.add(match.groups())
    if not lockfiles:
        errors.append("no committed Gradle dependency lockfile found")
    if not locked_components:
        errors.append("Gradle lockfiles contain no external module entries")

    metadata_path = root / "gradle" / "verification-metadata.xml"
    verified: set[tuple[str, str, str]] = set()
    verified_artifacts = 0
    try:
        document = ET.parse(metadata_path)
    except (OSError, ET.ParseError) as exc:
        errors.append(f"cannot parse Gradle verification metadata: {exc}")
    else:
        for component in document.iter():
            if component.tag.rsplit("}", 1)[-1] != "component":
                continue
            key = (
                component.attrib.get("group", ""),
                component.attrib.get("name", ""),
                component.attrib.get("version", ""),
            )
            if all(key):
                verified.add(key)
            for child in component.iter():
                if child.tag.rsplit("}", 1)[-1] != "sha256":
                    continue
                value = child.attrib.get("value", "")
                if re.fullmatch(r"[0-9a-f]{64}", value):
                    verified_artifacts += 1
                else:
                    errors.append(f"verification metadata has invalid sha256 {value!r}")
    if verified_artifacts == 0:
        errors.append("Gradle verification metadata contains no SHA-256 artifact checks")
    for group, name, version in sorted(locked_components - verified):
        errors.append(f"locked Gradle module lacks checksum metadata: {group}:{name}:{version}")
    return len(locked_components), verified_artifacts, errors
