#!/usr/bin/env python3
"""W09 isolated codec conformance probe; no production manifest/lock changes.

Compare the actual legacy modules with Symphonia 0.6.1 using only the selected
mini-game audio codecs/containers, metadata needed for MP3, and optional SIMD.
Generate original PCM, encode locally with FFmpeg and retain independent decode
output. Input frames, FFmpeg output frames and candidate frames are all recorded:
FFmpeg raw AAC output is not assumed to prove exact container/gapless duration.
This is correctness preparation, not an end-to-end/device performance benchmark.
"""
from __future__ import annotations

import argparse
import array
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import wave


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command: list[str], log: Path, *, cwd: Path, env: dict | None = None) -> None:
    with log.open("w", encoding="utf-8") as stream:
        result = subprocess.run(command, cwd=cwd, env=env, stdout=stream,
                                stderr=subprocess.STDOUT, timeout=300)
    assert result.returncode == 0, f"command failed ({result.returncode}): {log}"


def library_patches(root: Path, work: Path) -> dict[str, Path]:
    """Materialize checksum-pinned published crates plus the recorded patches.

    These paths belong only to this isolated workspace. The product does not
    select them until semantic, memory and performance acceptance is complete.
    """
    vendor = work / "vendor"
    vendor.mkdir()
    cache = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "registry/cache"
    result = {}
    inventory = []
    for name in ("symphonia-core", "symphonia-codec-aac", "symphonia-format-isomp4",
                 "symphonia-bundle-mp3", "symphonia-metadata",
                 "symphonia-format-ogg", "symphonia-format-riff", "symphonia-codec-pcm", "symphonia-codec-vorbis"):
        patches = root / "engine/third_party" / f"{name}-patches"
        declared, version, checksum = (patches / "UPSTREAM").read_text().split()
        assert declared == name and version == "0.6.1"
        archives = list(cache.glob(f"*/{name}-{version}.crate"))
        assert archives, f"fetch the published {name} {version} crate before the offline probe"
        archive = archives[0]
        assert sha(archive) == checksum, f"published {name} checksum differs"
        applied = []
        # Some workspace mounts map the inherited group to nobody. GNU patch
        # cannot preserve that unmapped group; patch our own /tmp extraction,
        # then copy verified bytes to the retained result, without chown.
        with tempfile.TemporaryDirectory(prefix="migo-codec-source-", dir="/tmp") as temporary:
            stage = Path(temporary)
            with tarfile.open(archive) as stream:
                stream.extractall(stage, filter="data")
            for patch in sorted(patches.glob("*.patch")):
                run(["patch", "--batch", "--forward", "--fuzz=0", "-p0", "-i", str(patch)],
                    work / f"{name}-{patch.stem}.log", cwd=stage)
                applied.append({"file": str(patch.relative_to(root)), "sha256": sha(patch)})
            assert applied, f"no {name} patches"
            shutil.copytree(stage / f"{name}-{version}", vendor / f"{name}-{version}")
        result[name] = vendor / f"{name}-{version}"
        files = {str(file.relative_to(result[name])): sha(file)
                 for file in sorted(result[name].rglob("*")) if file.is_file()}
        inventory.append({"crate": name, "version": version, "archive_sha256": checksum,
                          "patches": applied, "source_files": files})
    (work / "library-source-provenance.json").write_text(json.dumps(inventory, indent=2) + "\n")
    return result


