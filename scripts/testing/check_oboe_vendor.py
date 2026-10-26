"""Verify the exact upstream native projection; no C++ object layout is copied."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from gitignore_policy import check as check_ignore


def digest(data):
    return hashlib.sha256(data).hexdigest()


def verify(root, archive):
    metadata = json.loads((root / "engine/third_party/oboe-patches/SOURCE.json").read_text())
    assert metadata["schema"] == 1 and metadata["name"] == "oboe"
    assert metadata["version"] == "1.11.0"
    assert digest(archive.read_bytes()) == metadata["archive_sha256"], "upstream archive checksum differs"
    prefix = "oboe-" + metadata["revision"] + "/"
    assert metadata["retained_roots"] == ["include", "src"]
    assert metadata["retained_files"] == ["AUTHORS", "CMakeLists.txt", "CONTRIBUTORS", "LICENSE", "README.md"]
    expected = {}
    with tarfile.open(archive) as source:
        for member in source:
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            name = member.name[len(prefix):]
            if name in metadata["retained_files"] or name.split("/")[0] in metadata["retained_roots"]:
                assert not Path(name).is_absolute() and ".." not in Path(name).parts
                expected[name] = digest(source.extractfile(member).read())
    assert expected and metadata["files"] == expected, "declared projection differs from official archive"
    vendor = root / "engine/third_party/oboe-1.11.0"
    actual = {path.relative_to(vendor).as_posix(): digest(path.read_bytes())
              for path in vendor.rglob("*") if path.is_file()}
    assert actual == expected, "native sources differ from official projection"
    units = {line.strip() for line in (vendor / "CMakeLists.txt").read_text().splitlines()
             if line.strip().startswith("src/") and line.strip().endswith(".cpp")}
    assert len(units) == 62 and units == {name for name in expected if name.endswith(".cpp")}
    check_ignore(root, required=[f"engine/third_party/oboe-1.11.0/{name}" for name in expected])
    return expected


def main(root):
    metadata = json.loads((root / "engine/third_party/oboe-patches/SOURCE.json").read_text())
    with tempfile.TemporaryDirectory(prefix="migo-oboe-proof-") as temporary:
        work = Path(temporary)
        archive = Path(os.environ.get("MIGO_OBOE_UPSTREAM_ARCHIVE", str(work / "upstream.tar.gz")))
        if not archive.exists():
            with urllib.request.urlopen(metadata["archive_url"], timeout=60) as response, archive.open("wb") as output:
                size = 0
                while block := response.read(1024 * 1024):
                    size += len(block)
                    if size > 64 * 1024 * 1024:
                        raise ValueError("Oboe archive exceeds the reviewed download bound")
                    output.write(block)
        expected = verify(root, archive)
        # Actual malformed trees must fail the same checker as production.
        for kind in ("source", "license", "forged-inventory", "extra-source"):
            scratch = work / kind
            vendor = scratch / "engine/third_party/oboe-1.11.0"
            shutil.copytree(root / "engine/third_party/oboe-1.11.0", vendor)
            patch = scratch / "engine/third_party/oboe-patches"
            patch.mkdir()
            (patch / "SOURCE.json").write_text(json.dumps(metadata))
            if kind == "source":
                target = vendor / "src/common/AudioStream.cpp"
                target.write_bytes(target.read_bytes() + b"\n// unrecorded drift\n")
            elif kind == "license":
                (vendor / "LICENSE").unlink()
            elif kind == "extra-source":
                (vendor / "src/stray.cpp").write_text("// not an upstream input\n")
            else:
                damaged = dict(metadata)
                damaged["files"] = {**metadata["files"], "LICENSE": "0" * 64}
                (patch / "SOURCE.json").write_text(json.dumps(damaged))
            try:
                verify(scratch, archive)
            except AssertionError:
                pass
            else:
                raise AssertionError(f"negative control accepted {kind}")
        print(f"Oboe 1.11.0: {len(expected)} unchanged upstream files, 62 compilation units, four drift controls PASS")


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve())
