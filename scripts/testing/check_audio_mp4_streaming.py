#!/usr/bin/env python3
"""Validate M4A sequential library packets and explicit IO action protocol.

This is not HTTP Range/fallback or Migo player acceptance. Replay the bounded
index gate first, then exercise real split input and compiled packetizer faults.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("migo_mp4_index_gate", ROOT / "scripts/testing/check_audio_mp4_index.py")
index = importlib.util.module_from_spec(spec)
spec.loader.exec_module(index)


def large_front_fixture(fixtures: Path, output: Path) -> None:
    # FFmpeg remux can discard the large test title. Construct the actual
    # optional atom in the test fixture and relocate its real stco entries.
    atom = lambda kind, body: struct.pack(">I4s", len(body) + 8, kind) + body
    source = (fixtures / "1-front.m4a").read_bytes()
    movie = index.timeline.payloads(source, (b"moov",))[0]
    meta = atom(b"meta", bytes(4) + atom(b"ilst", atom(b"\xa9nam", atom(
        b"data", struct.pack(">II", 1, 0) + b"x" * (1024 * 1024)))))
    changed = index.timeline.rewrite(source, {(b"moov",): movie + atom(b"udta", meta)})
    delta = len(changed) - len(source)
    path = (b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stco")
    table = index.timeline.payloads(source, path)[0]
    count = int.from_bytes(table[4:8], "big")
    offsets = struct.unpack(">" + str(count) + "I", table[8:])
    table = table[:8] + b"".join(struct.pack(">I", value + delta) for value in offsets)
    changed = index.timeline.rewrite(changed, {path: table})
    target = fixtures / "1-metadata-front.m4a"
    target.write_bytes(changed)
    movie_bytes = len(index.timeline.payloads(changed, (b"moov",))[0]) + 8
    assert movie_bytes > 1024 * 1024, "large moov oracle must actually be large"
    hashes = []
    for file in [fixtures / "1-front.m4a", target]:
        result = subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(file), "-f", "f32le", "-"],
                                capture_output=True, check=True, timeout=30)
        hashes.append(hashlib.sha256(result.stdout).hexdigest())
    assert hashes[0] == hashes[1], "metadata fixture relocation changed independent FFmpeg PCM"
    (output / "front-metadata-oracle.json").write_text(json.dumps({
        "moov_bytes": movie_bytes, "encoded_bytes": len(changed), "ffmpeg_pcm_sha256": hashes[0],
        "same_as_clean": True,
    }, indent=2) + "\n")


def controls(work: Path, output: Path, env: dict) -> int:
    source = work / "vendor/symphonia-format-isomp4-0.6.1/src/streaming/sequential.rs"
    original = source.read_bytes()
    text = original.decode()
    real = "sequential_partitions_and_explicit_index_range_actions_preserve_exact_pcm"
    tails = "truncated_atoms_samples_and_terminal_tails_never_become_successful_end"
    metadata = "large_optional_metadata_is_bounded_and_cold_atom_storage_is_released"
    wait = "index_and_range_resume_failures_are_sticky_and_preserve_request_identity"
    declarations = [
        ("waits-for-future-media", "        loop {\n            match self.stage {",
         "        if input.len() < 8192 && !eof { return Ok(PacketStep::NeedMore); }\n        loop {\n            match self.stage {",
         "first_valid_packet_does_not_wait_for_remaining_media"),
        ("sample-crosses-mdat", "if span.end.is_some_and(|limit| end > limit) {", "if false {", tails),
        ("object-tail-ignored", ".is_some_and(|len| len != self.position)", ".is_some_and(|_| false)", tails),
        ("discard-step-unbounded", "const DISCARD_STEP: usize = 4096;", "const DISCARD_STEP: usize = 2 * 1024 * 1024;", metadata),
        ("index-wait-advances", "Stage::AwaitIndex(request) => return Ok(PacketStep::NeedIndex(request)),",
         "Stage::AwaitIndex(request) => { self.position += 1; return Ok(PacketStep::NeedIndex(request)); },", real),
        ("range-wait-loses-packet", "return Ok(PacketStep::ReadAt(packet));",
         "self.pending = None; return Ok(PacketStep::ReadAt(packet));", real),
        ("cold-index-storage-retained", "self.scratch = Vec::new();", "self.scratch.clear();", metadata),
        ("external-object-length-ignored", "&& index.options.object_len == self.options.index.object_len", "", wait),
        ("range-kind-unchecked", "media.kind == AtomKind::MediaData", "true", wait),
        ("failure-not-sticky", "        if result.is_err() {\n            self.failed = true;\n        }", "", wait),
        ("declared-index-cap-unchecked", ".filter(|n| *n <= cap as u64)", ".filter(|_| true)",
         "index_budget_rejects_announced_atom_before_buffering_its_body"),
        ("packet-payload-copied", "        let borrowed = packet.packet(",
         "        std::hint::black_box(input[..packet.size as usize].to_vec());\n        let borrowed = packet.packet(",
         "sequential_packet_decode_and_convert_has_no_steady_rust_allocation"),
        ("metadata-skip-quota-ignored", ".filter(|n| *n <= self.options.max_skipped_metadata_bytes)", ".filter(|_| true)", metadata),
        ("range-resume-off-by-one", "self.position = offset;", "self.position = offset + 1;", real),
    ]
    for name, before, _, _ in declarations:
        assert text.count(before) == 1, (name, "mutation anchor changed")
    for name, before, after, test in declarations:
        source.write_text(text.replace(before, after))
        try:
            binary = index.build(work, output, env, "sequential-" + name, "migo-mp4-index-probe",
                                 mutated="symphonia-format-isomp4", test_target="front")
            log = output / f"sequential-{name}-test.log"
            with log.open("w") as stream:
                result = subprocess.run([str(binary), "--exact", test, "--nocapture"],
                                        cwd=work, env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=90)
            observed = log.read_text()
            assert result.returncode == 101 and "panicked at" in observed and "1 failed" in observed, (name, "must reject in an executed Rust assertion")
        finally:
            source.write_bytes(original)
        print(f"actual M4A sequential mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def main(output: Path, target: Path) -> None:
    assert not output.exists(), "use a fresh sequential result directory"
    output.mkdir(parents=True)
    extra = [Path(__file__).resolve(), ROOT / "scripts/testing/fixtures/audio-codec-probe/isomp4_streaming_contracts.rs"]
    frozen = {str(p.relative_to(ROOT)): index.codecs.sha(p) for p in extra}
    work = output / "index"
    index.main(work, target)
    frozen.update(json.loads((work / "source-inputs.json").read_text()))
    (output / "source-inputs.json").write_text(json.dumps(frozen, indent=2) + "\n")
    large_front_fixture(work / "fixtures", output)
    shutil.copy2(ROOT / "scripts/testing/fixtures/audio-codec-probe/isomp4_streaming_contracts.rs", work / "probe/tests/front.rs")
    shutil.copy2(ROOT / "scripts/testing/fixtures/audio-codec-probe/isomp4_index_contracts.rs", work / "probe/tests/isomp4_index_contracts.rs")
    retained_tests = {str(p.relative_to(work)): index.codecs.sha(p) for p in (work / "probe/tests").glob("*.rs")}
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    env.pop("ANDROID_NDK", None)
    env.pop("ANDROID_NDK_HOME", None)
    binary = index.build(work, output, env, "positive-sequential", "migo-mp4-index-probe", test_target="front")
    log = output / "positive-sequential-tests.log"
    index.codecs.run([str(binary), "--nocapture", "--test-threads=1"], log, cwd=work, env=env)
    assert "15 passed; 0 failed" in log.read_text(), "eight sequential and seven common index groups must execute"
    negatives = controls(work, output, env)
    for row in json.loads((work / "library-source-provenance.json").read_text()):
        selected = work / "vendor" / f'{row["crate"]}-{row["version"]}'
        assert index.timeline.source_tree(selected) == row["source_files"], "mutated vendor copy not restored"
    assert all(index.codecs.sha(work / path) == checksum for path, checksum in retained_tests.items()), "test sources changed"
    assert all(index.codecs.sha(ROOT / path) == checksum for path, checksum in frozen.items()), "Root inputs changed"
    summary = {"sequential_tests": 8, "common_index_tests": 7, "pcm_partition_comparisons": 60,
               "sequential_compiled_controls": negatives, "index_compiled_controls": 15,
               "compiled_controls_total": 15 + negatives,
               "test_binary_sha256": index.codecs.sha(binary), "index_summary": "index/summary.json",
               "library_sources_restored": True, "root_inputs_unchanged": True,
               "root_lock_sha256": index.codecs.sha(ROOT / "engine/Cargo.lock"),
               "scope": "library packet/codec and explicit NeedIndex/ReadAt protocol over fixture bytes; no actual HTTP Range/fallback, common Migo adapter/player/product/native/device/performance acceptance"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
