"""Exercise actual CPAL registration ownership and compile its Windows API path.

The old WASAPI realtime branch discarded the scheduling handle, and its fallback
requested unscoped TIME_CRITICAL priority. Migo instead selects a Windows-only
MMCSS scope through the existing Windows bindings. Host policy tests are not
native Windows proof. --native runs the real APIs on the owning Windows thread;
a denied service is recorded as refusal, never as successful RT promotion.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import tomllib


def command(args: list[str], *, cwd: Path, log: Path) -> subprocess.CompletedProcess:
    result = subprocess.run(args, cwd=cwd, text=True, encoding="utf-8", stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=180)
    log.write_text(result.stdout, encoding="utf-8")
    return result


def policy_tests(root: Path, work: Path) -> None:
    original = (root / "engine/third_party/cpal-0.18.2/src/host/wasapi/mmcss_scope.rs").read_text()
    mutations = {
        "production": None,
        "missing-release": ("(self.release)(handle);", "let _ = handle;"),
        "release-before-callback": ("        Ok(guard)", "        if let Some(handle) = guard.handle.take() { (guard.release)(handle); }\n        Ok(guard)"),
        "ignore-configuration-refusal": ('configure(guard.handle.as_ref().expect("new registration"))?;',
                                          'let _ = configure(guard.handle.as_ref().expect("new registration"));'),
    }
    for case, mutation in mutations.items():
        directory = work / case
        directory.mkdir()
        source = directory / "scope.rs"
        text = original
        if mutation:
            before, after = mutation
            assert text.count(before) == 1, f"scheduling mutation anchor changed: {case}"
            text = text.replace(before, after, 1)
            if case == "release-before-callback":
                text = text.replace("let guard = Self {", "let mut guard = Self {")
        source.write_text(text, encoding="utf-8")
        fixture = directory / "harness.rs"
        fixture.write_text(f'#[path = {json.dumps(str(source), ensure_ascii=False)}] mod scope;\n', encoding="utf-8")
        binary = directory / ("tests.exe" if os.name == "nt" else "tests")
        result = command(["rustc", "--edition=2021", "--test", str(fixture), "-o", str(binary)],
                         cwd=directory, log=directory / "build.log")
        assert result.returncode == 0, f"scheduling source did not compile: {case}: {result.stdout}"
        result = command([str(binary)], cwd=directory, log=directory / "tests.log")
        if mutation:
            assert result.returncode != 0 and "test result: FAILED" in result.stdout, \
                f"scheduling mutation escaped executable tests: {case}: {result.stdout}"
        else:
            assert result.returncode == 0 and "4 passed" in result.stdout, result.stdout
        print(f"MMCSS ownership {case}: expected {'rejection' if mutation else 'pass'} observed", flush=True)

    for trait, message in (("Send", "sent between threads"), ("Sync", "shared between threads")):
        directory = work / f"non-{trait.lower()}"
        directory.mkdir()
        source = directory / "scope.rs"
        source.write_text(original, encoding="utf-8")
        fixture = directory / "harness.rs"
        fixture.write_text(f'''#[path = {json.dumps(str(source), ensure_ascii=False)}] mod scope;
fn require<T: {trait}>() {{}}
fn main() {{ require::<scope::Registration<()>>(); }}
''', encoding="utf-8")
        result = command(["rustc", "--edition=2021", str(fixture), "-o", str(directory / "probe")],
                         cwd=directory, log=directory / "protected.log")
        assert result.returncode != 0 and "E0277" in result.stdout and message in result.stdout, result.stdout
        source.write_text(original.replace("_thread: PhantomData<Rc<()>>", "_thread: PhantomData<()>"), encoding="utf-8")
        result = command(["rustc", "--edition=2021", str(fixture), "-o", str(directory / "probe")],
                         cwd=directory, log=directory / "protection-removed.log")
        assert result.returncode == 0, "withholding thread-affinity protection must compile the forbidden transfer"
        print(f"MMCSS {trait}: forbidden trait rejected; actual protection-removal control compiled", flush=True)


def worker_scope_tests(root: Path, work: Path) -> None:
    # Execute the exact registration prefix of each real worker. Only the OS
    # acquisition and data callback are replaced; the scope and error delivery
    # binding are production text. This catches `let _ = register()` even when
    # registration ownership tests themselves still pass.
    directory = root / "engine/third_party/cpal-0.18.2/src/host/wasapi"
    original = (directory / "stream.rs").read_text()
    blocks = []
    for worker in ("run_input", "run_output"):
        start = original.index(f"fn {worker}(")
        begin = original.index("    // Registration belongs to this native worker,", start)
        end = original.index("\n    loop {", begin)
        blocks.append(original[begin:end])
    template = '''#[path = SCOPE_PATH] mod scope;
use std::{cell::Cell, sync::atomic::{AtomicUsize, Ordering}};
thread_local! {
    static LIVE: Cell<bool> = const { Cell::new(false) };
    static DENIED: Cell<bool> = const { Cell::new(false) };
}
mod mmcss {
    pub(super) fn register() -> Result<super::scope::Registration<()>, ()> {
        if super::DENIED.with(|v| v.get()) { return Err(()); }
        super::scope::Registration::acquire(
            || { super::LIVE.with(|v| v.set(true)); Ok(()) }, |_| Ok(()),
            |_| super::LIVE.with(|v| v.set(false)),
        )
    }
}
fn emit_error(errors: &AtomicUsize, _: ()) { errors.fetch_add(1, Ordering::Relaxed); }
fn run_input(error_callback: &AtomicUsize, callback: fn()) {
INPUT_PREFIX
    callback();
}
fn run_output(error_callback: &AtomicUsize, callback: fn()) {
OUTPUT_PREFIX
    callback();
}
fn callback() { assert_eq!(LIVE.with(|v| v.get()), !DENIED.with(|v| v.get()), "registration covers callbacks"); }
const WORKERS: [fn(&AtomicUsize, fn()); 2] = [run_input, run_output];
#[test] fn workers_keep_registration_live_through_callbacks() {
    DENIED.with(|v| v.set(false));
    let errors = AtomicUsize::new(0);
    for worker in WORKERS {
        worker(&errors, callback);
        assert!(!LIVE.with(|v| v.get()));
    }
    assert_eq!(errors.load(Ordering::Relaxed), 0);
}
#[test] fn denial_continues_callbacks_at_normal_priority() {
    DENIED.with(|v| v.set(true));
    let errors = AtomicUsize::new(0);
    for worker in WORKERS { worker(&errors, callback); }
    assert_eq!(errors.load(Ordering::Relaxed), 2);
}
#[test] fn callback_unwind_reverts_on_worker_exit() {
    DENIED.with(|v| v.set(false));
    let errors = AtomicUsize::new(0);
    for worker in WORKERS {
        assert!(std::panic::catch_unwind(|| worker(&errors, || panic!("callback unwound"))).is_err());
        assert!(!LIVE.with(|v| v.get()));
    }
}
'''
    for case in ("production", "discard-registration", "drop-before-loop"):
        output = work / f"workers-{case}"
        output.mkdir()
        prefixes = blocks.copy()
        if case == "discard-registration":
            prefixes = [block.replace("let _mmcss_registration =", "let _ =") for block in prefixes]
            assert all(a != b for a, b in zip(blocks, prefixes))
        elif case == "drop-before-loop":
            prefixes = [block + "\n    drop(_mmcss_registration);\n" for block in prefixes]
        source = template.replace("SCOPE_PATH", json.dumps(str(directory / "mmcss_scope.rs"), ensure_ascii=False))
        source = source.replace("INPUT_PREFIX", prefixes[0]).replace("OUTPUT_PREFIX", prefixes[1])
        fixture = output / "harness.rs"
        fixture.write_text(source, encoding="utf-8")
        binary = output / ("workers.exe" if os.name == "nt" else "workers")
        result = command(["rustc", "--edition=2021", "--test", "--cfg", 'feature="wasapi-mmcss"',
                          str(fixture), "-o", str(binary)], cwd=output, log=output / "build.log")
        assert result.returncode == 0, result.stdout
        result = command([str(binary)], cwd=output, log=output / "tests.log")
        if case == "production":
            assert result.returncode == 0 and "7 passed" in result.stdout, result.stdout
        else:
            assert result.returncode != 0 and "workers_keep_registration_live_through_callbacks ... FAILED" in result.stdout, \
                f"worker scope mutation escaped callback lifetime assertions: {case}: {result.stdout}"
        print(f"MMCSS real worker prefixes {case}: expected {'pass' if case == 'production' else 'rejection'} observed", flush=True)


def product_graphs(root: Path, work: Path) -> None:
    for profile in ("external-frames", "profile-full"):
        result = command(["cargo", "tree", "--locked", "-p", "migo-capi", "--no-default-features",
                          "--features", profile, "--target", "x86_64-pc-windows-msvc", "-e", "normal",
                          "--prefix", "none", "--color", "never", "--format", "{p}|{f}"],
                         cwd=root / "engine", log=work / f"windows-{profile}-graph.txt")
        assert result.returncode == 0, result.stdout
        rows = [line.removesuffix(" (*)").split("|") for line in result.stdout.splitlines()]
        cpal = [row for row in rows if row[0].startswith("cpal v")]
        assert cpal and all(len(row) == 2 and row[0].startswith("cpal v0.18.2 ")
                            and row[1] == "wasapi-mmcss" for row in cpal), "Windows product scheduling features drifted"
        assert not any(row[0].startswith(("audio_thread_priority v", "dbus v", "libdbus-sys v")) for row in rows), \
            "Windows scheduling added an unused cross-platform priority stack"
        print(f"Windows {profile}: scoped WASAPI scheduling / no priority-library or D-Bus closure PASS", flush=True)


def identity(package: dict) -> tuple:
    return tuple(package.get(key) for key in ("name", "version", "source", "checksum"))


def windows_probe(root: Path, work: Path, *, target: str | None, native: bool) -> None:
    probe = work / "windows-probe"
    (probe / "src").mkdir(parents=True)
    cpal = root / "engine/third_party/cpal-0.18.2"
    (probe / "Cargo.toml").write_text(f'''[package]
name = "migo-wasapi-scheduling-check"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
cpal = {{ path = {json.dumps(str(cpal), ensure_ascii=False)}, default-features = false, features = ["wasapi-mmcss"] }}
windows = {{ version = "=0.62.2", features = ["Win32_Foundation", "Win32_System_Threading"] }}
''', encoding="utf-8")
    (probe / "src/lib.rs").write_text(
        '#![allow(dead_code)]\nuse cpal::{Error, ErrorKind};\n'
        f'#[path = {json.dumps(str(cpal / "src/host/wasapi/mmcss.rs"), ensure_ascii=False)}]\nmod mmcss;\n', encoding="utf-8")
    lock = root / "engine/Cargo.lock"
    original_lock = lock.read_bytes()
    (probe / "Cargo.lock").write_bytes(original_lock)
    command_target = ["--target", target] if target else []
    metadata = command(["cargo", "metadata", "--format-version", "1", *(["--filter-platform", target] if target else [])],
                       cwd=probe, log=work / "windows-probe-metadata.jsonl")
    assert metadata.returncode == 0, metadata.stdout
    expected = {identity(p) for p in tomllib.loads(original_lock.decode())["package"]}
    resolved = tomllib.loads((probe / "Cargo.lock").read_text())["package"]
    unexpected = [identity(p) for p in resolved if p["name"] != "migo-wasapi-scheduling-check" and identity(p) not in expected]
    assert not unexpected, f"Windows scheduling source probe drifted from workspace: {unexpected}"
    if native:
        assert platform.system() == "Windows", "--native requires a Windows host; no emulated pass"
        args = ["cargo", "test", "--locked", "--lib", *command_target, "--", "--nocapture"]
    else:
        assert target and "windows" in target, "Windows target required for source probe"
        args = ["cargo", "check", "--locked", "--tests", *command_target]
    result = command(args, cwd=probe, log=work / ("windows-native.log" if native else "windows-source-check.log"))
    assert lock.read_bytes() == original_lock, "scheduling probe modified the workspace lock"
    assert result.returncode == 0, result.stdout
    if native:
        print(result.stdout, end="")
        assert "5 passed" in result.stdout and "MMCSS_NATIVE " in result.stdout, "native probe did not execute real APIs"
    print(f"Windows scheduling {'native API' if native else 'Rust source'}: "
          f"{len(resolved) - 1} workspace-locked dependency identities PASS", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("work", type=Path)
    parser.add_argument("--check-target")
    parser.add_argument("--native", action="store_true")
    args = parser.parse_args()
    root, work = args.root.resolve(), args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    assert not any(work.iterdir()), "scheduling output must be empty"
    policy_tests(root, work)
    worker_scope_tests(root, work)
    product_graphs(root, work)
    if args.check_target or args.native:
        windows_probe(root, work, target=args.check_target, native=args.native)
    print(f"CPAL scheduling contract PASS; native execution={args.native}; artifacts={work}", flush=True)


if __name__ == "__main__":
    main()
