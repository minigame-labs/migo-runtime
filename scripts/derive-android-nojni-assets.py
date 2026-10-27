#!/usr/bin/env python3
"""Derive the engine-less AAR and its per-ABI engine archives from one built AAR.

A host that integrates Migo pays ~19 MB of first-install download and ~45 MB of
installed size per ABI for `libmigo.so`, whether or not a user ever opens a
mini-game. This splits the published AAR in two so that cost can be deferred:

  migo-<version>-android-nojni.aar        the same build with `jni/**` removed
  migo-<version>-jni-android-<arch>.tar.gz  the bytes that were removed

Both are derived from the AAR the release already publishes rather than built
separately, so there is no second build to drift: the classes, the manifest and
the embedded artifact identities are the same bytes in both AARs, and the gate
in `test-android-nojni-aar-contract.sh` is what keeps that true.

The engine-less AAR deliberately keeps `assets/migo/artifacts/**`. It carries no
engine, but it carries the SHA-256 of the engine it expects, which is what lets
`MigoNativeLoader` check a host-delivered binary offline -- without reaching the
network and without trusting whatever mirror the host fetched it from.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import pathlib
import sys
import tarfile
import zipfile

# The ABI directory inside an AAR, and the `arch` segment the release asset
# scheme uses. They differ (`arm64-v8a` vs `arm64`) and always have: the asset
# names follow the C ABI packages, which predate this script.
ARCH_SEGMENT = {"arm64-v8a": "arm64", "x86_64": "x86_64"}

JNI_PREFIX = "jni/"
SLICE_PREFIX = "assets/migo/artifacts/slices/"
ENGINE_BINARY = "libmigo.so"


class DeriveError(RuntimeError):
    pass


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aar", required=True, type=pathlib.Path)
    parser.add_argument(
        "--nojni-out", required=True, type=pathlib.Path, help="the engine-less AAR to write"
    )
    parser.add_argument(
        "--archive-template",
        required=True,
        help="output path for each engine archive, with {arch} standing for the "
        "release asset scheme's arch segment (arm64 / x86_64)",
    )
    parser.add_argument(
        "--source-date-epoch",
        type=int,
        default=0,
        help="timestamp stamped into the archives; keeps them reproducible",
    )
    parser.add_argument(
        "--compress-level",
        type=int,
        default=9,
        choices=range(1, 10),
        metavar="1-9",
        help="gzip level. 9 for anything published; a local or CI variant build "
        "passes 1, where the point is to exercise this path rather than to ship "
        "the result, and -9 over a debug engine costs minutes",
    )
    return parser.parse_args()


CHUNK = 1 << 20


def stream_sha256(stream) -> tuple[str, int]:
    """Digest and length of a stream, without ever holding it whole."""
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = stream.read(CHUNK)
        if not chunk:
            break
        digest.update(chunk)
        size += len(chunk)
    return digest.hexdigest(), size


def derive_nojni_aar(source: pathlib.Path, destination: pathlib.Path) -> list[str]:
    """Rewrite the AAR without its `jni/` entries, preserving everything else."""
    removed: list[str] = []
    with zipfile.ZipFile(source) as src:
        entries = src.infolist()
        if not any(info.filename.startswith(JNI_PREFIX) for info in entries):
            raise DeriveError(
                f"{source.name} carries no {JNI_PREFIX} entries; deriving an "
                "engine-less AAR from an already engine-less one would publish two "
                "names for one artifact"
            )
        with zipfile.ZipFile(destination, "w") as out:
            for info in entries:
                if info.filename.startswith(JNI_PREFIX):
                    removed.append(info.filename)
                    continue
                # Carry the entry's own metadata across: the date_time an AAR
                # holds is already normalised by the reproducible build, and
                # re-deriving it here would be a second source of truth.
                copied = zipfile.ZipInfo(info.filename, date_time=info.date_time)
                copied.compress_type = info.compress_type
                copied.external_attr = info.external_attr
                copied.internal_attr = info.internal_attr
                copied.create_system = info.create_system
                out.writestr(copied, src.read(info.filename))
    return removed


def engine_archives(
    source: pathlib.Path,
    archive_template: str,
    epoch: int,
    compress_level: int,
) -> list[pathlib.Path]:
    """One archive per ABI, holding exactly what the engine-less AAR dropped."""
    written: list[pathlib.Path] = []
    with zipfile.ZipFile(source) as src:
        names = src.namelist()
        abis = sorted(
            {
                name[len(JNI_PREFIX):].split("/", 1)[0]
                for name in names
                if name.startswith(JNI_PREFIX) and "/" in name[len(JNI_PREFIX):]
            }
        )
        if not abis:
            raise DeriveError(f"{source.name} carries no ABI directories under {JNI_PREFIX}")
        for abi in abis:
            if abi not in ARCH_SEGMENT:
                raise DeriveError(
                    f"{source.name} carries jni/{abi}, which the release asset scheme "
                    f"has no arch segment for (known: {sorted(ARCH_SEGMENT)})"
                )
            members = sorted(
                name
                for name in names
                if name.startswith(f"{JNI_PREFIX}{abi}/") and not name.endswith("/")
            )
            binary = f"{JNI_PREFIX}{abi}/{ENGINE_BINARY}"
            if binary not in members:
                raise DeriveError(f"{source.name} has no {binary} to publish")

            slice_path = f"{SLICE_PREFIX}{abi}.json"
            if slice_path not in names:
                raise DeriveError(
                    f"{source.name} ships {binary} but no {slice_path}. Without the "
                    "manifest the SDK has nothing to verify a delivered engine against "
                    "and will refuse to load it"
                )
            slice_bytes = src.read(slice_path)
            declared = json.loads(slice_bytes.decode("utf-8"))["hashes"]["runtime_binary"]
            with src.open(binary) as engine:
                actual, _ = stream_sha256(engine)
            if declared != actual:
                raise DeriveError(
                    f"{binary} hashes to {actual} but {slice_path} declares {declared}. "
                    "Publishing this pair would ship an engine no host could load"
                )

            package = pathlib.Path(archive_template.format(arch=ARCH_SEGMENT[abi]))
            package.parent.mkdir(parents=True, exist_ok=True)
            prefix = package.name[: -len(".tar.gz")]
            staged: list[tuple[str, str | bytes, int]] = [
                (f"{prefix}/{name.rsplit('/', 1)[1]}", name, 0o755) for name in members
            ]
            staged.append((f"{prefix}/slice.json", slice_bytes, 0o644))
            staged.append(
                (f"{prefix}/README.md", readme(prefix, abi, actual).encode("utf-8"), 0o644)
            )
            write_reproducible_tar_gz(package, src, staged, epoch, compress_level)
            written.append(package)
    return written


def write_reproducible_tar_gz(
    package: pathlib.Path,
    source_zip: zipfile.ZipFile,
    members: list[tuple[str, "str | bytes", int]],
    epoch: int,
    compress_level: int,
) -> None:
    """Same recipe as package-sdk.sh: sorted, ownerless, one fixed timestamp.

    gzip is written with mtime=0 for the same reason that script passes `-n`:
    the timestamp gzip stores in its own header is invisible to tar and would
    still make two builds of the same input differ.

    Members whose payload is a `str` name an entry in `source_zip` and are
    streamed straight from it. A debug `libmigo.so` is ~374 MB; buffering one --
    let alone one per ABI -- is how a packaging step OOMs a CI runner.
    """
    with open(package, "wb") as out:
        with gzip.GzipFile(
            fileobj=out, mode="wb", compresslevel=compress_level, mtime=0
        ) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w|", format=tarfile.GNU_FORMAT
            ) as tar:
                for name, payload, mode in sorted(members, key=lambda member: member[0]):
                    info = tarfile.TarInfo(name)
                    info.mtime = epoch
                    info.mode = mode
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    if isinstance(payload, bytes):
                        info.size = len(payload)
                        tar.addfile(info, io.BytesIO(payload))
                    else:
                        info.size = source_zip.getinfo(payload).file_size
                        with source_zip.open(payload) as entry:
                            tar.addfile(info, entry)


def readme(prefix: str, abi: str, sha256: str) -> str:
    return f"""# Migo engine binary — {abi}