def make_package(root: Path, work: Path, env: dict, simd: bool,
                 patched_libraries: bool = False, benchmark: bool = False,
                 phase_probe: bool = False, streaming_probe: bool = False) -> Path:
    assert not streaming_probe or patched_libraries, "push API requires the recorded library patches"
    package = work / "probe"
    (package / "src").mkdir(parents=True)
    run(["rustc", "-Vv"], work / "rustc-version.txt", cwd=root, env=env)
    run(["cargo", "-V"], work / "cargo-version.txt", cwd=root, env=env)
    locked = tomllib.loads((root / "engine/Cargo.lock").read_text())["package"]

    def version(name: str) -> str:
        versions = [row["version"] for row in locked if row["name"] == name]
        assert len(versions) == 1, (name, versions)
        return versions[0]

    manifest = f'''[package]
name="migo-codec-candidate"
version="0.0.0"
edition="2024"
[workspace]
[features]
simd=["symphonia/opt-simd"]
phase-probe=["dep:libc"]
metadata-budgets=[]
streaming-probe={json.dumps(["dep:symphonia-bundle-mp3", "symphonia-bundle-mp3/streaming"] if patched_libraries else [])}
wave-streaming-probe={json.dumps(["streaming-probe", "dep:symphonia-format-riff", "symphonia-format-riff/streaming", "dep:symphonia-codec-pcm"] if patched_libraries else [])}
adts-streaming-probe={json.dumps(["streaming-probe", "dep:symphonia-codec-aac", "symphonia-codec-aac/streaming"] if patched_libraries else [])}
vorbis-streaming-probe={json.dumps(["streaming-probe", "dep:symphonia-format-ogg", "symphonia-format-ogg/streaming", "dep:symphonia-codec-vorbis"] if patched_libraries else [])}
m4a-streaming-probe={json.dumps(["streaming-probe", "dep:symphonia-format-isomp4", "symphonia-format-isomp4/streaming", "dep:symphonia-codec-aac"] if patched_libraries else [])}
[dependencies]
symphonia={{version="=0.6.1",default-features=false,features=["aac","isomp4","mp3","ogg","vorbis","wav","pcm","id3v2"]}}
minimp3-sys="={version('minimp3-sys')}"
lewton="={version('lewton')}"
hound="={version('hound')}"
serde_json="={version('serde_json')}"
libc={{version="={version('libc')}",optional=true}}
symphonia-bundle-mp3={{version="=0.6.1",default-features=false,optional=true}}
symphonia-format-riff={{version="=0.6.1",default-features=false,features=["wav"],optional=true}}
symphonia-codec-pcm={{version="=0.6.1",optional=true}}
symphonia-codec-aac={{version="=0.6.1",optional=true}}
symphonia-format-ogg={{version="=0.6.1",optional=true}}
symphonia-codec-vorbis={{version="=0.6.1",optional=true}}
symphonia-format-isomp4={{version="=0.6.1",optional=true}}
shared={{package="migo-shared",path={json.dumps(str(root / 'engine/crates/shared'), ensure_ascii=False)},default-features=false}}
[dev-dependencies]
migo-alloc-probe={{path={json.dumps(str(root / 'engine/testing/alloc-probe'))}}}
[profile.release]
opt-level=3
lto="thin"
codegen-units=1
'''
    if patched_libraries:
        manifest += "\n[patch.crates-io]\n"
        for name, path in library_patches(root, work).items():
            manifest += f'{name}={{path={json.dumps(str(path), ensure_ascii=False)}}}\n'
        manifest += '\n[[test]]\nname="packet-core-contracts"\npath="tests/packet_core.rs"\n'
        tests = package / "tests"
        tests.mkdir()
        packet_source = work / "vendor/symphonia-core-0.6.1/src/packet.rs"
        (tests / "packet_core.rs").write_text(
            'mod io { pub use symphonia::core::io::*; }\n'
            'mod units { pub use symphonia::core::units::*; }\n'
            f'#[path = {json.dumps(str(packet_source))}] mod packet;\n')
    (package / "Cargo.toml").write_text(manifest, encoding="utf-8")
    (package / "Cargo.lock").write_bytes((root / "engine/Cargo.lock").read_bytes())
    source = []
    for name, path in (("decoder", "engine/crates/audio/src/decoder/mod.rs"),
                       ("limits", "engine/crates/audio/src/limits.rs"),
                       ("candidate", "scripts/testing/fixtures/audio-codec-probe/candidate.rs")):
        source.append(f'#[path = {json.dumps(str(root / path), ensure_ascii=False)}] mod {name};\n')
    entry = "benchmark.rs" if benchmark else "main.rs"
    source.append((root / f"scripts/testing/fixtures/audio-codec-probe/{entry}").read_text())
    streaming = root / "scripts/testing/fixtures/audio-codec-probe/streaming.rs"
    streaming_tests = root / "scripts/testing/fixtures/audio-codec-probe/streaming_contracts.rs"
    source.append(f'\n#[cfg(feature="streaming-probe")] #[path={json.dumps(str(streaming))}] mod streaming;\n')
    source.append(f'#[cfg(all(test,feature="streaming-probe"))] #[path={json.dumps(str(streaming_tests))}] mod streaming_contracts;\n')
    adts_tests = root / "scripts/testing/fixtures/audio-codec-probe/adts_adapter_contracts.rs"
    source.append(f'#[cfg(all(test,feature="adts-streaming-probe"))] #[path={json.dumps(str(adts_tests))}] mod adts_adapter_contracts;\n')
    wave_tests = root / "scripts/testing/fixtures/audio-codec-probe/wave_streaming_contracts.rs"
    source.append(f'#[cfg(all(test,feature="wave-streaming-probe"))] #[path={json.dumps(str(wave_tests))}] mod wave_streaming_contracts;\n')
    vorbis_tests = root / "scripts/testing/fixtures/audio-codec-probe/vorbis_adapter_contracts.rs"
    source.append(f'#[cfg(all(test,feature="vorbis-streaming-probe"))] #[path={json.dumps(str(vorbis_tests))}] mod vorbis_adapter_contracts;\n')
    m4a_tests = root / "scripts/testing/fixtures/audio-codec-probe/isomp4_adapter_contracts.rs"
    source.append(f'#[cfg(all(test,feature="m4a-streaming-probe"))] #[path={json.dumps(str(m4a_tests))}] mod isomp4_adapter_contracts;\n')
    if patched_libraries:
        contracts = root / "scripts/testing/fixtures/audio-codec-probe/timeline_contracts.rs"
        source.append(f'\n#[cfg(test)] #[path = {json.dumps(str(contracts))}] mod timeline_contracts;\n')
        mp3_fixture = root / "engine/crates/audio/src/mp3_fixture.rs"
        source.append(f'#[cfg(test)] #[path = {json.dumps(str(mp3_fixture))}] mod mp3_fixture;\n')
    (package / "src/main.rs").write_text("".join(source), encoding="utf-8")
    inputs = [root / "engine/crates/audio/src/limits.rs",
              root / "engine/crates/audio/src/mp3_fixture.rs"]
    inputs += sorted((root / "engine/crates/audio/src/decoder").rglob("*.rs"))
    inputs += sorted((root / "engine/crates/shared/src").rglob("*.rs"))
    inputs += sorted((root / "engine/testing/alloc-probe/src").rglob("*.rs"))
    inputs += sorted((root / "scripts/testing/fixtures/audio-codec-probe").rglob("*.rs"))
    source_inputs = {str(path.relative_to(root)): sha(path) for path in inputs}
    (work / "source-inputs.json").write_text(json.dumps(source_inputs, indent=2) + "\n")
    for path in inputs:
        retained = work / "source-inputs" / path.relative_to(root)
        retained.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, retained)
    # Resolve extra candidate packages only in this isolated lock, then freeze.
    run(["cargo", "metadata", "--offline", "--format-version", "1"], work / "metadata.jsonl", cwd=package, env=env)
    selected_features = ((["simd"] if simd else []) + (["metadata-budgets"] if patched_libraries else []) + (["phase-probe"] if phase_probe else [])
                         + (["streaming-probe"] if streaming_probe else []))
    features = ["--features", ",".join(selected_features)] if selected_features else []
    run(["cargo", "tree", "--offline", "--locked", *features, "-e", "normal,features"],
        work / "features.txt", cwd=package, env=env)
    tree = (work / "features.txt").read_text()
    for forbidden in ("symphonia-format-mkv", "symphonia-bundle-flac", "symphonia-codec-adpcm",
                      "symphonia-codec-alac", "exp-video-codecs", "exp-subtitle-codecs"):
        assert forbidden not in tree, f"candidate enabled unused capability: {forbidden}"
    run(["cargo", "build", "--offline", "--locked", "--release", *features, "--message-format=json"],
        work / "build.jsonl", cwd=package, env=env)
    found = []
    for line in (work / "build.jsonl").read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable"):
            found.append(Path(row["executable"]))
    assert len(found) == 1, found
    binary = work / ("codec-probe.exe" if os.name == "nt" else "codec-probe")
    shutil.copy2(found[0], binary)
    assert all(sha(root / path) == checksum for path, checksum in source_inputs.items()), \
        "codec source changed during build"
    return binary


