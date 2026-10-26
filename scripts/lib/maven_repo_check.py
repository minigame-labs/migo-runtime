#!/usr/bin/env python3
"""Whether a staged Maven repository is the one `stage-maven-repo.py` promises.

Held to five properties, each one a way a published SDK has gone or could go
wrong without a build noticing:

  1. the coordinates: every POM names com.minigame-labs, its own artifactId and
     release/VERSION, packaging aar, with the metadata Central requires;
  2. the names: exactly the expected artifact directories, and in each exactly
     the AAR, POM, sources and javadoc jars with their four checksums and a
     signature -- no debug variant, no single-ABI intermediate, no stray file;
  3. the checksums: each is the digest of the file it names;
  4. the signatures: each `.asc` verifies over its file in the GNUPGHOME given;
  5. the bytes: each AAR is the AAR the release built, byte for byte, and the
     sources jar is the library's main sources and nothing else.

Prints one line per violation; exit 1 when there is any, 0 when there is none.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import subprocess
import sys
import xml.etree.ElementTree as ET
import zipfile
from android_runtime_deps import load as runtime_dependencies

ROOT = pathlib.Path(__file__).resolve().parents[2]
GROUP = "com.minigame-labs"
SOURCES = ROOT / "platforms/android/library/src/main/java"
CHECKSUMS = ("md5", "sha1", "sha256", "sha512")
POM_NS = "{http://maven.apache.org/POM/4.0.0}"
REQUIRED_POM = (
    "name", "description", "url", "licenses/license/name", "licenses/license/url",
    "developers/developer/name", "scm/connection", "scm/developerConnection", "scm/url",
)


def text_at(project: ET.Element, path: str) -> str:
    element = project.find("/".join(POM_NS + part for part in path.split("/")))
    return (element.text or "").strip() if element is not None else ""


def check_pom(path: pathlib.Path, artifact: str, version: str) -> list[str]:
    try:
        project = ET.parse(path).getroot()
    except ET.ParseError as error:
        return [f"{path.name}: not XML ({error})"]
    errors = []
    for field, expected in (("groupId", GROUP), ("artifactId", artifact),
                            ("version", version), ("packaging", "aar")):
        actual = text_at(project, field)
        if actual != expected:
            errors.append(f"{path.name}: {field} is {actual!r}, not {expected!r}")
    for field in REQUIRED_POM:
        if not text_at(project, field):
            errors.append(f"{path.name}: {field} is missing, which Central requires")
    expected = [(d["group"], d["artifact"], d["version"], "runtime")
                for d in runtime_dependencies()["maven_runtime"]]
    nodes = project.findall(f"{POM_NS}dependencies/{POM_NS}dependency")
    actual = [tuple(text_at(node, field) for field in ("groupId", "artifactId", "version", "scope"))
              for node in nodes]
    allowed = {POM_NS + field for field in ("groupId", "artifactId", "version", "scope")}
    if actual != expected or any(len(node) != 4 or {child.tag for child in node} != allowed for node in nodes):
        errors.append(f"{path.name}: runtime dependencies differ from canonical Android dependency list")
    return errors


def check_signature(path: pathlib.Path, gnupghome: str) -> list[str]:
    signature = path.with_name(f"{path.name}.asc")
    if not signature.is_file():
        return []  # reported as a missing file
    result = subprocess.run(
        ["gpg", "--batch", "--verify", str(signature), str(path)],
        capture_output=True, text=True, env={**os.environ, "GNUPGHOME": gnupghome},
    )
    if result.returncode != 0:
        return [f"{signature.name}: bad signature over {path.name}"]
    return []


def check_sources(path: pathlib.Path) -> list[str]:
    expected = {p.relative_to(SOURCES).as_posix(): p.read_bytes() for p in SOURCES.rglob("*.java")}
    try:
        with zipfile.ZipFile(path) as jar:
            actual = {name: jar.read(name) for name in jar.namelist()}
    except zipfile.BadZipFile:
        return [f"{path.name}: not a jar"]
    errors = []
    for name in sorted(set(actual) - set(expected)):
        errors.append(f"{path.name}: {name} is not one of the library's main sources")
    for name in sorted(set(expected) - set(actual)):
        errors.append(f"{path.name}: lacks {name}")
    for name in sorted(set(actual) & set(expected)):
        if actual[name] != expected[name]:
            errors.append(f"{path.name}: {name} differs from the library's source")
    return errors


def check(repo: pathlib.Path, expected_aars: dict[str, pathlib.Path], gnupghome: str) -> list[str]:
    version = (ROOT / "release/VERSION").read_text(encoding="utf-8").strip()
    errors: list[str] = []
    group_dir = repo / GROUP.replace(".", "/")
    expected_files: set[pathlib.Path] = set()
    for artifact in expected_aars:
        base = group_dir / artifact / version / f"{artifact}-{version}"
        for suffix in (".aar", ".pom", "-sources.jar", "-javadoc.jar"):
            primary = pathlib.Path(f"{base}{suffix}")
            expected_files.add(primary)
            expected_files.add(primary.with_name(f"{primary.name}.asc"))
            expected_files.update(primary.with_name(f"{primary.name}.{c}") for c in CHECKSUMS)

    actual_files = {path for path in repo.rglob("*") if path.is_file()}
    for path in sorted(actual_files - expected_files):
        errors.append(f"unexpected file {path.relative_to(repo)}: nothing but the release's "
                      "artifacts may be published")
    for path in sorted(expected_files - actual_files):
        errors.append(f"missing {path.relative_to(repo)}")

    for path in sorted(expected_files & actual_files):
        name = path.name
        algorithm = name.rsplit(".", 1)[-1]
        if algorithm in CHECKSUMS:
            target = path.with_name(name[: -len(algorithm) - 1])
            if target.is_file():
                digest = hashlib.new(algorithm, target.read_bytes()).hexdigest()
                if path.read_text(encoding="ascii", errors="replace").strip() != digest:
                    errors.append(f"{name}: checksum is not the {algorithm} of {target.name}")
        elif not name.endswith(".asc"):
            errors.extend(check_signature(path, gnupghome))

    for artifact, source_aar in expected_aars.items():
        base = group_dir / artifact / version / f"{artifact}-{version}"
        aar = pathlib.Path(f"{base}.aar")
        if aar.is_file() and aar.read_bytes() != source_aar.read_bytes():
            errors.append(f"{aar.name}: differs from the release AAR {source_aar}")
        pom = pathlib.Path(f"{base}.pom")
        if pom.is_file():
            errors.extend(check_pom(pom, artifact, version))
        sources = pathlib.Path(f"{base}-sources.jar")
        if sources.is_file():
            errors.extend(check_sources(sources))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--aar", required=True)
    parser.add_argument("--nojni-aar", required=True)
    parser.add_argument("--gnupghome", default=os.environ.get("GNUPGHOME", ""))
    args = parser.parse_args()
    errors = check(
        pathlib.Path(args.repo),
        {"migo-android": pathlib.Path(args.aar), "migo-android-nojni": pathlib.Path(args.nojni_aar)},
        args.gnupghome,
    )
    for error in errors:
        print(error)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
