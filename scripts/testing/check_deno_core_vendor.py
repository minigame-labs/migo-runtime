"""Verify core provenance, actual Cargo selection, and negative controls."""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from gitignore_policy import check as check_ignored_sources
from gitignore_policy import matching_rules
import vendor_layout


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree(path: Path, layout=None) -> dict[str, str]:
    result = {}
    for current, dirs, files in os.walk(path):
        base = Path(current)
        dirs[:] = [name for name in dirs if vendor_layout.directory_needed(
            (base / name).relative_to(path).as_posix(), layout)]
        for name in files:
            file = base / name
            relative = file.relative_to(path).as_posix()
            if vendor_layout.selected(relative, layout):
                result[relative] = digest(file)
    return result


def check(root: Path, patch_set: str = "deno-core-patches") -> None:
    engine = root / "engine"
    patches = engine / "third_party" / patch_set
    name, version, expected = (patches / "UPSTREAM").read_text().split()
    layout = vendor_layout.load(patches, name, version)
    vendor = engine / f"third_party/{name}-{version}"
    cargo = tomllib.loads((engine / "Cargo.toml").read_text())
    selected = cargo["patch"]["crates-io"][name]["path"]
    assert (engine / selected).resolve() == vendor.resolve(), "Cargo selects a different core copy"
    lock = tomllib.loads((engine / "Cargo.lock").read_text())
    entries = [p for p in lock["package"] if p["name"] == name]
    assert len(entries) == 1 and entries[0]["version"] == version, "core lock version differs"
    assert "source" not in entries[0], "Cargo still builds registry core"
    cache = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "registry/cache"
    candidates = list(cache.glob(f"*/{name}-{version}.crate"))
    with tempfile.TemporaryDirectory(prefix="migo-core-vendor-") as scratch_dir:
        scratch = Path(scratch_dir)
        archive = candidates[0] if candidates else scratch / f"{name}-{version}.crate"
        if not candidates:
            urllib.request.urlretrieve(f"https://static.crates.io/crates/{name}/{name}-{version}.crate", archive)
        assert digest(archive) == expected, "upstream crate checksum differs"
        with tarfile.open(archive) as source:
            archive_inventory = {member.name.split('/', 1)[1]: member.size
                                 for member in source.getmembers() if member.isfile()}
            vendor_layout.validate_archive(layout, archive_inventory)
            source.extractall(scratch, filter="data")
        pristine = scratch / f"{name}-{version}"
        original = tree(pristine)
        patch_files = sorted(patches.glob("*.patch"))
        assert patch_files, "no core patches recorded"
        for patch in patch_files:
            subprocess.run(["patch", "--silent", "--forward", "-p0", "-i", str(patch)], cwd=scratch, check=True)
        expected_tree = tree(pristine, layout)
        actual = tree(vendor, layout)
        assert actual == expected_tree, "vendored core differs from upstream plus recorded patches"
        # Check the verified upstream inventory, rather than whatever files
        # survived a checkout. Ignored lockfiles must not silently go missing.
        check_ignored_sources(root, [
            (vendor.relative_to(root) / name).as_posix() for name in expected_tree
        ])
        if layout is not None:
            omitted = [(vendor.relative_to(root) / path).as_posix()
                       for path in archive_inventory if not vendor_layout.selected(path, layout)]
            rules = matching_rules(root, omitted)
            assert all(path in rules and not rules[path][1].startswith('!') for path in omitted), \
                'unused V8 source is not ignored by the recorded consumer layout'
        assert actual != vendor_layout.project(original, layout), \
            "negative control: withholding the patches compared equal"
        edited_file = {"v8": "src/platform.rs", "deno_core": "02_timers.js", "ada-url": "build.rs"}[name]
        with (pristine / edited_file).open("a") as changed:
            changed.write("\n// unrecorded drift\n")
        assert tree(pristine, layout) != actual, "negative control: an unrecorded edit compared equal"
    suffix = f', {len(actual)} retained consumer files' if layout else ''
    print(f"{name} vendor: {version}, checksum / patch / Cargo selection / negative controls PASS{suffix}")


if __name__ == "__main__":
    check(Path(sys.argv[1]).resolve(), sys.argv[2] if len(sys.argv) > 2 else "deno-core-patches")