def floats(path: Path) -> array.array:
    result = array.array("f")
    result.frombytes(path.read_bytes())
    if sys.byteorder != "little":
        result.byteswap()
    return result


def fixture_pcm(path: Path, rate: int, channels: int, frames: int) -> None:
    samples = bytearray()
    for frame in range(frames):
        envelope = min(1.0, frame / 32, (frames - 1 - frame) / 32)
        for channel in range(channels):
            sample = envelope * (0.3 * math.sin(2 * math.pi * (440 + 220 * channel) * frame / rate)
                                 + 0.05 * math.sin(2 * math.pi * 3100 * frame / rate))
            samples.extend(struct.pack("<h", round(sample * 32767)))
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(channels)
        stream.setsampwidth(2)
        stream.setframerate(rate)
        stream.writeframes(samples)


def limits() -> None:
    # The native decoder must report a graceful rejection. A panic, allocator
    # abort, signal, or timeout is a failed case rather than an accepted input
    # error. Bound malformed-input experiments without changing the host.
    if os.name == "posix":
        import resource
        resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def probe(binary: Path, mode: str, source: Path, directory: Path, *, cap: int = 16_777_216) -> dict:
    dump = directory / f"{mode}.f32le"
    result = subprocess.run([str(binary), mode, str(source), str(cap), str(dump)],
                            text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            timeout=30, preexec_fn=limits if os.name == "posix" else None)
    (directory / f"{mode}.log").write_text(result.stdout + result.stderr, encoding="utf-8")
    assert result.returncode in (0, 2), f"codec crashed instead of reporting an error: {mode}: {source}: {result.returncode}"
    data = json.loads(result.stdout)
    assert data["ok"] == (result.returncode == 0), data
    if data["ok"]:
        assert data["finite"], f"nonfinite decoded PCM: {source}"
        assert dump.stat().st_size == data["samples"] * 4
        if mode.startswith("candidate"):
            assert data["pcm_capacity_samples"] <= cap, "PCM allocated capacity exceeded its quota"
    return data


