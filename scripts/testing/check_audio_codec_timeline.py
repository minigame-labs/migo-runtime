#!/usr/bin/env python3
"""W09 isolated AAC timeline acceptance and executable source mutations.

Published Symphonia ignored elst, AAC ignored packet trims, and reading to EOF
destroyed the MP4 raw-read anchor. Test actual patched libraries and containers;
do not substitute arithmetic-only checks or FFmpeg's padded raw output for them.
This is a prototype gate, not production, native-device or performance approval.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("audio_codecs", ROOT / "scripts/bench/audio-codecs.py")
codecs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(codecs)


def payloads(data: bytes, path: tuple[bytes, ...]) -> list[bytes]:
    result = []
    pos = 0
    while pos < len(data):
        size, tag = struct.unpack_from(">I4s", data, pos)
        assert 8 <= size <= len(data) - pos, "test fixture uses ordinary bounded boxes"
        if tag == path[0]:
            payload = data[pos + 8:pos + size]
            result.extend(payloads(payload, path[1:]) if len(path) > 1 else [payload])
        pos += size
    return result


def rewrite(data: bytes, changes: dict[tuple[bytes, ...], bytes], prefix=()) -> bytes:
    """Test-only box surgery. The product still uses the library's sole parser.

    Fixtures have mdat before moov; editing moov cannot move encoded data or
    invalidate stco. Assert that layout explicitly before every transformed case.
    """
    result = bytearray()
    pos = 0
    containers = {b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts"}
    while pos < len(data):
        size, tag = struct.unpack_from(">I4s", data, pos)
        assert 8 <= size <= len(data) - pos
        path = (*prefix, tag)
        payload = data[pos + 8:pos + size]
        if path in changes:
            payload = changes[path]
        elif tag in containers:
            payload = rewrite(payload, changes, path)
        result.extend(struct.pack(">I4s", len(payload) + 8, tag) + payload)
        pos += size
    return bytes(result)


ELST = (b"moov", b"trak", b"edts", b"elst")
MDHD = (b"moov", b"trak", b"mdia", b"mdhd")
STTS = (b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stts")


def edit(duration: int, start: int, *, version=0, rate=1, fraction=0) -> bytes:
    fields = struct.pack(">Qqhh" if version == 1 else ">Iihh", duration, start, rate, fraction)
    return bytes([version, 0, 0, 0]) + struct.pack(">I", 1) + fields


def source_tree(path: Path) -> dict[str, str]:
    return {str(p.relative_to(path)): codecs.sha(p) for p in sorted(path.rglob("*")) if p.is_file()}


def provenance(cohort: Path, output: Path) -> None:
    recorded = json.loads((cohort / "library-source-provenance.json").read_text())
    expected = {row["crate"]: row["source_files"] for row in recorded}
    for row in recorded:
        for patch in row["patches"]:
            assert codecs.sha(ROOT / patch["file"]) == patch["sha256"], "patch changed since probe build"
        selected = cohort / "vendor" / f'{row["crate"]}-{row["version"]}'
        assert source_tree(selected) == expected[row["crate"]], "unrecorded library source drift"
    with tempfile.TemporaryDirectory(prefix="migo-codec-replay-", dir="/tmp") as temporary:
        scratch = Path(temporary)
        replay = codecs.library_patches(ROOT, scratch)
        for name, selected in replay.items():
            assert source_tree(selected) == expected[name], "patch replay differs"
            target = selected / {"symphonia-codec-aac": "src/aac/mod.rs",
                                 "symphonia-format-isomp4": "src/demuxer.rs",
                                 "symphonia-bundle-mp3": "src/demuxer.rs",
                                 "symphonia-metadata": "src/id3v2/mod.rs",
                                 "symphonia-core": "src/packet.rs",
                                 "symphonia-format-ogg": "src/demuxer.rs",
                                 "symphonia-format-riff": "src/wave/mod.rs",
                                 "symphonia-codec-pcm": "src/lib.rs",
                                 "symphonia-codec-vorbis": "src/lib.rs"}[name]
            target.write_text(target.read_text() + "\n// unrecorded drift\n")
            assert source_tree(selected) != expected[name], "source-drift negative control escaped"
    # The actual Cargo artifacts must select these copies, not the registry crate.
    artifacts = [json.loads(line) for line in (cohort / "build.jsonl").read_text().splitlines()
                 if line.startswith('{"reason":"compiler-artifact"')]
    for name in expected:
        assert any(str(cohort / "vendor" / f"{name}-0.6.1") in row["package_id"] for row in artifacts), name
    (output / "source-provenance.json").write_text(json.dumps({
        "published_plus_patch_replay": True, "unrecorded_drift_controls": len(expected),
        "actual_cargo_path_selection": True,
    }, indent=2) + "\n")


def container_cases(binary: Path, output: Path) -> tuple[Path, Path, int, int]:
    cases = []
    positive = negative = 0
    seek_fixture = pns_fixture = None
    for rate, channels, frames in [(32000, 1, 6437), (44100, 1, 8957), (48000, 2, 9738)]:
        base = output / f"aac-{rate}-{channels}"
        base.mkdir()
        source = base / "source.wav"
        codecs.fixture_pcm(source, rate, channels, frames)
        encoded = base / "exact-timeline.m4a"
        codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source), "-c:a", "aac",
                    "-aac_pns", "0", "-b:a", "128k", "-movie_timescale", str(rate), str(encoded)],
                   base / "encode.log", cwd=output)
        data = encoded.read_bytes()
        assert data.index(b"mdat") < data.index(b"moov")
        baseline = codecs.probe(binary, "candidate", encoded, base)
        assert baseline["ok"] and baseline["frames"] == frames, baseline
        baseline_pcm = codecs.floats(base / "candidate.f32le")
        if rate == 48000:
            seek_fixture = encoded
            pns_fixture = base / "pns.m4a"
            codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source), "-c:a", "aac",
                        "-aac_pns", "1", "-b:a", "128k", "-movie_timescale", str(rate), str(pns_fixture)],
                       base / "pns-encode.log", cwd=output)
        media_duration = struct.unpack_from(">I", payloads(data, MDHD)[0], 16)[0]
        # Empty table is distinct from an empty edit (media_time=-1).
        noedit = {ELST: struct.pack(">II", 0, 0)}
        raw_directory = base / "no-edit"
        raw_directory.mkdir()
        raw_file = raw_directory / "input.m4a"
        raw_file.write_bytes(rewrite(data, noedit))
        raw = codecs.probe(binary, "candidate", raw_file, raw_directory)
        assert raw["ok"] and raw["frames"] == media_duration, raw
        raw_pcm = codecs.floats(raw_directory / "candidate.f32le")
        assert baseline_pcm == raw_pcm[1024 * channels:(1024 + frames) * channels]
        good = {
            "nonstandard-delay-211": (edit(5000, 211), 211, 5000),
            "multi-packet-delay-1537": (edit(5000, 1537), 1537, 5000),
            "version-one": (edit(frames, 1024, version=1), 1024, frames),
            "clamp-edit-to-media-end": (edit(frames + rate, 1024), 1024, frames),
        }
        for name, (elst, start, count) in good.items():
            directory = base / name
            directory.mkdir()
            file = directory / "input.m4a"
            file.write_bytes(rewrite(data, {ELST: elst}))
            actual = codecs.probe(binary, "candidate", file, directory, cap=count * channels)
            assert actual["ok"] and actual["frames"] == count, actual
            assert codecs.floats(directory / "candidate.f32le") == raw_pcm[start * channels:(start + count) * channels]
            cases.append({"name": name, "rate": rate, "frames": count, "exact_pcm_slice": True})
            positive += 1
        invalid = {
            "empty-edit": (edit(100, -1), "empty audio edits"),
            "negative-media-time": (edit(100, -2), "negative audio edit"),
            "zero-rate": (edit(100, 0, rate=0), "rate must be one"),
            "fractional-rate": (edit(100, 0, fraction=1), "rate must be one"),
            "multiple-edits": (struct.pack(">II", 0, 2) + edit(10, 0)[8:] * 2, "multiple audio edits"),
            "outside-media": (edit(100, media_duration), "presentation window"),
            "huge-v1-duration": (edit(2**64 - 1, 1024, version=1), "overflow"),
            "huge-v1-offset": (edit(100, 2**40, version=1), "presentation window"),
            "invalid-version": (bytes([2, 0, 0, 0]) + edit(100, 0)[4:], "version"),
            "overclaimed-count": (struct.pack(">II", 0, 2**32 - 1) + edit(100, 0)[8:], ""),
            "truncated-entry": (edit(100, 0)[:-5], ""),
        }
        for name, (elst, message) in invalid.items():
            directory = base / name
            directory.mkdir()
            file = directory / "input.m4a"
            file.write_bytes(rewrite(data, {ELST: elst}))
            actual = codecs.probe(binary, "candidate", file, directory)
            assert not actual["ok"] and message in actual["error"], actual
            cases.append({"name": name, "rate": rate, "error": actual["error"]})
            negative += 1
        if rate == 48000:
            mdhd = bytearray(payloads(data, MDHD)[0])
            struct.pack_into(">II", mdhd, 12, rate // 2, media_duration // 2)
            stts = bytearray(payloads(data, STTS)[0])
            for offset in range(12, len(stts), 8):
                delta = struct.unpack_from(">I", stts, offset)[0]
                assert delta % 2 == 0
                struct.pack_into(">I", stts, offset, delta // 2)
            directory = base / "media-timescale-half"
            directory.mkdir()
            file = directory / "input.m4a"
            file.write_bytes(rewrite(data, {ELST: edit(frames, 512), MDHD: bytes(mdhd), STTS: bytes(stts)}))
            actual = codecs.probe(binary, "candidate", file, directory)
            assert actual["ok"] and actual["frames"] == frames, actual
            assert codecs.floats(directory / "candidate.f32le") == baseline_pcm
            cases.append({"name": "media-timescale-half", "rate": rate, "exact_pcm_slice": True})
            positive += 1
        positive += 2  # Generated exact-timeline and absent-edit cases.
    (output / "container-results.json").write_text(json.dumps(cases, indent=2) + "\n")
    return seek_fixture, pns_fixture, positive, negative


def executable_tests(package: Path, output: Path, env: dict, *, library=False,
                     library_package: str | None = None, features: str = "simd,metadata-budgets",
                     release: bool = True) -> Path:
    command = ["cargo", "test", "--offline", "--locked", "--no-run", "--message-format=json"]
    if release:
        command.append("--release")
    selected_library = library_package or ("symphonia-format-isomp4" if library else None)
    if selected_library == "symphonia-core":
        command += ["--test", "packet-core-contracts", "--features", features]
    elif selected_library:
        command += ["-p", selected_library, "--lib"]
    else:
        command += ["--bin", "migo-codec-candidate", "--features", features]
    codecs.run(command, output / "test-build.jsonl", cwd=package, env=env)
    rows = []
    for line in (output / "test-build.jsonl").read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable") and row["profile"]["test"]:
            rows.append(row["executable"])
    assert len(rows) == 1, rows
    retained = output / "contract-tests"
    shutil.copy2(rows[0], retained)
    return retained


def mutations(cohort: Path, output: Path, env: dict, *, sample_tables_only: bool = False) -> int:
    source = cohort / "vendor"
    declarations = [
        ("missing-aac-trim", "symphonia-codec-aac", "src/aac/mod.rs", "            self.buf.trim(start, end);", "",
         "timeline_contracts::actual_packets_and_decoder_agree_on_trim_units_and_total_frames", False),
        ("fixed-delay", "symphonia-format-isomp4", "src/audio_timeline.rs",
         "start = scale(edit.media_time as u64, media_scale, rate, true)?;", "start = 1024u64;",
         "audio_timeline::tests::nonstandard_delay_partial_packet_and_edit_end", True),
        ("missing-tail-trim", "symphonia-format-isomp4", "src/audio_timeline.rs",
         "let trim_end = self.block_frames - trim_start - valid;", "let trim_end = 0;",
         "audio_timeline::tests::delay_and_short_last_packet_come_from_container", True),
        ("missing-seek-map", "symphonia-format-isomp4", "src/demuxer.rs", "timeline.seek_media_ts(ts)?", "ts",
         "timeline_contracts::seek_with_preroll_matches_the_corresponding_full_decode", False),
        ("missing-pns-reset", "symphonia-codec-aac", "src/aac/cpe.rs", "        self.lcg = Lcg::new(0x1f2e3d4c);", "",
         "timeline_contracts::restarting_a_pns_stream_is_deterministic_and_clears_output", False),
        ("consume-eof-anchor", "symphonia-format-isomp4", "src/demuxer.rs",
         "                if self.segs.last().is_some_and(|seg| seg.all_tracks_ended()) {\n                    return Ok(None);\n                }", "",
         "timeline_contracts::seek_with_preroll_matches_the_corresponding_full_decode", False),
        ("old-packet-duration", "symphonia-core", "src/packet.rs",
         "let dur = block_dur.saturating_sub(trim_start).saturating_sub(trim_end);",
         "let dur = block_dur.saturating_sub(self.trim_start).saturating_sub(self.trim_end);",
         "timeline_contracts::mp3_packet_valid_duration_matches_gapless_decoder_output", False),
        ("unbounded-packet-tail", "symphonia-core", "src/packet.rs",
         ".max(0).min(block_len - start)", ".max(0)",
         "packet::tests::trimmed_duration_is_valid_frames_for_both_owned_and_borrowed_packets", "symphonia-core"),
        ("signed-duration-overflow", "symphonia-core", "src/packet.rs",
         "let start = (-block_start).max(0).min(block_len);",
         "let start = i128::from(pts.0.duration_to(Timestamp::ZERO).unwrap_or(Duration::ZERO).get()).min(block_len);",
         "packet::tests::trimmed_duration_is_valid_frames_for_both_owned_and_borrowed_packets", "symphonia-core"),
        ("chunk-group-start-exclusive", "symphonia-format-isomp4", "src/atoms/stsc.rs",
         "if entry.first_sample <= sample_num {", "if entry.first_sample < sample_num {",
         "streaming_index_boundary_contracts::chunk_groups_include_their_first_sample", True),
        ("time-table-offset-overflow", "symphonia-format-isomp4", "src/atoms/stts.rs",
         "let entry_sample_offset = sample_num - (next_entry_first_sample - entry.sample_count);",
         "let entry_sample_offset = sample_num + entry.sample_count - next_entry_first_sample;",
         "streaming_index_boundary_contracts::time_table_arithmetic_handles_valid_large_sample_indices", True),
        ("time-table-count-overflow", "symphonia-format-isomp4", "src/atoms/stts.rs",
         '            total_samples = match total_samples.checked_add(sample_count) {\n'
         '                Some(total) => total,\n'
         '                None => return decode_error("isomp4 (stts): sample count overflow"),\n'
         '            };', "",
         "streaming_index_boundary_contracts::sample_count_overflow_is_rejected_before_index_use", True),
    ]
    if sample_tables_only:
        selected = {"chunk-group-start-exclusive", "time-table-offset-overflow", "time-table-count-overflow"}
        declarations = [row for row in declarations if row[0] in selected]
        assert len(declarations) == len(selected)
    for name, crate, file, before, after, test, library in declarations:
        directory = output / name
        directory.mkdir()
        shutil.copytree(source, directory / "vendor")
        shutil.copytree(cohort / "probe", directory / "probe")
        manifest = directory / "probe/Cargo.toml"
        manifest.write_text(manifest.read_text().replace(str(source), str(directory / "vendor")))
        for generated in (directory / "probe").rglob("*.rs"):
            generated.write_text(generated.read_text().replace(str(source), str(directory / "vendor")))
        target = directory / "vendor" / f"{crate}-0.6.1" / file
        text = target.read_text()
        assert text.count(before) == 1, f"mutation anchor changed: {name}"
        target.write_text(text.replace(before, after, 1))
        binary = executable_tests(directory / "probe", directory, env,
                                  library=library if isinstance(library, bool) else False,
                                  library_package=library if isinstance(library, str) else None,
                                  # The old offset expression wraps to the right value in
                                  # unchecked release builds but panics with overflow checks.
                                  # Exercise that real supported build instead of claiming
                                  # the mutation corrupts release PCM.
                                  release=name != "time-table-offset-overflow")
        with (directory / "observed-red.log").open("w") as stream:
            status = subprocess.run([str(binary), "--exact", test, "--nocapture"], env=env,
                                    stdout=stream, stderr=subprocess.STDOUT, timeout=30)
        text = (directory / "observed-red.log").read_text()
        assert status.returncode == 101 and "1 failed" in text and "panicked at" in text, (name, text[-2000:])
        print(f"actual library mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def main(cohort: Path, output: Path, target: Path, *, run_controls: bool = True,
         sample_tables_only: bool = False) -> None:
    assert not output.exists(), "use a fresh timeline result directory"
    output.mkdir(parents=True)
    lock = (ROOT / "engine/Cargo.lock").read_bytes()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    provenance(cohort, output)
    # The executable was copied from the actual positive Cargo artifact before
    # the budget mutants were built. Never select a target binary by mtime.
    binary = cohort / "codec-probe"
    assert codecs.sha(binary) == json.loads((cohort / "summary.json").read_text())["sha256"]
    seek, pns, positive, negative = container_cases(binary, output)
    env.update(W09_M4A_FIXTURE=str(seek), W09_M4A_PNS_FIXTURE=str(pns),
               W09_MP3_FIXTURE=str(cohort / "mp3-cbr-44100-1/encoded.mp3"))
    tests = executable_tests(cohort / "probe", output, env)
    codecs.run([str(tests), "timeline_contracts::", "--nocapture"], output / "contracts.log", cwd=output, env=env)
    unit_directory = output / "library-unit-tests"
    unit_directory.mkdir()
    units = executable_tests(cohort / "probe", unit_directory, env, library=True)
    codecs.run([str(units)], unit_directory / "tests.log", cwd=output, env=env)
    assert "11 passed; 0 failed" in (unit_directory / "tests.log").read_text(), \
        "sample-table boundary contracts must execute with the timing suite"
    core_directory = output / "packet-core-tests"
    core_directory.mkdir()
    core_tests = executable_tests(cohort / "probe", core_directory, env, library_package="symphonia-core")
    codecs.run([str(core_tests), "packet::tests::"], core_directory / "tests.log", cwd=output, env=env)
    controls = mutations(cohort, output, env, sample_tables_only=sample_tables_only) if run_controls else 0
    assert (ROOT / "engine/Cargo.lock").read_bytes() == lock, "production lock changed"
    summary = {"positive_container_cases": positive, "unsupported_or_malformed_cases": negative,
               "library_unit_tests": 11, "actual_decoder_contract_tests": 6,
               "packet_builder_boundary_tests": 2,
               "compiled_library_mutants": controls, "mutation_controls_executed": run_controls,
               "mutation_control_scope": "sample-tables-only" if sample_tables_only else "all" if run_controls else "none",
               "debug_overflow_control_executed": run_controls,
               "root_lock_unchanged": True,
               "binary_sha256": codecs.sha(binary), "production_adoption_ready": False,
               "scope": __doc__}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cohort", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--positive-only", action="store_true",
                        help="run semantic/provenance regression without repeating unrelated source mutations")
    parser.add_argument("--sample-table-controls-only", action="store_true",
                        help="run new three sample-table mutants plus all semantic/provenance checks")
    args = parser.parse_args()
    if args.positive_only and args.sample_table_controls_only:
        parser.error("--positive-only and --sample-table-controls-only are mutually exclusive")
    main(args.cohort.resolve(), args.output.resolve(), args.target_dir.resolve(),
         run_controls=not args.positive_only, sample_tables_only=args.sample_table_controls_only)