`{prefix}`

`libmigo.so` for `{abi}`, the file the `-nojni` AAR does not carry.

    sha256(libmigo.so) = {sha256}

Hand it to the SDK through a `NativeLibraryProvider`:

```java
MigoNativeLoader.setProvider(context, abi -> {{
    File engine = new File(context.getNoBackupFilesDir(), abi + "/libmigo.so");
    return engine.isFile() ? engine : null;   // null means "not yet"
}});
```

The SDK verifies the file against the manifest embedded in the AAR before
loading it, so a partial download or a mirror still serving the previous
release fails at load with a readable reason rather than crashing inside the
engine. `slice.json` in this archive is that manifest; `MigoNativeLoader
.requiredArtifact(context)` returns the same digest at runtime.

## Where you may fetch this from

- **Google Play** — fetching executable code from anywhere but Play violates the
  Device and Network Abuse policy. Put this file in a Play Feature Delivery
  on-demand module and return the path Play installed.
- **Other stores** — Feature Delivery does not exist there. Host the file
  yourself and download it on first use.

## Licence

Migo is licensed under BSL 1.1 (see `LICENSE` in the repository). Hosting this
binary solely to deliver it into your own app is covered; redistributing it as a
standalone library or SDK is not. See `LEGAL.md`.
"""


def main() -> int:
    args = parse_arguments()
    if not args.aar.is_file():
        print(f"derive-android-nojni-assets: no such AAR: {args.aar}", file=sys.stderr)
        return 1
    if "{arch}" not in args.archive_template:
        print(
            "derive-android-nojni-assets: --archive-template must contain {arch}; "
            "without it every ABI would write over the last one",
            file=sys.stderr,
        )
        return 1
    nojni = args.nojni_out
    nojni.parent.mkdir(parents=True, exist_ok=True)

    try:
        removed = derive_nojni_aar(args.aar, nojni)
        archives = engine_archives(
            args.aar, args.archive_template, args.source_date_epoch, args.compress_level
        )
    except DeriveError as error:
        if nojni.exists():
            nojni.unlink()
        print(f"derive-android-nojni-assets: {error}", file=sys.stderr)
        return 1

    print(f"OK: {nojni.name}  {nojni.stat().st_size} bytes  (dropped {len(removed)} jni entries)")
    for archive in archives:
        print(f"OK: {archive.name}  {archive.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
