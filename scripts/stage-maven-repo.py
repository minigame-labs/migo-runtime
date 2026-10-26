#!/usr/bin/env python3
"""Stage the Android SDK as a Maven repository: the directory a host's Gradle
resolves `com.minigame-labs:migo-android:<version>` from, and the bundle Maven
Central's Portal takes as one upload.

The AARs are the ones the release already built, verified and attested, copied
byte for byte. Nothing here builds an AAR: a second build for Maven would be a
second artifact under the first one's name, and the attestation would vouch for
bytes no host received.

Each artifact directory holds the AAR, its POM, a sources jar, a javadoc jar,
an `.md5` / `.sha1` / `.sha256` / `.sha512` for each of the four, and an `.asc`
signature for each -- what Central requires, and what any static host serving
the same tree needs nothing more than. No `maven-metadata.xml`: a fixed version
resolves without one, and one written here would have to be merged with the
versions already published, which is state this script does not have.

Everything but the signatures is reproducible: entries sorted, every timestamp
SOURCE_DATE_EPOCH (HEAD's commit time by default). A detached signature records
when it was made, so two signings of one file differ by design.

Usage:
  stage-maven-repo.py --aar migo-android.aar --nojni-aar migo-android-nojni.aar \\
      --out dist/maven --gpg-key KEYID [--bundle dist/migo-maven-central.zip]

GPG runs in the GNUPGHOME of the environment; MIGO_MAVEN_GPG_PASSPHRASE is
passed to it when set.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import time
import zipfile
from xml.sax.saxutils import escape
from lib.android_runtime_deps import load as runtime_dependencies

ROOT = pathlib.Path(__file__).resolve().parent.parent
GROUP = "com.minigame-labs"
REPOSITORY_URL = "https://github.com/minigame-labs/migo"
SOURCES = ROOT / "platforms/android/library/src/main/java"
DOCS_URL = "https://github.com/minigame-labs/migo/tree/master/developer-docs"

# artifactId -> (the flag naming its AAR, its POM name, its POM description).
ARTIFACTS = {
    "migo-android": (
        "aar",
        "Migo for Android",
        "An embeddable Canvas/WebGL mini-game runtime for Android apps: the Java SDK and "
        "its native engine, for arm64-v8a and x86_64.",
    ),
    "migo-android-nojni": (
        "nojni_aar",
        "Migo for Android, engine delivered separately",
        "The Migo Android SDK without its native engine, which the host delivers at "
        "runtime and the SDK verifies against the digest this AAR carries.",
    ),
}

CHECKSUMS = ("md5", "sha1", "sha256", "sha512")


def release_version() -> str:
    version = (ROOT / "release/VERSION").read_text(encoding="utf-8").strip()
    if not version or version.endswith("-SNAPSHOT"):
        raise SystemExit(f"release/VERSION {version!r} is not a publishable version")
    return version


def source_date_epoch() -> int:
    value = os.environ.get("SOURCE_DATE_EPOCH")
    if value is None:
        value = subprocess.run(
            ["git", "-C", str(ROOT), "log", "-1", "--format=%ct"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
    epoch = int(value)
    if epoch < 0:
        raise SystemExit(f"SOURCE_DATE_EPOCH must be non-negative, got {epoch}")
    return epoch


def zip_time(epoch: int) -> tuple[int, int, int, int, int, int]:
    # A zip stores local time from 1980 on; the epoch is held to that floor.
    stamp = time.gmtime(max(epoch, 315532800))
    return stamp[:6]


def write_jar(path: pathlib.Path, entries: dict[str, bytes], epoch: int) -> None:
    """A zip with sorted entries, one timestamp and no per-entry extra fields."""
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as jar:
        for name in sorted(entries):
            info = zipfile.ZipInfo(name, date_time=zip_time(epoch))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            jar.writestr(info, entries[name])


def sources_entries() -> dict[str, bytes]:
    entries = {
        path.relative_to(SOURCES).as_posix(): path.read_bytes()
        for path in SOURCES.rglob("*.java")
    }
    if not entries:
        raise SystemExit(f"no Java sources under {SOURCES}")
    return entries


def javadoc_entries(version: str) -> dict[str, bytes]:
    readme = f"""# Migo for Android {version}

The API documentation is the Javadoc in the sources jar beside this one, which
an IDE shows inline, and the developer guide:

  {DOCS_URL}
