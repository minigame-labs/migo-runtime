#!/usr/bin/env python3
"""W09 isolated borrowed-packet MP3 streaming correctness/allocation probe.

The old streaming path maintained minimp3 framing/lookahead/EOF workarounds and
left codec delay/padding in output. Exercise the library push interface before
product selection; no blocking Read or live network is used in this probe.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import wave

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("timeline", ROOT / "scripts/testing/check_audio_codec_timeline.py")
timeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timeline)
codecs = timeline.codecs


def fixtures(output: Path) -> Path:
    directory = output / "fixtures"
    directory.mkdir()
    cases = [("cbr-44100-1", 44100, 1, 8957, ["-b:a", "128k"]),
             ("vbr-48000-2", 48000, 2, 9737, ["-q:a", "4"]),
             ("mpeg2-22050-1", 22050, 1, 4731, ["-b:a", "64k"]),
             ("mpeg25-8000-2", 8000, 2, 1937, ["-b:a", "32k"]),
             ("no-xing", 44100, 1, 8957, ["-b:a", "128k", "-write_xing", "0"]),
             ("no-xing-vbr", 48000, 2, 240137, ["-q:a", "4", "-write_xing", "0"]),
             ("tiny", 48000, 1, 17, ["-b:a", "128k"]),
             ("long", 48000, 2, 240137, ["-b:a", "128k"])]
    for name, rate, channels, frames, options in cases:
        source = directory / f"{name}.wav"
        codecs.fixture_pcm(source, rate, channels, frames)
        codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source),
                    "-c:a", "libmp3lame", *options, str(directory / f"{name}.mp3")],
                   directory / f"{name}-encode.log", cwd=output)
    codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
                "anullsrc=r=8000:cl=mono", "-t", "2200", "-c:a", "libmp3lame", "-b:a", "8k",
                str(directory / "duration-over-cache-cap.mp3")], directory / "long-duration-encode.log", cwd=output)
    return directory


def controls(cohort: Path, output: Path, env: dict) -> int:
    declarations = [
        ("packet-heap-copy", "vendor/symphonia-bundle-mp3-0.6.1/src/streaming.rs",
         "        let track = self.track.as_ref().expect(\"initialized with signature\");",
         "        std::hint::black_box(input[..size].to_vec());\n        let track = self.track.as_ref().expect(\"initialized with signature\");",
         "steady_push_packetize_decode_convert_has_no_heap_allocation"),
        ("input-capacity-removed", "source/incremental_decoder.rs",
         "let accepted = data.len().min(ENCODED_CAPACITY - self.input.len());", "let accepted = data.len();",
         "input_and_work_are_bounded_and_cancellation_is_sticky"),
        ("cancel-check-removed", "source/incremental_decoder.rs",
         "if self.cancelled || cancel.load(Ordering::Acquire) {", "if false {",
         "input_and_work_are_bounded_and_cancellation_is_sticky"),
        ("step-budget-removed", "source/incremental_decoder.rs",
         "if packets >= self.packet_budget\n                || bytes >= P::ENCODED_BYTES_PER_STEP\n                || scanned >= WORK_BYTES_PER_STEP\n            {\n                return Ok(Progress::Yield);\n            }", "",
         "input_and_work_are_bounded_and_cancellation_is_sticky"),
        ("gapless-disabled", "source/streaming.rs",
         "AudioDecoderOptions::default().gapless(true)", "AudioDecoderOptions::default().gapless(false)",
         "arbitrary_chunks_and_eof_equal_the_whole_file_decoder"),
        ("footer-not-skipped", "vendor/symphonia-metadata-0.6.1/src/id3v2/mod.rs",
         "Ok(header.size + 10 + if header.has_footer { 10 } else { 0 })", "Ok(header.size + 10)",
         "id3_footer_is_in_the_metadata_extent"),
        ("estimated-frame-count-as-trim", "vendor/symphonia-bundle-mp3-0.6.1/src/demuxer.rs",
         "track.with_duration(Duration::new(n_mpeg_frames * u64::from(header.num_frames())));",
         "track.with_num_frames(n_mpeg_frames * u64::from(header.num_frames()));",
         "arbitrary_chunks_and_eof_equal_the_whole_file_decoder"),
        ("cache-quota-imposed-on-stream", "source/incremental_decoder.rs", "            cap,",
         "            cap: Some(cap.unwrap_or(crate::limits::MAX_AUDIO_PCM_SAMPLES)),",
         "long_music_has_resident_limits_without_a_default_duration_limit"),
        ("failed-step-leaks-pcm", "source/incremental_decoder.rs",
         "if result.is_err() {\n            self.failed = true;\n            out.clear();\n        }",
         "if result.is_err() { self.failed = true; }",
         "malformed_truncated_and_over_budget_inputs_return_errors"),
        ("metadata-budget-removed", "vendor/symphonia-bundle-mp3-0.6.1/src/streaming.rs",
         ".filter(|total| *total <= self.options.max_metadata_bytes)", ".filter(|_| true)",
         "malformed_truncated_and_over_budget_inputs_return_errors"),
        ("sync-budget-removed", "vendor/symphonia-bundle-mp3-0.6.1/src/streaming.rs",
         ".filter(|total| *total <= self.options.max_junk_bytes)", ".filter(|_| true)",
         "malformed_truncated_and_over_budget_inputs_return_errors"),
    ]
    for name, file, before, after, test in declarations:
        directory = output / name
        directory.mkdir()
        for folder in ("probe", "vendor", "source"):
            shutil.copytree(cohort / folder, directory / folder)
        for path in [directory / "probe/Cargo.toml", *(directory / "probe").rglob("*.rs")]:
            path.write_text(path.read_text().replace(str(cohort), str(directory)))
        target = directory / file
        text = target.read_text()
        assert text.count(before) == 1, f"streaming mutation anchor changed: {name}"
        target.write_text(text.replace(before, after, 1))
        binary = timeline.executable_tests(directory / "probe", directory, env,
                                            features="simd,streaming-probe,metadata-budgets")
        log = directory / "observed-red.log"
        with log.open("w") as stream:
            status = subprocess.run([str(binary), "--exact", f"streaming_contracts::{test}", "--nocapture"],
                                    env=env, stdout=stream, stderr=subprocess.STDOUT, timeout=120)
        text = log.read_text()
        assert status.returncode == 101 and "1 failed" in text and "panicked at" in text, (name, text[-1500:])
        print(f"actual streaming mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def independent_oracle(cohort: Path, output: Path) -> list[dict]:
    output.mkdir()
    rows = []
    binary = cohort / "codec-probe"
    for source in sorted((cohort / "fixtures").glob("*.mp3")):
        # The intentionally long declaration tests resident buffering using only
        # its first quantum; full-buffer loading should reject its total quota.
        if source.stem == "duration-over-cache-cap":
            continue
        case = output / source.stem
        case.mkdir()
        actual = codecs.probe(binary, "candidate", source, case)
        assert actual["ok"], actual
        oracle = case / "ffmpeg.f32le"
        codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source), "-f", "f32le",
                    "-c:a", "pcm_f32le", str(oracle)], case / "ffmpeg.log", cwd=case)
        decoded = codecs.floats(case / "candidate.f32le")
        reference = codecs.floats(oracle)
        assert len(decoded) == len(reference), (source.name, len(decoded), len(reference))
        assert all(math.isfinite(value) for value in reference)
        noise = sum((float(a)-float(b))**2 for a, b in zip(decoded, reference))
        energy = sum(float(value)**2 for value in reference)
        snr = 10 * math.log10(energy / noise) if energy and noise else None
        if energy:
            assert snr is None or snr > 80, (source.name, snr)
        else:
            assert decoded == reference, "silent reference must remain silent"
        with wave.open(str(source.with_suffix(".wav")), "rb") as wav:
            assert actual["sample_rate"] == wav.getframerate()
            assert actual["channels"] == wav.getnchannels()
            source_frames = wav.getnframes()
        rows.append({"case": source.stem, "source_frames": source_frames,
                     "decoded_frames": actual["frames"], "matches_source_frames": source_frames == actual["frames"],
                     "same_ffmpeg_frames": True, "prefix_snr_db": snr,
                     "binary_sha256": codecs.sha(binary), "fixture_sha256": codecs.sha(source)})
    assert len(rows) == 8, "oracle corpus did not execute"
    (output / "summary.json").write_text(json.dumps(rows, indent=2) + "\n")
    return rows


def main(output: Path, target: Path, *, run_controls: bool = True) -> None:
    assert not output.exists(), "use a fresh streaming result directory"
    output.mkdir(parents=True)
    original_lock = (ROOT / "engine/Cargo.lock").read_bytes()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    codecs.run(["ffmpeg", "-version"], output / "ffmpeg-version.txt", cwd=output)
    fixtures_directory = fixtures(output)
    binary = codecs.make_package(ROOT, output, env, True, True, False, False, True)
    # Freeze the adapter/test source paths in this retained isolated package, so
    # later source controls cannot accidentally compile the root's positive copy.
    source = output / "source"
    source.mkdir()
    for name in ("streaming.rs", "streaming_contracts.rs", "incremental_decoder.rs", "wave_streaming.rs"):
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe" / name, source / name)
    main_rs = output / "probe/src/main.rs"
    main_rs.write_text(main_rs.read_text().replace(str(ROOT / "scripts/testing/fixtures/audio-codec-probe/streaming.rs"),
                                                str(source / "streaming.rs"))
                      .replace(str(ROOT / "scripts/testing/fixtures/audio-codec-probe/streaming_contracts.rs"),
                               str(source / "streaming_contracts.rs")))
    env["W09_STREAM_FIXTURES"] = str(fixtures_directory)
    tests = timeline.executable_tests(output / "probe", output, env, features="simd,streaming-probe,metadata-budgets")
    codecs.run([str(tests), "streaming_contracts::", "--nocapture", "--test-threads=1"],
               output / "tests.log", cwd=output, env=env)
    assert "8 passed" in (output / "tests.log").read_text(), "streaming tests did not execute"
    provenance = output / "verification"
    provenance.mkdir()
    timeline.provenance(output, provenance)
    independent_oracle(output, output / "independent-oracle")
    count = controls(output, output, env) if run_controls else 0
    assert (ROOT / "engine/Cargo.lock").read_bytes() == original_lock
    summary = {"production_lock_unchanged": True, "compiled_stream_mutants": count,
               "mutation_controls_executed": run_controls,
               "contract_tests": 8, "fixture_files": 9, "binary_sha256": codecs.sha(binary),
               "test_binary_sha256": codecs.sha(tests), "production_adoption_ready": False,
               "scope": __doc__}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--positive-only", action="store_true",
                        help="run semantic/oracle/provenance regression without repeating unrelated source mutations")
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), run_controls=not args.positive_only)
