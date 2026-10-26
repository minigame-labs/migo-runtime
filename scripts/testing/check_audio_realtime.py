"""Native allocator/lock interception plus an independent callback syscall gate.

Link LLVM's unmodified RTSan archive into the selected test binaries, never
override workspace RUSTFLAGS, and never certify an uninstrumented fallback.
The only RTSan suppression is syscall(); the separate complete syscall trace
accepts only FUTEX_WAKE_PRIVATE(address, 1), and requires the parked slow path.
Calibration controls must fail for System allocation, cold Thread::current,
pthread locking and a futex WAIT that the RTSan suppression would otherwise hide.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import tomllib
import urllib.request


BEGIN = 'write(2, "MIGO_RT_BEGIN\\n", 14)'
END = 'write(2, "MIGO_RT_END\\n", 12)'
WAKE = re.compile(r"futex\(0x[0-9a-f]+, FUTEX_WAKE_PRIVATE, 1\)\s+= [01]$")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_trace(prefix: Path, *, require_wake: bool = False) -> int:
    intervals = []
    for path in sorted(prefix.parent.glob(prefix.name + ".*")):
        inside = False
        for line in path.read_text().splitlines():
            if line.startswith(BEGIN):
                assert not inside, f"nested RT trace interval: {path}"
                inside = True
                intervals.append([])
            elif line.startswith(END):
                assert inside, f"unmatched RT end: {path}"
                inside = False
            elif inside:
                intervals[-1].append(line)
        assert not inside, f"unterminated RT trace interval: {path}"
    assert len(intervals) == 1, f"expected one native callback interval, got {len(intervals)}"
    for line in intervals[0]:
        assert WAKE.fullmatch(line), f"unexpected syscall in callback: {line}"
    assert not require_wake or intervals[0], "parked callback never exercised native wake"
    return len(intervals[0])


def runtime(root: Path, work: Path) -> tuple[Path, dict]:
    pin = json.loads((root / "contracts/testing/audio-realtime.json").read_text())
    toolchain = tomllib.loads((root / "engine/rust-toolchain.toml").read_text())
    assert toolchain["toolchain"]["channel"] == pin["rust_toolchain"], "re-audit std wake for new toolchain"
    version = subprocess.check_output(["rustc", "--version"], text=True)
    assert version.startswith(f'rustc {pin["rust_toolchain"]} '), f"unexpected Rust compiler: {version}"
    cached = os.environ.get("MIGO_RTSAN_DEB")
    package = Path(cached) if cached else work / "runtime.deb"
    if not cached:
        with urllib.request.urlopen(pin["url"], timeout=60) as response:
            payload = response.read(pin["bytes"] + 1)
        assert len(payload) == pin["bytes"], "RTSan package length mismatch"
        package.write_bytes(payload)
    assert package.stat().st_size == pin["bytes"], "RTSan package length mismatch"
    assert digest(package) == pin["sha256"], "RTSan package checksum mismatch"
    extracted = work / "llvm"
    subprocess.run(["dpkg-deb", "-x", str(package), str(extracted)], check=True)
    archive = extracted / pin["archive"]["path"]
    assert digest(archive) == pin["archive"]["sha256"], "RTSan archive checksum mismatch"
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    source = sysroot / "lib/rustlib/src/rust/library/std/src"
    for relative, expected in pin["std_source_sha256"].items():
        assert digest(source / relative) == expected, f"re-audit pinned std wake path: {relative}"
    return archive, pin


def link_flags(archive: Path) -> list[str]:
    flags = ["-C", "link-arg=-Wl,--whole-archive", "-C", f"link-arg={archive}",
             "-C", "link-arg=-Wl,--no-whole-archive"]
    for library in ("dl", "pthread", "rt", "m"):
        flags += ["-C", f"link-arg=-l{library}"]
    flags += ["-C", "link-arg=-Wl,--export-dynamic"]
    return flags


def build(root: Path, work: Path, archive: Path, package: str, target: list[str]) -> Path:
    command = ["cargo", "rustc", "--locked", "-p", package, *target,
               "--message-format=json", "--", *link_flags(archive)]
    log = work / f"{package}-build.jsonl"
    with log.open("w") as out:
        result = subprocess.run(command, cwd=root / "engine", stdout=out, stderr=subprocess.STDOUT)
    if result.returncode:
        raise AssertionError(f"native test build failed: {log}\n{log.read_text()[-6000:]}")
    found = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable"):
            found.append(Path(row["executable"]))
    assert len(found) == 1, f"expected one actual Cargo test executable: {found}"
    return found[0]


def check_policy(root: Path, work: Path) -> dict:
    """Execute the production owner policy and actual source mutations."""
    directory = work / "buffer-policy"
    directory.mkdir()
    parent = (root / "engine/crates/audio/src/output.rs").read_text()
    constants = re.findall(r"^pub\(crate\) const (?:RENDER_QUANTUM_FRAMES|RING_BUFFER_FRAMES): usize = [0-9]+;$",
                           parent, re.MULTILINE)
    assert len(constants) == 2, "buffer policy constants moved; re-audit fixture"
    original = (root / "engine/crates/audio/src/output_policy.rs").read_text()
    controls = {
        "production": None,
        "ignore-device-burst": (".max(self.hint_frames)", ".max(0)", "actual_burst_and_observed_callback"),
        "ignore-software-miss": ("(reserve + 1).min(8)", "reserve", "software_miss_adds_reserve"),
        "unbounded-target": (".saturating_mul(RENDER_QUANTUM_FRAMES)\n            .min(RING_BUFFER_FRAMES)",
                             ".saturating_mul(RENDER_QUANTUM_FRAMES)", "unknown_odd_and_hostile_hints"),
    }
    for case, mutation in controls.items():
        source = directory / f"{case}.rs"
        text = original
        if mutation:
            before, after, expected_test = mutation
            assert text.count(before) == 1, f"buffer mutation anchor changed: {case}"
            text = text.replace(before, after, 1)
        source.write_text(text)
        fixture = directory / f"{case}-fixture.rs"
        fixture.write_text("\n".join(constants) + f'\n#[path = {json.dumps(str(source))}] mod policy;\n')
        binary = directory / f"{case}-tests"
        with (directory / f"{case}-build.log").open("w") as out:
            result = subprocess.run(["rustc", "--edition=2024", "--crate-name", "policy_probe", "--test",
                                     str(fixture), "-o", str(binary)], stdout=out, stderr=subprocess.STDOUT)
        assert result.returncode == 0, f"buffer policy source did not compile: {case}"
        result = subprocess.run([str(binary)], cwd=directory, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=30)
        (directory / f"{case}.log").write_text(result.stdout)
        if mutation:
            assert result.returncode != 0 and "test result: FAILED" in result.stdout and \
                expected_test in result.stdout, f"buffer policy mutation did not fail its assertion: {case}"
        else:
            assert result.returncode == 0 and "3 passed" in result.stdout, result.stdout
        print(f"native buffer policy {case}: expected {'rejection' if mutation else 'pass'} observed", flush=True)
    return {"production_tests": 3, "compiled_negative_controls": 3}


def check(root: Path, work: Path) -> None:
    assert platform.system() == "Linux" and platform.machine() == "x86_64", "Linux x86_64 gate required"
    strace = shutil.which(os.environ.get("MIGO_STRACE", "strace"))
    assert strace, "strace is required; there is no trace-free fallback"
    archive, pin = runtime(root, work)
    # Match the syscall interceptor's function name only, never a stack or
    # callback wildcard (which would also hide allocations and locking).
    suppression = work / "rtsan.supp"
    suppression.write_text("function-name-matches:syscall\n")
    outputs = []

    def run(binary: Path, test: str, case: str, *, suppressed: bool, traced: bool = False,
            unsafe_call: str | None = None, forbidden_wait: bool = False) -> None:
        name = f"{case}-{'scoped' if suppressed else 'strict'}"
        env = os.environ.copy()
        env["MIGO_RTSAN_CASE"] = case
        # Do not inherit user suppression files or a recovery setting that
        # would turn a diagnostic into exit 0.
        options = "halt_on_error=1:exitcode=43:symbolize=false"
        if suppressed:
            options += f":suppressions={suppression}"
        env["RTSAN_OPTIONS"] = options
        prefix = work / f"{name}.trace"
        command = [str(binary)]
        if test:
            command += ["--exact", test, "--ignored", "--nocapture"]
        if traced:
            command = [strace, "-ff", "-qq", "-s", "128", "-o", str(prefix), *command]
        log = work / f"{name}.log"
        with log.open("w") as out:
            result = subprocess.run(command, cwd=work, env=env, stdout=out,
                                    stderr=subprocess.STDOUT, timeout=30)
        message = log.read_text()
        wake_count = None
        if unsafe_call:
            assert result.returncode == 43, f"missing RTSan failure: {name}: {result.returncode}: {message}"
            assert "RealtimeSanitizer: unsafe-library-call" in message and \
                f"function `{unsafe_call}`" in message, f"unrelated negative-control failure: {message}"
        else:
            success_marker = "1 passed" if test else "AUDIO_WAKE_SOURCE_PROBE_OK"
            assert result.returncode == 0 and success_marker in message, f"native case failed: {name}: {message}"
            if traced:
                if forbidden_wait:
                    try:
                        check_trace(prefix)
                    except AssertionError as error:
                        assert "unexpected syscall in callback: futex(" in str(error) and \
                            "FUTEX_WAIT_PRIVATE" in str(error), f"unrelated trace failure: {error}"
                    else:
                        raise AssertionError("blocked-syscall control escaped the callback trace gate")
                else:
                    wake_count = check_trace(prefix, require_wake=case.startswith("parked-"))
        outputs.append({"case": case, "suppressed_syscall": suppressed,
                        "exit_code": result.returncode, "wake_count": wake_count,
                        "expected_unsafe_call": unsafe_call, "trace_rejects_wait": forbidden_wait})
        print(f"native RT {name}: expected {'rejection' if unsafe_call or forbidden_wait else 'pass'} observed", flush=True)

    harness = build(root, work, archive, "migo-alloc-probe", ["--test", "harness"])
    test = "native_realtime_runtime_matrix"
    run(harness, test, "clean", suppressed=False, traced=True)
    for case, call in (("system-allocation", "malloc"), ("system-deallocation", "free"), ("thread-current", "malloc"),
                       ("pthread-mutex", "pthread_mutex_lock")):
        run(harness, test, case, suppressed=False, unsafe_call=call)
        run(harness, test, case, suppressed=True, unsafe_call=call)
    run(harness, test, "blocking-syscall", suppressed=False, unsafe_call="syscall")
    run(harness, test, "blocking-syscall", suppressed=True, traced=True, forbidden_wait=True)
    audio = build(root, work, archive, "migo-audio", ["--tests"])
    test = "output::tests::native_realtime_callback_matrix"
    run(audio, test, "cold-callbacks", suppressed=False, traced=True)
    for case in ("parked-callback", "parked-converted-callback"):
        run(audio, test, case, suppressed=False, unsafe_call="syscall")
        run(audio, test, case, suppressed=True, traced=True)

    # Mutate the executable production notification, not a copy of its logic
    # or a test assertion. Compile only this std-only module in an isolated
    # native fixture. The workspace and Cargo target are never edited.
    libraries = {}
    for line in (work / "migo-alloc-probe-build.jsonl").read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact":
            for filename in row.get("filenames", []):
                if filename.endswith(".rlib"):
                    libraries[row["target"]["name"]] = Path(filename)
    original = (root / "engine/crates/shared/src/channel/audio.rs").read_text()
    needle = "pub(super) fn notify(&self) {"
    assert original.count(needle) == 1, "notification mutation anchor changed"
    mutations = {
        "source-clean": "",
        "source-heap": "std::hint::black_box(Box::new(41u8));",
        "source-cold-thread": "std::hint::black_box(std::thread::current());",
        "source-wait": """let word = std::sync::atomic::AtomicU32::new(0);
            let timeout = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG, 0,
                &timeout as *const libc::timespec); }""",
    }
    for case, injection in mutations.items():
        directory = work / case
        directory.mkdir()
        source = directory / "audio.rs"
        source.write_text(original.replace(needle, needle + injection, 1))
        fixture = directory / "main.rs"
        fixture.write_text(f"""#[path = {json.dumps(str(source))}] mod audio;
