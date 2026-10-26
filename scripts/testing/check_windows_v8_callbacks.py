"""Replay the native patch and check the Rust consumer's callback agreement.

This source gate complements the real Windows DLL probe; it cannot prove an
archive was compiled from these sources or replace running that probe.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def slots(cpp: str) -> set[str]:
    table = cpp.split("const HostCallbackSlot kHostCallbackSlots[] = {", 1)[1].split("};", 1)[0]
    entries = re.findall(r'\{"([^"]+)",\s*&p_(\w+)\}', table)
    assert entries and all(name == pointer for name, pointer in entries), "misnamed DLL slot"
    assert len(entries) == len({name for name, _ in entries}), "duplicate DLL slot"
    return {name for name, _ in entries}


def registrations(rust: str) -> set[str]:
    body = rust.split("register_all!(", 1)[1].split(");", 1)[0]
    entries = re.findall(r"\w+::(\w+)", body)
    assert entries and len(entries) == len(set(entries)), "empty or duplicate Rust registration"
    return set(entries)


def probe_slots(probe: str) -> set[str]:
    body = probe.split("kExpected[] = {", 1)[1].split("};", 1)[0]
    entries = re.findall(r'"(\w+)"', body)
    assert entries and len(entries) == len(set(entries)), "empty or duplicate DLL probe"
    return set(entries)


def agreement(cpp: str, rust: str, probe: str) -> None:
    expected = slots(cpp)
    assert registrations(rust) == expected, "Rust consumer / DLL callback mismatch"
    assert probe_slots(probe) == expected, "DLL probe / actual callback mismatch"


def check(root: Path) -> None:
    patches = root / "engine/third_party/v8-patches"
    vendor = root / "engine/third_party/v8-150.4.0"
    consumer_patches = sorted((root / "engine/third_party/rusty-v8-rust-patches").glob("*.patch"))
    rust_patch = patches / "0007-windows-register-host-callbacks-from-rust.patch"
    paths = re.findall(r"^\+\+\+ b/(.+)$", rust_patch.read_text(), re.MULTILINE)
    assert len(paths) == 7, "native Rust patch file closure changed; review its consumer mirror"
    consumer_paths = set()
    for patch in consumer_patches:
        for name in re.findall(r'^\+\+\+ v8-150\.4\.0/(.+)$', patch.read_text(), re.MULTILINE):
            assert not Path(name).is_absolute() and '..' not in Path(name).parts
            consumer_paths.add(name)
    with tempfile.TemporaryDirectory(prefix="migo-windows-callbacks-") as temporary:
        scratch = Path(temporary)
        source = scratch / "v8-150.4.0"
        for name in set(paths + ["src/binding.cc", "src/crdtp_binding.cc"]) | consumer_paths:
            target = source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((vendor / name).read_bytes())
        # Remove the consumer patch to obtain upstream, then replay the native
        # build's patches. Both routes must produce identical Rust bytes.
        for patch in reversed(consumer_patches):
            subprocess.run(["patch", "--silent", "--batch", "--fuzz=0", "--reverse", "-p0",
                            "-i", str(patch)], cwd=scratch, check=True)
        for patch in [patches / "0005-windows-host-callback-registration.patch", rust_patch]:
            subprocess.run(["patch", "--silent", "--batch", "--forward", "--fuzz=0", "-p1",
                            "-i", str(patch)], cwd=source, check=True)
        # Additional Rust-only consumer features do not change the DLL's C++
        # ABI. Apply those after its callback mirror before comparing bytes.
        for patch in consumer_patches[1:]:
            subprocess.run(["patch", "--silent", "--batch", "--forward", "--fuzz=0", "-p0",
                            "-i", str(patch)], cwd=scratch, check=True)
        for name in paths:
            assert (source / name).read_bytes() == (vendor / name).read_bytes(), f"consumer mirror drift: {name}"
        cpp = (source / "src/binding.cc").read_text()
        rust = (source / "src/V8.rs").read_text()
        probe = (root / "scripts/test-windows-v8-dll.sh").read_text()
        agreement(cpp, rust, probe)
        # Each side independently loses a real newly introduced callback.
        name = "crdtp__DomainDispatcher__BASE__Dispatch"
        mutants = [
            (cpp.replace('{"' + name + '", &p_' + name + '},', ""), rust, probe),
            (cpp, rust.replace("crdtp::" + name + ",", ""), probe),
            (cpp, rust, probe.replace('"' + name + '",', "")),
        ]
        for mutant in mutants:
            try:
                agreement(*mutant)
            except AssertionError:
                pass
            else:
                raise AssertionError("negative control failed to detect a missing callback")
        platform = (source / "src/platform.rs").read_text().split("pub fn new_custom", 1)[1]
        assert platform.index("register_host_callbacks_once()") < platform.index("Box::"), \
            "CustomPlatform must register before constructing a callback owner"
        print(f"Windows V8: {len(slots(cpp))} callbacks; native / consumer / probe / negative controls PASS")


if __name__ == "__main__":
    check(Path(sys.argv[1]).resolve())
