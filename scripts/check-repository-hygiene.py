#!/usr/bin/env python3
"""Reject machine-local or sensitive material in Git-tracked publication text."""

from __future__ import annotations

import argparse
import os
import re
import socket
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Iterable, Pattern


def _hex(value: str) -> str:
    return bytes.fromhex(value).decode("utf-8")


def _joined(*parts: str) -> str:
    return "".join(parts)


@dataclass(frozen=True)
class TextRule:
    name: str
    pattern: Pattern[str]


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    rule: str


def _text_rules() -> tuple[TextRule, ...]:
    short_brand = _hex("77 78")
    long_brands = (
        _hex("77 65 63 68 61 74"),
        _hex("77 65 69 78 69 6e"),
        "".join(chr(value) for value in (0x5FAE, 0x4FE1)),
    )
    brand_pattern = _joined(
        r"(?:",
        r"(?<![A-Za-z0-9_])",
        re.escape(short_brand),
        r"(?![A-Za-z0-9_])|",
        re.escape(short_brand),
        r"(?=(?:file|local|asset|adapter|api)(?:[^A-Za-z0-9_]|$))|",
        "|".join(re.escape(value) for value in long_brands),
        r")",
    )
    prefixed_brand_pattern = _joined(
        r"(?<![A-Za-z0-9_])", re.escape(short_brand), r"(?=[A-Za-z0-9])"
    )
    camel_brand_pattern = _joined(re.escape("W" + "x"), r"[A-Z][a-z]{2,}")

    posix_home = _joined(r"/(?:", "ho", "me", r"|root)/[A-Za-z0-9._-]+(?:/|\\b)")
    apple_home = _joined(r"/", "Users", r"/[A-Za-z0-9._-]+(?:/|\\b)")
    drive_home = _joined(
        r"(?i:[A-Z]:[\\/]+", "Users", r"[\\/]+[^\\/\s]+(?:[\\/]|\\b))"
    )
    subsystem_home = _joined(
        r"/mnt/[A-Za-z]/", "Users", r"/[A-Za-z0-9._-]+(?:/|\\b)"
    )
    workspace_home = _joined(r"/data/(?:", "work", "|home", r")/")
    private_key = _hex(
        "2d 2d 2d 2d 2d 42 45 47 49 4e 20 28 3f 3a 52 53 41 20 7c 45 43 20 7c 4f 50 45 4e 53 53 48 20 7c 44 53 41 20 29 3f 50 52 49 56 41 54 45 20 4b 45 59 2d 2d 2d 2d 2d"
    )

    return (
        TextRule("legacy brand namespace", re.compile(brand_pattern, re.IGNORECASE)),
        TextRule(
            "legacy brand namespace",
            re.compile(prefixed_brand_pattern, re.IGNORECASE),
        ),
        TextRule("legacy brand namespace", re.compile(camel_brand_pattern)),
        TextRule(
            "user-home absolute path",
            re.compile(
                "|".join(
                    (posix_home, apple_home, drive_home, subsystem_home, workspace_home)
                )
            ),
        ),
        TextRule("private signing key", re.compile(private_key)),
        TextRule(
            "cloud access credential",
            re.compile(
                r"(?:AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{32,}|"
                r"xox[baprs]-[A-Za-z0-9-]{20,}|AIza[0-9A-Za-z_-]{30,})"
            ),
        ),
        TextRule(
            "literal credential assignment",
            re.compile(
                r"(?i)(?:password|passwd|api[_-]?key|access[_-]?token|client[_-]?secret)"
                r"\s*[:=]\s*['\"](?!\$|\{|<|example|placeholder|redacted)"
                r"[^'\"\r\n]{8,}['\"]"
            ),
        ),
    )


# The prescreen/api-surface tooling's whole job is comparing a *third-party*
# bundle's identifiers against this build's own published surface -- the
# legacy short brand token this file's own _text_rules() hex-encodes appears
# in it only as a literal string being matched, never as a namespace this
# engine implements (that was removed deliberately; see .gitignore's own note
# on why the tool itself was un-ignored rather than kept exempt: ignoring it
# "put it beyond CI, so nothing could hold it to a contract"). The legacy
# brand namespace rule exists to catch an accidental leak of the old
# namespace into engine source or docs, not to forbid a compatibility
# scanner from naming the thing it scans for. Everything else this gate
# checks (credentials, private keys, local paths, the machine hostname)
# still applies to these files -- only the brand-namespace rule is narrowed.
#
# CHANGELOG.md and the two READMEs are exempt for the mirror-image reason: the
# release that removed the namespace has to be able to say so (the removal is
# the single biggest breaking change in v0.9.4), and both READMEs have to be
# able to name the org's public compatibility adapter -- the `migo-<brand>-adapter`
# package, which is how content written for the removed namespace still runs. A
# gate that forbids the token in the file whose job is to record its removal is
# a gate that cannot be satisfied. Prose using it as a live namespace would
# still be wrong there, but nothing here can tell that apart from a citation, so
# for those three files it is a review responsibility.
_LEGACY_BRAND_EXEMPT_PATHS = frozenset(
    {
        ".gitignore",
        "PRESCREEN.md",
        "CHANGELOG.md",
        "README.md",
        "README.zh-CN.md",
        # The concept page names the adapter repositories the engine does not
        # carry, for the same reason README.md does.
        "developer-docs/src/content/docs/concepts/sdk-architecture.mdx",
        "developer-docs/src/content/docs/0.9/concepts/sdk-architecture.mdx",
        "developer-docs/src/content/docs/en/concepts/sdk-architecture.mdx",
        "scripts/dump-api-surface.sh",
        "scripts/prescreen-game.sh",
        "scripts/test-prescreen-scanner.sh",
    }
)
_LEGACY_BRAND_EXEMPT_PREFIXES = ("tools/api-surface/fixture/", "tools/api-surface/probe/")