"""
    return {"README.md": readme.encode("utf-8")}


def pom(artifact: str, version: str, name: str, description: str) -> bytes:
    dependencies = "\n".join(
        f"    <dependency><groupId>{d['group']}</groupId><artifactId>{d['artifact']}</artifactId>"
        f"<version>{d['version']}</version><scope>runtime</scope></dependency>"
        for d in runtime_dependencies()["maven_runtime"]
    )
    text = f"""<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{GROUP}</groupId>
  <artifactId>{artifact}</artifactId>
  <version>{escape(version)}</version>
  <packaging>aar</packaging>
  <dependencies>
{dependencies}
  </dependencies>
  <name>{escape(name)}</name>
  <description>{escape(description)}</description>
  <url>{REPOSITORY_URL}</url>
  <licenses>
    <license>
      <name>Business Source License 1.1</name>
      <url>{REPOSITORY_URL}/blob/v{escape(version)}/LICENSE</url>
      <distribution>repo</distribution>
    </license>
  </licenses>
  <developers>
    <developer>
      <id>minigame-labs</id>
      <name>minigame-labs</name>
      <url>https://github.com/minigame-labs</url>
    </developer>
  </developers>
  <scm>
    <connection>scm:git:{REPOSITORY_URL}.git</connection>
    <developerConnection>scm:git:ssh://git@github.com/minigame-labs/migo.git</developerConnection>
    <url>{REPOSITORY_URL}/tree/v{escape(version)}</url>
    <tag>v{escape(version)}</tag>
  </scm>
</project>
"""
    return text.encode("utf-8")


def write_checksums(path: pathlib.Path) -> None:
    data = path.read_bytes()
    for algorithm in CHECKSUMS:
        digest = hashlib.new(algorithm, data).hexdigest()
        path.with_name(f"{path.name}.{algorithm}").write_text(digest, encoding="ascii")


def sign(path: pathlib.Path, key: str) -> None:
    command = ["gpg", "--batch", "--yes", "--armor", "--detach-sign",
               "--local-user", key, "--output", f"{path}.asc"]
    passphrase = os.environ.get("MIGO_MAVEN_GPG_PASSPHRASE")
    if passphrase is not None:
        command += ["--pinentry-mode", "loopback", "--passphrase-fd", "0"]
    command.append(str(path))
    result = subprocess.run(command, input=passphrase, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"gpg could not sign {path.name}: {result.stderr.strip()}")


def stage(args: argparse.Namespace) -> None:
    version = release_version()
    epoch = source_date_epoch()
    out = pathlib.Path(args.out)
    if out.exists():
        shutil.rmtree(out)
    sources = sources_entries()
    javadoc = javadoc_entries(version)
    for artifact, (flag, name, description) in ARTIFACTS.items():
        source_aar = pathlib.Path(getattr(args, flag))
        if not source_aar.is_file():
            raise SystemExit(f"--{flag.replace('_', '-')} {source_aar} is not a file")
        directory = out / GROUP.replace(".", "/") / artifact / version
        directory.mkdir(parents=True)
        base = directory / f"{artifact}-{version}"
        primaries = [
            pathlib.Path(f"{base}.aar"),
            pathlib.Path(f"{base}.pom"),
            pathlib.Path(f"{base}-sources.jar"),
            pathlib.Path(f"{base}-javadoc.jar"),
        ]
        shutil.copyfile(source_aar, primaries[0])
        primaries[1].write_bytes(pom(artifact, version, name, description))
        write_jar(primaries[2], sources, epoch)
        write_jar(primaries[3], javadoc, epoch)
        for path in primaries:
            os.utime(path, (epoch, epoch))
            write_checksums(path)
            sign(path, args.gpg_key)
    if args.bundle:
        bundle(out, pathlib.Path(args.bundle), epoch)


def bundle(out: pathlib.Path, destination: pathlib.Path, epoch: int) -> None:
    """The tree as one zip, rooted at the group's first segment, as the Portal's
    upload takes it."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    entries = {
        path.relative_to(out).as_posix(): path.read_bytes()
        for path in out.rglob("*")
        if path.is_file()
    }
    write_jar(destination, entries, epoch)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--aar", required=True, help="the published AAR, with the engine")
    parser.add_argument("--nojni-aar", required=True, help="the published engine-less AAR")
    parser.add_argument("--out", required=True, help="the repository root to (re)create")
    parser.add_argument("--gpg-key", required=True, help="the key every file is signed with")
    parser.add_argument("--bundle", help="also write the tree as a Central upload bundle")
    stage(parser.parse_args())
    return 0


if __name__ == "__main__":
    sys.exit(main())