def negative_controls(root: Path, output: Path, env: dict, simd: bool, short_sounds: bool = False,
                      *, metadata_budgets: bool = False) -> None:
    original_path = root / "scripts/testing/fixtures/audio-codec-probe/candidate.rs"
    original = original_path.read_text()
    mutations = {
        "unbounded-capacity": (".try_reserve_exact(target - output.samples.len())", ".try_reserve(additional)"),
        "missing-append-limit": ('        if end > cap {\n            return Err("decoded audio exceeds PCM budget".into());\n        }', ""),
        "codec-block-as-output-budget": ("crate::limits::MAX_AUDIO_PCM_SAMPLES,", "cap as u64,"),
    }
    if not short_sounds:
        del mutations["codec-block-as-output-budget"]
    for name, (before, after) in mutations.items():
        assert original.count(before) == 1, f"codec mutation anchor changed: {name}"
        directory = output / name
        directory.mkdir()
        source = directory / "candidate.rs"
        source.write_text(original.replace(before, after, 1), encoding="utf-8")
        package = directory / "probe"
        shutil.copytree(output / "probe", package)
        main = package / "src/main.rs"
        text = main.read_text().replace(json.dumps(str(original_path), ensure_ascii=False),
                                        json.dumps(str(source), ensure_ascii=False))
        main.write_text(text, encoding="utf-8")
        selected = (["simd"] if simd else []) + (["metadata-budgets"] if metadata_budgets else [])
        features = ["--features", ",".join(selected)] if selected else []
        run(["cargo", "build", "--offline", "--locked", "--release", *features, "--message-format=json"],
            directory / "build.jsonl", cwd=package, env=env)
        artifacts = []
        for line in (directory / "build.jsonl").read_text().splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("reason") == "compiler-artifact" and row.get("executable"):
                artifacts.append(Path(row["executable"]))
        assert len(artifacts) == 1
        binary = directory / ("codec-probe.exe" if os.name == "nt" else "codec-probe")
        shutil.copy2(artifacts[0], binary)
        if name == "codec-block-as-output-budget":
            fixture = output / "tiny-17-1/aac.m4a"
            result = probe(binary, "candidate", fixture, directory, cap=17)
            assert not result["ok"] and "budget" in result["error"], result
            # This is the same success condition checked by short_sound_cases.
            try:
                assert result["ok"], "valid trimmed sound rejected by output budget"
            except AssertionError as error:
                (directory / "observed-red.txt").write_text(str(error) + "\n")
            print(f"Codec actual source mutation {name}: short-sound assertion failed as required", flush=True)
            continue
        # Known-length files reserve the demuxer's complete bounded estimate.
        # Both growth controls must therefore use a genuinely unknown length.
        fixture = output / "aac-adts-44100-1/encoded.aac"
        rows = json.loads((output / "valid-results.json").read_text())
        cap = next(row["arms"]["candidate"]["samples"] for row in rows
                   if row["codec"] == "aac-adts" and row["rate"] == 44_100)
        if name == "missing-append-limit":
            cap -= 1
        try:
            probe(binary, "candidate-unseekable", fixture, directory, cap=cap)
        except AssertionError as error:
            assert "capacity exceeded its quota" in str(error), f"unrelated codec control failure: {error}"
            (directory / "observed-red.txt").write_text(str(error) + "\n")
        else:
            raise AssertionError(f"codec source mutation escaped the quota gate: {name}")
        print(f"Codec actual source mutation {name}: quota assertion failed as required", flush=True)


