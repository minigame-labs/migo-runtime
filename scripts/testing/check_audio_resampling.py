#!/usr/bin/env python3
"""Offline, isolated Rubato 5.0.1 stream/timeline/alias/allocation gate.

Requires an explicitly supplied Cargo home containing the published dependencies.
No product dependency, Root lock, registry cache or system directory is modified.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("transport", ROOT / "scripts/testing/check_audio_streaming_transport.py")
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
codecs = transport.codecs


def controls(package: Path, output: Path, env: dict) -> int:
    source = output / "source/bandlimited.rs"
    original = source.read_text()
    cases = [
        ("startup-delay-untrimmed", "let skip = self.delay.min(produced);", "let skip = 0;",
         "impulse_start_tail_duration_and_channel_separation_are_preserved"),
        ("partial-tail-zeroed", "Indexing::new().partial_len(input_frames)", "Indexing::new().partial_len(0)",
         "tiny_partial_long_chunks_and_reset_equal_library_time_aligned_output"),
        ("duration-rounded-down", ".div_ceil(u128::from(self.input_rate))", "/ u128::from(self.input_rate)",
         "tiny_partial_long_chunks_and_reset_equal_library_time_aligned_output"),
        ("unbounded-input-step", "let consumed = input.len().min(need - self.input.len());", "let consumed = input.len();",
         "tiny_partial_long_chunks_and_reset_equal_library_time_aligned_output"),
        ("heap-in-processing", "let result = self.step_inner(input, eof, out);",
         "std::hint::black_box(vec![0_u8; 64]);\n        let result = self.step_inner(input, eof, out);",
         "bounded_preallocated_steps_never_allocate_after_construction"),
        ("append-capacity-unbounded", "if out.capacity().saturating_sub(out.len()) < self.output.len() {",
         "if false {", "appended_quanta_equal_individual_steps_and_respect_destination_capacity"),
    ]
    for name, before, after, test in cases:
        assert original.count(before) == 1, (name, "mutation anchor changed")
        source.write_text(original.replace(before, after))
        try:
            binary = transport.build(package, output, env, name)
            with (output / f"{name}-test.log").open("w") as log:
                status = subprocess.run([str(binary), "--exact", f"bandlimited_contracts::{test}", "--nocapture"],
                                        cwd=package, env=env, stdout=log, stderr=subprocess.STDOUT,
                                        timeout=90).returncode
            assert status == 101, (name, status, "must fail an executed Rust assertion")
        finally:
            source.write_text(original)
        print(f"actual resampler mutation {name}: executable assertion rejected", flush=True)
    return len(cases)


def main(output: Path, target: Path, cargo_home: Path, locked: Path) -> None:
    assert not output.exists(), "use a fresh result directory"
    package = output / "probe"
    (package / "src").mkdir(parents=True)
    (output / "source").mkdir()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target), CARGO_HOME=str(cargo_home), CARGO_NET_OFFLINE="true")
    manifest = '''[package]
name="migo-bandlimited-resampling-probe"
version="0.0.0"
edition="2024"
[workspace]
[dependencies]
rubato={version="=5.0.1",default-features=false,features=["fft_resampler"]}
[dev-dependencies]
'''
    manifest += f'migo-alloc-probe={{path={json.dumps(str(ROOT / "engine/testing/alloc-probe"))}}}\n'
    manifest += '[profile.release]\nopt-level=3\nlto="thin"\ncodegen-units=1\n'
    (package / "Cargo.toml").write_text(manifest)
    (package / "Cargo.lock").write_bytes(locked.read_bytes())
    source = []
    for module in ("bandlimited", "bandlimited_contracts"):
        destination = output / f"source/{module}.rs"
        shutil.copyfile(ROOT / f"scripts/testing/fixtures/audio-codec-probe/{module}.rs", destination)
        source.append(f'#[path={json.dumps(str(destination))}] mod {module};\n')
    (package / "src/lib.rs").write_text(''.join(source))
    root_inputs = [ROOT / "engine/Cargo.lock", ROOT / "engine/testing/alloc-probe/Cargo.toml"]
    root_inputs += sorted((ROOT / "engine/testing/alloc-probe/src").rglob("*.rs"))
    root_inputs += [ROOT / f"scripts/testing/fixtures/audio-codec-probe/{name}.rs" for name in ("bandlimited", "bandlimited_contracts")]
    root_inputs.append(ROOT / "scripts/testing/check_audio_resampling.py")
    checksums = {str(path.relative_to(ROOT)): codecs.sha(path) for path in root_inputs}
    for path in root_inputs:
        destination = output / "source-inputs" / path.relative_to(ROOT)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    (output / "source-inputs.json").write_text(json.dumps(checksums, indent=2) + '\n')
    for command, log in ((["rustc", "-Vv"], "rustc-version.txt"), (["cargo", "-V"], "cargo-version.txt"),
                         (["cargo", "metadata", "--offline", "--format-version", "1"], "metadata.jsonl")):
        codecs.run(command, output / log, cwd=package, env=env)
    lock = tomllib.loads((package / "Cargo.lock").read_text())["package"]
    archives = output / "upstream-archives"
    archives.mkdir()
    upstream = []
    for row in lock:
        if row.get("source", "").startswith("registry+"):
            candidates = list((cargo_home / "registry/cache").glob(f'*/{row["name"]}-{row["version"]}.crate'))
            assert len(candidates) == 1, row
            assert codecs.sha(candidates[0]) == row["checksum"], row
            shutil.copyfile(candidates[0], archives / candidates[0].name)
            upstream.append({key: row[key] for key in ("name", "version", "checksum")})
    (output / "upstream.json").write_text(json.dumps(upstream, indent=2) + '\n')
    codecs.run(["cargo", "tree", "--offline", "--locked", "-e", "normal,features"],
               output / "features.txt", cwd=package, env=env)
    binary = transport.build(package, output, env, "positive")
    codecs.run([str(binary), "bandlimited_contracts::", "--nocapture", "--test-threads=1"],
               output / "tests.log", cwd=package, env=env)
    assert "6 passed" in (output / "tests.log").read_text()
    negative = controls(package, output, env)
    assert all(codecs.sha(ROOT / path) == checksum for path, checksum in checksums.items()), "Root inputs changed during validation"
    summary = {"tests": 6, "rate_channel_length_algorithm_cases": 112,
               "partition_comparisons": 448, "compiled_negative_controls": negative,
               "rubato_version": "5.0.1", "test_binary_sha256": codecs.sha(binary),
               "root_lock_unchanged": True, "registry_archives_checksum_verified": len(upstream),
               "scope": "isolated sinc/FFT buffering, delay, exact EOF, impulse, 1kHz passband/20kHz alias at48->16kHz, per-thread Rust heap; no codec integration/device/performance acceptance"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--cargo-home", type=Path, required=True)
    parser.add_argument("--candidate-lock", type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.cargo_home.resolve(), args.candidate_lock.resolve())