fn main() {{
    let probe = migo_alloc_probe::RealtimeProbe::load().unwrap();
    let wake = std::sync::Arc::new(audio::AudioWake::default());
    wake.bind();
    let notify = wake.clone();
    migo_alloc_probe::on_native_thread(move || {{
        probe.run_traced(|| notify.notify());
    }}).unwrap();
    assert!(wake.wait_timeout(std::time::Duration::ZERO));
    println!("AUDIO_WAKE_SOURCE_PROBE_OK");
}}
""")
        binary = directory / "notification"
        command = ["rustc", "--edition=2024", "--crate-name", "notification", str(fixture),
                   "-o", str(binary), "-L", f'dependency={libraries["migo_alloc_probe"].parent}']
        for name in ("migo_alloc_probe", "libc"):
            command += ["--extern", f"{name}={libraries[name]}"]
        command += link_flags(archive)
        with (directory / "build.log").open("w") as out:
            result = subprocess.run(command, stdout=out, stderr=subprocess.STDOUT)
        assert result.returncode == 0, f"source mutation failed to compile: {directory}"
        run(binary, "", case, suppressed=True, traced=case in ("source-clean", "source-wait"),
            unsafe_call="malloc" if case in ("source-heap", "source-cold-thread") else None,
            forbidden_wait=case == "source-wait")
    policy_result = check_policy(root, work)
    (work / "result.json").write_text(json.dumps({"runtime": pin, "cases": outputs, "buffer_policy": policy_result,
        "binaries": [{"path": str(p), "sha256": digest(p)} for p in (harness, audio)],
        "scope": "Linux host callback allocator/lock/syscall evidence; no hard RT, device latency or other-platform claim"}, indent=2) + "\n")
    print(f"Native audio RT contract PASS; complete artifacts: {work}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    work = args.output or Path(tempfile.mkdtemp(prefix="migo-audio-rt-"))
    work.mkdir(parents=True, exist_ok=True)
    # Existing traces cannot supply the evidence for a new invocation.
    assert not any(work.iterdir()), "output directory must be empty"
    check(args.root.resolve(), work.resolve())