def short_sound_cases(binary: Path, output: Path) -> int:
    count = 0
    for frames in (1, 17, 511, 1023):
        for channels in (1, 2):
            directory = output / f"tiny-{frames}-{channels}"
            directory.mkdir()
            wav = directory / "source.wav"
            fixture_pcm(wav, 48000, channels, frames)
            aac = directory / "aac.m4a"
            run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(wav), "-c:a", "aac",
                 "-movie_timescale", "48000", str(aac)], directory / "encode.log", cwd=output)
            for name, source in (("wav", wav), ("aac", aac)):
                target = directory / name
                target.mkdir()
                result = probe(binary, "candidate", source, target, cap=frames * channels)
                assert result["ok"], "valid trimmed sound rejected by output budget"
                assert result["frames"] == frames and result["pcm_capacity_samples"] <= frames * channels, result
                count += 1
    return count


def main(root: Path, output: Path, target: Path, simd: bool, patched_libraries: bool = False) -> None:
    output.mkdir(parents=True, exist_ok=True)
    assert not any(output.iterdir()), "codec output directory must be empty"
    original_lock = (root / "engine/Cargo.lock").read_bytes()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN=tomllib.loads((root / "engine/rust-toolchain.toml").read_text())["toolchain"]["channel"],
               CARGO_TARGET_DIR=str(target))
    binary = make_package(root, output, env, simd, patched_libraries)
    ffmpeg = shutil.which("ffmpeg")
    assert ffmpeg, "FFmpeg is required for the independent fixture oracle"
    run([ffmpeg, "-version"], output / "ffmpeg-version.txt", cwd=output)
    results = []
    settings = {"wav16": ("wav", []), "wav24": ("wav", ["-c:a", "pcm_s24le"]),
                "wav-float": ("wav", ["-c:a", "pcm_f32le"]),
                "mp3-cbr": ("mp3", ["-c:a", "libmp3lame", "-b:a", "128k"]),
                "mp3-vbr": ("mp3", ["-c:a", "libmp3lame", "-q:a", "4"]),
                "vorbis": ("ogg", ["-c:a", "libvorbis", "-q:a", "4"]),
                "aac-adts": ("aac", ["-c:a", "aac", "-profile:a", "aac_low", "-b:a", "128k", "-f", "adts"]),
                "aac-m4a": ("m4a", ["-c:a", "aac", "-profile:a", "aac_low", "-b:a", "128k", "-movflags", "+faststart"])}
    for rate, channels in ((44_100, 1), (48_000, 2)):
        frames = rate // 5 + 137
        for codec, (suffix, encoding) in settings.items():
            directory = output / f"{codec}-{rate}-{channels}"
            directory.mkdir()
            source = directory / "source.wav"
            fixture_pcm(source, rate, channels, frames)
            audio = source
            if encoding:
                audio = directory / f"encoded.{suffix}"
                run([ffmpeg, "-nostdin", "-v", "error", "-i", str(source), *encoding, str(audio)],
                    directory / "encode.log", cwd=output)
            oracle = directory / "ffmpeg.f32le"
            run([ffmpeg, "-nostdin", "-v", "error", "-i", str(audio), "-f", "f32le", "-c:a", "pcm_f32le", str(oracle)],
                directory / "oracle.log", cwd=output)
            reference = floats(oracle)
            row = {"codec": codec, "rate": rate, "channels": channels, "input_frames": frames,
                   "ffmpeg_frames": len(reference) // channels, "fixture_sha256": sha(audio), "arms": {}}
            for mode in ("legacy", "candidate"):
                data = probe(binary, mode, audio, directory)
                if data["ok"]:
                    assert data["sample_rate"] == rate and data["channels"] == channels
                    actual = floats(directory / f"{mode}.f32le")
                    count = min(len(actual), len(reference))
                    noise = sum((float(a) - float(b)) ** 2 for a, b in zip(actual, reference))
                    signal = sum(float(v) ** 2 for v in reference[:count])
                    data["prefix_snr_db"] = 10 * math.log10(signal / noise) if noise and signal else None
                    data["same_frames_as_input"] = data["frames"] == frames
                    data["same_frames_as_ffmpeg"] = data["frames"] == row["ffmpeg_frames"]
                row["arms"][mode] = data
            assert row["arms"]["candidate"]["ok"], f"candidate rejected a valid {codec} fixture: {row}"
            tight = directory / "exact-output-budget"
            tight.mkdir()
            size = row["arms"]["candidate"]["samples"]
            exact = probe(binary, "candidate", audio, tight, cap=size)
            assert exact["ok"] and exact["samples"] == size and exact["pcm_capacity_samples"] <= size, exact
            row["exact_capacity_budget"] = True
            budget = directory / "small-budget"
            budget.mkdir()
            rejected = probe(binary, "candidate", audio, budget, cap=128)
            assert not rejected["ok"] and "budget" in rejected["error"], rejected
            row["small_budget_rejected"] = True
            if codec == "aac-adts":
                cumulative = directory / "cumulative-budget"
                cumulative.mkdir()
                # The library's nonseekable source has no total byte length,
                # so ADTS cannot preflight an estimated total frame count.
                # Exercise the actual append boundary after successful blocks.
                capped = probe(binary, "candidate-unseekable", audio, cumulative,
                               cap=row["arms"]["candidate"]["samples"] - channels)
                assert not capped["ok"] and "decoded audio exceeds PCM budget" in capped["error"], capped
                row["cumulative_budget_rejected"] = True
            results.append(row)
            (output / "valid-results.json").write_text(json.dumps(results, indent=2) + "\n")
            print(f'{codec} {rate}/{channels}: legacy={row["arms"]["legacy"].get("frames", "unsupported")} '
                  f'candidate={row["arms"]["candidate"].get("frames")} input={frames} ffmpeg={row["ffmpeg_frames"]}', flush=True)
    short_cases = short_sound_cases(binary, output) if patched_libraries else 0
    malformed = {"empty": b"", "garbage": bytes(range(256)) * 16,
                 "id3-overclaimed": b"ID3\x04\x00\x00\x7f\x7f\x7f\x7f",
                 "mp4-no-track": b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00M4A isom\x00\x00\x00\x08moov"}
    rejected = []
    for name, payload in malformed.items():
        directory = output / f"malformed-{name}"
        directory.mkdir()
        path = directory / "input.bin"
        path.write_bytes(payload)
        result = probe(binary, "candidate", path, directory)
        assert not result["ok"], f"malformed input was accepted: {name}"
        rejected.append({"name": name, **result})
    (output / "malformed-results.json").write_text(json.dumps(rejected, indent=2) + "\n")
    oversized = output / "oversized-encoded"
    oversized.mkdir()
    large = oversized / "input.bin"
    with large.open("wb") as stream:
        stream.truncate(16 * 1024 * 1024 + 1)
    over = probe(binary, "candidate", large, oversized)
    assert not over["ok"] and "16MiB" in over["error"], over
    negative_controls(root, output, env, simd, short_sounds=patched_libraries,
                      metadata_budgets=patched_libraries)
    assert (root / "engine/Cargo.lock").read_bytes() == original_lock, "candidate changed production lock"
    summary = {"binary": str(binary), "sha256": sha(binary), "simd": simd,
               "patched_libraries": patched_libraries,
               "valid_cases": len(results), "budget_rejections": len(results), "malformed_rejections": len(rejected),
               "exact_capacity_cases": len(results), "cumulative_budget_cases": 2, "encoded_limit_cases": 1,
               "compiled_negative_controls": 3 if patched_libraries else 2,
               "short_exact_output_cases": short_cases,
               "production_adoption_ready": False,
               "open_semantic_issues": [{"codec": row["codec"], "rate": row["rate"],
                    "channels": row["channels"], "input_frames": row["input_frames"],
                    "candidate_frames": row["arms"]["candidate"]["frames"]}
                    for row in results if row["codec"] == "aac-m4a"
                    and row["arms"]["candidate"]["frames"] != row["input_frames"]],
               "production_lock_unchanged": True, "scope": __doc__}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--simd", action="store_true")
    parser.add_argument("--library-patches", action="store_true")
    args = parser.parse_args()
    main(Path(__file__).resolve().parents[2], args.output.resolve(), args.target_dir.resolve(), args.simd,
         args.library_patches)
