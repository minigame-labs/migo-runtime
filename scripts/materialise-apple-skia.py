#!/usr/bin/env python3
"""Put the locked macOS Skia archives on disk, verified, and say where.

usage: materialise-apple-skia.py [--lock PATH] [--root DIR] TRIPLE [TRIPLE...]

Prints one line: a `file://` template for `SKIA_BINARIES_URL`, naming a
directory that holds `skia-binaries-<key>.tar.gz` for every TRIPLE, each with
the size and sha256 `contracts/artifact-manifest/apple-skia.lock.json` records.
Exits non-zero, printing why, if any of them cannot be had with those bytes.

WHY skia-bindings IS NOT ALLOWED TO DOWNLOAD THESE ITSELF. Its downloader caches
by FILE NAME in `OUT_DIR/.cache/` and resumes with `curl -C -`. Our archives
have exactly the names upstream's have -- the name is the binary-cache key, and
the key is the point -- so a build directory that once downloaded upstream's
archive, which is every macOS build directory older than the lock, holds a
complete file under the name ours is fetched to. Measured 2026-09-17 against
the x86_64 pair: curl asks to resume at upstream's 17,632,067 bytes, GitHub
sends the last 190,210 bytes of ours, curl exits 0, and the file is upstream's
archive with a fragment of ours glued on. skia-bindings reads the first gzip
member, which is all upstream's, prints DOWNLOAD AND INSTALL SUCCEEDED and links
the Skia that assumes desktop GL. The CI row that restores a cached target
directory failed `make_gl` exactly that way while the row that caches nothing
passed with the same lock -- and nothing checked the sha256 the lock records,
because the only reader of those hashes was a gate that looks at the release,
not at what a build installed.

So the bytes are fetched here, whole (never resumed), checked against the lock
before anything can read them, and handed to skia-bindings as a `file://` URL,
which it reads directly and never caches. The directory is named by a digest of
the lock's assets: different archives are a different URL, and skia-bindings
declares `rerun-if-env-changed` on `SKIA_BINARIES_URL`, so a lock change reruns
its build script and reinstalls instead of trusting an OUT_DIR from before.

A file already present is re-hashed on every call and kept only if it matches.
That costs well under a second for both archives, and it is what makes the
directory safe to cache: anything a cache or a person did to it is detected,
not trusted.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_LOCK = REPO / "contracts/artifact-manifest/apple-skia.lock.json"
DEFAULT_ROOT = REPO / "engine/target/apple-skia-archives"
ASSET_PREFIX = "skia-binaries-"
ASSET_SUFFIX = ".tar.gz"


class Refused(Exception):
    pass


def sha256_of(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def matches(path: pathlib.Path, size: int, sha256: str) -> bool:
    return path.is_file() and path.stat().st_size == size and sha256_of(path) == sha256


def asset_key(asset: str) -> str:
    if not (asset.startswith(ASSET_PREFIX) and asset.endswith(ASSET_SUFFIX)):
        raise Refused(f"asset {asset} is not named {ASSET_PREFIX}<key>{ASSET_SUFFIX}")
    return asset[len(ASSET_PREFIX) : -len(ASSET_SUFFIX)]


def lock_digest(targets: dict) -> str:
    """Names the set of archives, so a different set is a different directory."""
    lines = sorted(f"{triple} {entry['asset']} {entry['size_bytes']} {entry['sha256']}" for triple, entry in targets.items())
    return hashlib.sha256("\n".join(lines).encode()).hexdigest()


def fetch(url: str, destination: pathlib.Path, size: int, sha256: str) -> None:
    partial = destination.with_name(f".{destination.name}.partial-{os.getpid()}")
    try:
        # Whole, never resumed: a resume against a file that is not a prefix of
        # this one is the defect this script exists for.
        result = subprocess.run(
            ["curl", "--location", "--fail", "--silent", "--show-error", "--retry", "5",
             "--retry-connrefused", "--output", str(partial), url],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        if result.returncode != 0:
            raise Refused(f"could not download {url}: curl exited {result.returncode}: {result.stderr.strip()}")
        actual_size = partial.stat().st_size
        actual_sha = sha256_of(partial)
        if actual_size != size or actual_sha != sha256:
            raise Refused(
                f"{url} is not the archive the lock names: {actual_size} bytes, sha256 {actual_sha};"
                f" the lock says {size} bytes, sha256 {sha256}. A published asset was replaced, or"
                " the lock was edited without the release -- either way these bytes are not used."
            )
        os.replace(partial, destination)
    finally:
        partial.unlink(missing_ok=True)


def materialise(lock_path: pathlib.Path, root: pathlib.Path, triples: list[str]) -> str:
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    targets = lock["targets"]
    template = lock["url_template"]
    if "{key}" not in template:
        raise Refused(f"{lock_path} url_template has no {{key}}")
    unknown = [triple for triple in triples if triple not in targets]
    if unknown:
        raise Refused(
            f"{lock_path} has no archive for {', '.join(unknown)}; it locks {', '.join(sorted(targets))}."
            " Only macOS needs the corrected Skia (see scripts/apple-skia-gl-env.sh)."
        )
    directory = root / lock_digest(targets)
    directory.mkdir(parents=True, exist_ok=True)
    for triple in triples:
        entry = targets[triple]
        asset = entry["asset"]
        key = asset_key(asset)
        if triple not in key:
            raise Refused(f"{lock_path} files {asset} under {triple}, whose key does not name it")
        destination = directory / asset
        if matches(destination, entry["size_bytes"], entry["sha256"]):
            continue
        destination.unlink(missing_ok=True)
        fetch(template.replace("{key}", key), destination, entry["size_bytes"], entry["sha256"])
    return f"file://{directory}/{ASSET_PREFIX}{{key}}{ASSET_SUFFIX}"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--lock", type=pathlib.Path, default=DEFAULT_LOCK)
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path(os.environ.get("MIGO_SKIA_ARCHIVE_ROOT", DEFAULT_ROOT)))
    parser.add_argument("triples", nargs="+", metavar="TRIPLE")
    args = parser.parse_args(argv)
    try:
        print(materialise(args.lock.resolve(), args.root.resolve(), args.triples))
    except Refused as refusal:
        print(f"materialise-apple-skia: {refusal}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
