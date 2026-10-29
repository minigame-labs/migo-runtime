#!/usr/bin/env python3
"""Verify M4A in the common bounded decoder with explicit source suspension.

An index must preserve its prefetched mdat prefix; a real reposition must drop
old encoded bytes while retaining AAC history. Source waits are cancellation
points, not NeedInput retries. HTTP/version/fallback acceptance is separate.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


sequential = module("migo_mp4_adapter_sequential", ROOT / "scripts/testing/check_audio_mp4_streaming.py")
mp3 = module("migo_mp4_adapter_mp3", ROOT / "scripts/testing/check_audio_mp3_streaming.py")
ogg = module("migo_mp4_adapter_ogg", ROOT / "scripts/testing/check_audio_ogg_streaming.py")
codecs = sequential.index.codecs


def build(work: Path, env: dict, name: str) -> Path:
    package = work / "probe"
    codecs.run(["cargo", "clean", "--offline", "--locked", "-p", "migo-codec-candidate"],
               work / f"{name}-clean.log", cwd=package, env=env)
    log = work / f"{name}-build.jsonl"
    codecs.run(["cargo", "test", "--offline", "--locked", "--bin", "migo-codec-candidate",
                "--features", "simd,metadata-budgets,m4a-streaming-probe,wave-streaming-probe,adts-streaming-probe,vorbis-streaming-probe",
                "--no-run", "--message-format=json"],
               log, cwd=package, env=env)
    rows = [json.loads(line) for line in log.read_text().splitlines()
            if line.startswith('{"reason":"compiler-artifact"')]
    binaries = [row for row in rows if row.get("executable") and row["profile"]["test"]]
    assert len(binaries) == 1 and not binaries[0]["fresh"], "common decoder must actually rebuild"
    for library in ["symphonia-core", "symphonia-codec-aac", "symphonia-format-isomp4"]:
        assert any(str(work / "vendor" / f"{library}-0.6.1") in row["package_id"] for row in rows), library
    result = work / f"{name}-tests"
    shutil.copy2(binaries[0]["executable"], result)
    return result


def controls(work: Path, env: dict) -> int:
    path = work / "source/incremental_decoder.rs"
    original = path.read_bytes()
    text = original.decode()
    real = "common_m4a_partitions_and_source_resumes_preserve_whole_pcm"
    declarations = [
        ("source-wait-becomes-input", "BufferedSource::Unavailable => return Ok(Progress::NeedSource),",
         "BufferedSource::Unavailable => return Ok(Progress::NeedInput),", real),
        ("input-accepted-during-source-wait", '        if self.source_waiting {\n            return Err("source request must be resolved before accepting input".into());\n        }',
         "", real),
        ("index-resume-drops-prefetched-input", "        if reset_input {", "        if true {", real),
        ("range-resume-retains-old-input", "        if reset_input {", "        if false {",
         "common_m4a_reposition_discards_prefetched_tail_without_resetting_codec"),
        ("range-resume-resets-aac-history", "        if reset_input {",
         "        if reset_input {\n            self.decoder = None;", real),
        ("source-failure-not-latched", "        if result.is_err() {\n            self.failed = true;\n            return result;\n        }",
         "        if result.is_err() { return result; }", "common_m4a_invalid_source_resumes_latch_failure"),
        ("source-wait-precedes-cancellation", "        if self.finished {",
         "        if self.source_waiting { return Ok(Progress::NeedSource); }\n        if self.finished {",
         "common_m4a_cancellation_wins_over_pending_source_wait"),
    ]
    for name, before, after, test in declarations:
        assert text.count(before) == 1, (name, "mutation anchor changed")
        path.write_text(text.replace(before, after))
        try:
            binary = build(work, env, name)
            log = work / f"{name}-test.log"
            with log.open("w") as stream:
                status = subprocess.run([str(binary), "--exact", "isomp4_adapter_contracts::" + test, "--nocapture"],
                                        cwd=work / "probe", env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=90).returncode
            observed = log.read_text()
            assert status == 101 and "panicked at" in observed and "1 failed" in observed, (name, "must reject at an executed assertion")
        finally:
            path.write_bytes(original)
        print("actual common M4A mutation", name, ": executable assertion rejected", flush=True)
    return len(declarations)


def main(work: Path, target: Path) -> None:
    assert not work.exists(), "use a fresh common M4A result directory"
    work.mkdir(parents=True)
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true")
    env.pop("ANDROID_NDK", None)
    env.pop("ANDROID_NDK_HOME", None)
    extra = [Path(__file__).resolve(), ROOT / "scripts/bench/audio-codecs.py",
             ROOT / "engine/Cargo.lock", ROOT / "engine/Cargo.toml",
             ROOT / "engine/testing/alloc-probe/Cargo.toml",
             ROOT / "scripts/testing/check_audio_mp3_streaming.py",
             ROOT / "scripts/testing/check_audio_mp4_index.py",
             ROOT / "scripts/testing/check_audio_mp4_streaming.py"]
    extra += sorted((ROOT / "scripts/testing").glob("check_audio_*.py"))
    frozen = {str(p.relative_to(ROOT)): codecs.sha(p) for p in extra}
    codecs.make_package(ROOT, work, env, True, True)
    frozen.update(json.loads((work / "source-inputs.json").read_text()))
    for row in json.loads((work / "library-source-provenance.json").read_text()):
        for patch in (ROOT / "engine/third_party" / f'{row["crate"]}-patches').glob("*"):
            if patch.is_file():
                frozen[str(patch.relative_to(ROOT))] = codecs.sha(patch)
    # Copy the actual common implementation so all mutants remain isolated.
    fixture_root = ROOT / "scripts/testing/fixtures/audio-codec-probe"
    shutil.copytree(fixture_root, work / "source", ignore=shutil.ignore_patterns("__pycache__"))
    main_file = work / "probe/src/main.rs"
    main_file.write_text(main_file.read_text().replace(str(fixture_root), str(work / "source")))
    retained_sources = {str(p.relative_to(work)): codecs.sha(p) for p in (work / "source").rglob("*.rs")}
    (work / "frozen-root-inputs.json").write_text(json.dumps(frozen, indent=2) + "\n")
    for path in extra:
        retained = work / "source-inputs" / path.relative_to(ROOT)
        retained.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, retained)
    sequential.index.fixtures(work / "fixtures")
    sequential.large_front_fixture(work / "fixtures", work)
    env["W09_M4A_FIXTURES"] = str(work / "fixtures")
    mp3_work = work / "mp3"
    mp3_work.mkdir()
    env["W09_STREAM_FIXTURES"] = str(mp3.fixtures(mp3_work))
    ogg_work = work / "ogg"
    ogg_work.mkdir()
    ogg.fixtures(ogg_work)
    env["W09_OGG_FIXTURES"] = str(ogg_work / "fixtures")
    adts = work / "adts"
    adts.mkdir()
    env["W09_ADTS_FIXTURES"] = str(adts)
    for i, (rate, channels, frames) in enumerate([(44100, 1, 17), (48000, 2, 9737),
                                                (22050, 1, 9737), (48000, 2, 240137)]):
        source = adts / f"{i}.wav"
        codecs.fixture_pcm(source, rate, channels, frames)
        codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source), "-c:a", "aac",
                    "-profile:a", "aac_low", "-f", "adts", str(adts / f"{i}.aac")],
                   work / f"adts-{i}-encode.log", cwd=work)
    positive = build(work, env, "positive")
    groups = [("m4a", "isomp4_adapter_contracts::", 6),
              ("mp3-wave", "streaming_contracts::", 16),
              ("adts", "adts_adapter_contracts::", 3),
              ("vorbis", "vorbis_adapter_contracts::", 4)]
    for label, selected, count in groups:
        log = work / f"positive-{label}.log"
        codecs.run([str(positive), selected, "--nocapture", "--test-threads=1"], log, cwd=work / "probe", env=env)
        assert f"{count} passed; 0 failed" in log.read_text(), label
    negatives = controls(work, env)
    assert all(codecs.sha(ROOT / path) == checksum for path, checksum in frozen.items()), "Root inputs changed"
    assert all(codecs.sha(work / path) == checksum for path, checksum in retained_sources.items()), "common/test copy not restored"
    libraries = json.loads((work / "library-source-provenance.json").read_text())
    for row in libraries:
        assert sequential.index.timeline.source_tree(work / "vendor" / f'{row["crate"]}-{row["version"]}') == row["source_files"]
    summary = {"common_m4a_tests": 6, "mp3_common_regression_tests": 8,
               "wave_common_regression_tests": 8, "adts_common_regression_tests": 3,
               "vorbis_common_regression_tests": 4, "positive_tests_total": 29,
               "m4a_pcm_partition_source_comparisons": 120, "reposition_prefetch_comparisons": 2,
               "compiled_common_controls": negatives,
               "test_binary_sha256": codecs.sha(positive), "libraries": len(libraries),
               "root_inputs_unchanged": True, "source_copies_restored": True,
               "root_lock_sha256": codecs.sha(ROOT / "engine/Cargo.lock"),
               "scope": "Common bounded decoder/source suspension over fixture bytes; not actual HTTP Range/version checks, DSP/player/fallback/product/native/device/performance acceptance"}
    (work / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