def _legacy_brand_exempt(path: str) -> bool:
    return path in _LEGACY_BRAND_EXEMPT_PATHS or path.startswith(_LEGACY_BRAND_EXEMPT_PREFIXES)


def _forbidden_path_reason(path: str) -> str | None:
    parts = PurePosixPath(path).parts
    local_state = {".agents", ".codex", ".gradle", ".idea", ".vscode"}
    if any(part in local_state for part in parts):
        return "local tool state"
    if any(part.startswith("mutants.out") for part in parts):
        return "mutation output"

    lower_name = PurePosixPath(path).name.lower()
    if lower_name.endswith((".jks", ".keystore", ".p12", ".pfx")):
        return "signing-key container"
    if lower_name.endswith(
        (
            ".apk",
            ".aar",
            ".hap",
            ".ipa",
            ".o",
            ".obj",
            ".pdb",
            ".profraw",
            ".hprof",
        )
    ):
        return "generated build or capture artifact"
    return None


def _tracked_paths(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", os.fspath(root), "ls-files", "-z"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return [os.fsdecode(value) for value in result.stdout.split(b"\0") if value]


def _read_text(path: Path) -> str | None:
    try:
        if path.is_symlink():
            return os.readlink(path)
        data = path.read_bytes()
    except FileNotFoundError:
        # A tracked file scheduled for deletion is outside the next publication.
        return None
    if b"\0" in data:
        return None
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


# Two shapes contain the short brand's letters and are not the brand, and both
# were failing this gate on master. They are neutralised for the brand rules only,
# by shape, so the token itself stays forbidden everywhere else -- including as a
# quoted global name, which is the use the rule exists for.
#
# * A Subresource Integrity digest: base64, so any two letters appear sooner or
#   later, and a lockfile carries thousands of them.
# * The exclusive-create flag of the engine's own file open op, which takes
#   Node.js flag strings; that flag is spelled with exactly those two letters.
_INTEGRITY_DIGEST = re.compile(r"\bsha(?:256|384|512)-[A-Za-z0-9+/]{40,}={0,2}")
_OPEN_FILE_EXCLUSIVE_FLAG = re.compile(
    r"(\bop_open_file(?:_sync)?\(\s*[^,()]*,\s*)([\"'])" + "w" + "x" + r"(\+?)\2"
)


def _without_non_brand_tokens(text: str) -> str:
    text = _INTEGRITY_DIGEST.sub("sha-digest", text)
    return _OPEN_FILE_EXCLUSIVE_FLAG.sub(r"\1\2open-exclusive\3\2", text)


def _scan_text(path: str, text: str, rules: Iterable[TextRule]) -> list[Finding]:
    findings: list[Finding] = []
    for line_number, line in enumerate(text.splitlines() or [text], start=1):
        for rule in rules:
            if rule.pattern.search(line):
                findings.append(Finding(path, line_number, rule.name))
    return findings


def check(root: Path) -> tuple[list[Finding], int, int]:
    paths = _tracked_paths(root)
    rules = _text_rules()
    findings: list[Finding] = []
    text_files = 0

    machine_name = socket.gethostname().strip()
    machine_rule = None
    if len(machine_name) >= 4 and machine_name.lower() not in {"localhost", "localhost.localdomain"}:
        machine_rule = TextRule(
            "machine-specific hostname", re.compile(re.escape(machine_name), re.IGNORECASE)
        )

    for relative in paths:
        candidate = root / relative
        if not os.path.lexists(candidate):
            # A tracked path removed from the working tree is pending deletion.
            continue
        reason = _forbidden_path_reason(relative)
        if reason is not None:
            findings.append(Finding(relative, 0, reason))
            continue

        text = _read_text(candidate)
        if text is None:
            continue
        text_files += 1
        brand_rules = tuple(rule for rule in rules if rule.name == "legacy brand namespace")
        other_rules = tuple(rule for rule in rules if rule.name != "legacy brand namespace")
        applicable_rules = other_rules if _legacy_brand_exempt(relative) else rules
        findings.extend(_scan_text(relative, text, other_rules))
        if not _legacy_brand_exempt(relative):
            findings.extend(
                _scan_text(relative, _without_non_brand_tokens(text), brand_rules)
            )
        findings.extend(_scan_text(relative, relative, applicable_rules))
        if machine_rule is not None:
            findings.extend(_scan_text(relative, text, (machine_rule,)))

    return findings, len(paths), text_files


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check Git-tracked publication text for local or sensitive material."
    )
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()

    try:
        findings, tracked_files, text_files = check(root)
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"Repository hygiene gate: ERROR ({error})", file=sys.stderr)
        return 2

    if tracked_files == 0 or text_files == 0:
        print("Repository hygiene gate: ERROR (no tracked publication text scanned)", file=sys.stderr)
        return 2
    if findings:
        for finding in sorted(set(findings), key=lambda item: (item.path, item.line, item.rule)):
            location = finding.path if finding.line == 0 else f"{finding.path}:{finding.line}"
            print(f"{location}: {finding.rule}", file=sys.stderr)
        print(
            f"Repository hygiene gate: FAIL ({len(set(findings))} finding(s))",
            file=sys.stderr,
        )
        return 1

    print(
        f"Repository hygiene gate: PASS ({text_files} tracked text file(s), "
        f"{tracked_files} tracked path(s))"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
