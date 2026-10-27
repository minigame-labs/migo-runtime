#!/usr/bin/env python3
"""Run candidate MP3 and optional WAVE/AAC through HTTP/CPU/PCM transport.

The isolated test crate compiles services' real modules with cfg(test), so its
existing crate-private loopback resolver hook is available. No shipped feature,
network policy exception, codec dependency or Root lock entry is introduced.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import hashlib
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("timeline", ROOT / "scripts/testing/check_audio_codec_timeline.py")
timeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timeline)
codecs = timeline.codecs


def absolute_paths(text: str, directory: Path) -> str:
    return re.sub(r'path\s*=\s*"([^"]+)"',
                  lambda match: 'path=' + json.dumps(str((directory / match[1]).resolve())), text)


def published_source(directory: Path, cargo_home: Path, row: dict) -> dict:
    name = row["name"]
    prefix = f"1/{name}" if len(name) == 1 else f"2/{name}" if len(name) == 2 else f"3/{name[0]}/{name}" if len(name) == 3 else f"{name[:2]}/{name[2:4]}/{name}"
    records = []
    for index in (cargo_home / "registry/index").glob(f"*/.cache/{prefix}"):
        for field in index.read_bytes().split(b"\0"):
            try:
                entry = json.loads(field)
            except (ValueError, UnicodeDecodeError):
                continue
            if isinstance(entry, dict) and entry.get("vers") == row["version"]:
                records.append(entry)
    assert len(records) == 1, (name, "missing published registry identity")
    archives = list((cargo_home / "registry/cache").glob(f"*/{directory.name}.crate"))
    assert len(archives) == 1 and codecs.sha(archives[0]) == records[0]["cksum"], row
    files = {}
    with tarfile.open(archives[0]) as archive:
        for member in archive.getmembers():
            if not member.isfile():
                continue
            relative = str(Path(member.name).relative_to(directory.name))
            data = archive.extractfile(member).read()
            assert (directory / relative).read_bytes() == data, (name, relative, "registry source drift")
            files[relative] = hashlib.sha256(data).hexdigest()
    return {"package": records[0]["cksum"], "files": files}


def build(package: Path, output: Path, env: dict, name: str, features: str = "") -> Path:
    log = output / f"{name}-build.jsonl"
    extra = ["--features", features] if features else []
    # A restored source can have an older mtime than the last compiled mutant.
    # Force this test crate to compile and verify Cargo's fresh=false record.
    package_name = tomllib.loads((package / "Cargo.toml").read_text())["package"]["name"]
    codecs.run(["cargo", "clean", "--offline", "--locked", "--release", "-p",
                package_name], output / f"{name}-clean.log", cwd=package, env=env)
    codecs.run(["cargo", "test", "--offline", "--locked", "--release", "--lib", "--no-run", *extra,
                "--message-format=json"], log, cwd=package, env=env)
    paths = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable") and row.get("profile", {}).get("test"):
            assert not row.get("fresh"), "test artifact was not rebuilt"
            paths.append(Path(row["executable"]))
    assert len(paths) == 1, paths
    retained = output / f"{name}-tests"
    shutil.copy2(paths[0], retained)
    return retained


def controls(package: Path, output: Path, env: dict, features: str = "") -> int:
    declarations = [
        ("publication-cancel-removed", "streaming_transport.rs",
         "_ = state.cancelled() => false,", "_ = std::future::pending::<()>() => false,",
         "terminal_messages_cancel_while_queue_is_full"),
        ("cpu-wait-cancel-removed", "streaming_transport.rs",
         "_ = state.cancelled() => None,", "_ = std::future::pending::<()>() => None,",
         "cpu_step_leaves_http_worker_free_and_cancel_does_not_wait_for_join"),
        ("yield-discarded", "streaming_transport.rs",
         "Progress::Yield => tokio::task::yield_now().await,", "Progress::Yield => break,",
         "real_http_chunks_partial_feed_eof_and_pcm_equal_whole_file"),
        ("cpu-runs-on-io", "off_worker.rs", "let (value, outcome) = cpu\n            .run(move || {\n                let outcome = step(&mut value);\n                (value, outcome)\n            })\n            .await?;",
         "let outcome = step(&mut value); let (value, outcome) = (value, outcome);",
         "cpu_step_leaves_http_worker_free_and_cancel_does_not_wait_for_join"),
    ]
    if "bandlimited-probe" in features.split(","):
        declarations += [
            ("resampler-tail-not-flushed", "resampled_streaming.rs",
             "if self.decoder_done {", "if self.decoder_done { return Ok(self.outcome(0, Progress::Finished));",
             "resampling::http_resampling_and_bypass_match_library_timeline_with_exact_done"),
            ("ready-reports-source-rate", "resampled_streaming.rs",
             "                self.target_rate\n", "                self.decoder.rate\n",
             "resampling::http_resampling_and_bypass_match_library_timeline_with_exact_done"),
            ("composed-processing-allocates", "resampled_streaming.rs",
             "let result = self.step_inner(input, eof, pcm, cancel);",
             "std::hint::black_box(vec![0_u8;64]);\n        let result = self.step_inner(input, eof, pcm, cancel);",
             "resampling::bounded_codec_and_resampler_steps_have_no_steady_rust_allocation"),
        ]
    if "wave-streaming-probe" in features.split(","):
        declarations += [
            ("audio-end-before-http-eof", "incremental_decoder.rs",
             "if !self.eof {\n                        self.input.clear();\n                        self.head = 0;\n                        return Ok(Progress::NeedInput);\n                    }",
             "", "wave::http_wave_profiles_resampling_and_bypass_match_exact_pcm_timeline"),
        ]
    if "adts-streaming-probe" in features.split(","):
        # Run shared regressions against the newly composed AAC path as well.
        routed = {
            "yield-discarded": "adts::http_adts_profiles_match_exact_pcm_timeline",
            "resampler-tail-not-flushed": "adts::http_adts_profiles_match_exact_pcm_timeline",
            "ready-reports-source-rate": "adts::http_adts_profiles_match_exact_pcm_timeline",
            "composed-processing-allocates": "adts::bounded_adts_decode_resample_steps_have_no_steady_rust_allocation",
        }
        declarations = [(name, path, before, after, routed.get(name, test))
                        for name, path, before, after, test in declarations]
        declarations += [
            ("adts-waits-for-prefetch", "adts_streaming.rs",
             "        self.0.next(input, eof).map(|step| match step {",
             "        if input.len() < 8192 && !eof { return Ok(PacketStep::NeedMore); }\n        self.0.next(input, eof).map(|step| match step {",
             "adts::first_adts_packet_arrives_before_server_releases_remaining_body"),
            ("adts-errors-become-end", "adts_streaming.rs", "self.0.next(input, eof)",
             "self.0.next(input, eof).or(Ok(AdtsStep::End))",
             "adts::invalid_adts_and_http_truncation_report_error_without_done"),
        ]
    if "vorbis-streaming-probe" in features.split(","):
        routed = {
            "yield-discarded": "vorbis::http_vorbis_profiles_match_exact_pcm_timeline",
            "resampler-tail-not-flushed": "vorbis::http_vorbis_profiles_match_exact_pcm_timeline",
            "ready-reports-source-rate": "vorbis::http_vorbis_profiles_match_exact_pcm_timeline",
            "composed-processing-allocates": "vorbis::bounded_vorbis_resampling_has_no_steady_rust_allocation",
        }
        declarations = [(name, path, before, after, routed.get(name, test))
                        for name, path, before, after, test in declarations]
        declarations += [
            ("vorbis-waits-for-prefetch", "vorbis_streaming.rs",
             "        self.0.next(input, eof).map(|step| match step {",
             "        if input.len() < 65536 && !eof { return Ok(PacketStep::NeedMore); }\n        self.0.next(input, eof).map(|step| match step {",
             "vorbis::first_vorbis_pcm_arrives_before_remaining_body_is_released"),
            ("vorbis-errors-become-end", "vorbis_streaming.rs", "self.0.next(input, eof)",
             "self.0.next(input, eof).or(Ok(OggStep::End))",
             "vorbis::bad_vorbis_crc_eos_chain_and_http_never_publish_done"),
            ("vorbis-allocated-bit-eof", "../vendor/symphonia-core-0.6.1/src/io/bit.rs",
             "Err(io::Error::from(io::ErrorKind::UnexpectedEof))",
             "Err(io::Error::new(io::ErrorKind::UnexpectedEof, \"unexpected end of bitstream\"))",
             "vorbis::long_vorbis_http_player_has_bounded_pcm_beyond_cache_quota"),
        ]
    for name, relative, before, after, test in declarations:
        source = output / "source" / relative
        original = source.read_text()
        assert original.count(before) == 1, (name, "mutation anchor changed")
        source.write_text(original.replace(before, after))
        try:
            binary = build(package, output, env, name, features)
            with (output / f"{name}-test.log").open("w") as log:
                status = subprocess.run([str(binary), "--exact", f"transport_contracts::{test}", "--nocapture"],
                                        cwd=package, env=env, stdout=log, stderr=subprocess.STDOUT,
                                        timeout=90).returncode
            observed = (output / f"{name}-test.log").read_text()
            assert status == 101 and "panicked at" in observed and "1 failed" in observed, (name, status, "must fail an executed Rust assertion")
        finally:
            source.write_text(original)
        print(f"actual transport mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def validate(output: Path, env: dict, positive_only: bool = False) -> None:
    package = output / "probe"
    checksums = json.loads((output / "source-inputs.json").read_text())
    assert all(codecs.sha(ROOT / path) == checksum for path, checksum in checksums.items()), "Root sources changed"
    binary = output / "positive-tests"
    settings = json.loads((output / "build-settings.json").read_text()) if (output / "build-settings.json").exists() else {}
    expected_tests = settings.get("tests", 6)
    retained_sources = {str(path.relative_to(output)): codecs.sha(path)
                        for path in (output / "source").glob("*.rs")}
    codecs.run([str(binary), "transport_contracts::", "--nocapture", "--test-threads=1"],
               output / "tests-validated.log", cwd=package, env=env)
    assert f"{expected_tests} passed" in (output / "tests-validated.log").read_text()
    negative = 0 if positive_only else controls(package, output, env, settings.get("features", ""))
    assert all(codecs.sha(ROOT / path) == checksum for path, checksum in checksums.items()), "Root sources changed during tests"
    assert all(codecs.sha(output / path) == checksum for path, checksum in retained_sources.items()), "mutation was not restored"
    summary = {"tests": expected_tests, "actual_http_codec_cases": 24,
               "actual_http_resampling_cases": settings.get("resampling_cases", 0), "compiled_negative_controls": negative, "positive_only": positive_only,
               "actual_http_wave_cases": settings.get("wave_cases", 0),
               "actual_wave_player_cases": settings.get("wave_player_cases", 0),
               "actual_http_adts_cases": settings.get("adts_cases", 0),
               "actual_adts_player_cases": settings.get("adts_player_cases", 0),
               "actual_http_vorbis_cases": settings.get("vorbis_cases", 0),
               "actual_vorbis_player_cases": settings.get("vorbis_player_cases", 0),
               "test_binary_sha256": codecs.sha(binary), "root_lock_unchanged": True,
               "retained_sources": retained_sources,
               "scope": "host loopback HTTP/client/CPU handoff/PCM pool; optional bounded sinc composition; no device/performance acceptance"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


def main(output: Path, target: Path, fixtures: Path, rubato_home: Path | None = None,
         wave_streaming: bool = False, adts_streaming: bool = False, vorbis_streaming: bool = False,
         positive_only: bool = False) -> None:
    assert not (wave_streaming or adts_streaming or vorbis_streaming) or rubato_home is not None, "codec composition requires Rubato candidates"
    assert not output.exists(), "use a fresh result directory"
    output.mkdir(parents=True)
    package = output / "probe"
    (package / "src").mkdir(parents=True)
    (output / "source").mkdir()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true",
               W09_STREAM_FIXTURES=str(output / "fixtures"), CC="/usr/bin/clang", CXX="/usr/bin/clang++")
    env["W09_ADTS_FIXTURES"] = str(output / "adts-fixtures")
    env["W09_OGG_FIXTURES"] = str(output / "vorbis-input/fixtures")
    for name in ("ANDROID_NDK", "ANDROID_NDK_HOME"):
        env.pop(name, None)
    shutil.copytree(fixtures, output / "fixtures")
    if vorbis_streaming:
        spec = importlib.util.spec_from_file_location("ogg_stream", ROOT / "scripts/testing/check_audio_ogg_streaming.py")
        ogg = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ogg)
        inputs = output / "vorbis-input"
        inputs.mkdir()
        ogg.fixtures(inputs)
        ogg.silent_stream_fixture(inputs)
    if adts_streaming:
        adts = output / "adts-fixtures"
        adts.mkdir()
        codecs.run(["ffmpeg", "-version"], output / "ffmpeg-version.txt", cwd=output)
        for i, (rate, channels, frames) in enumerate([(44100, 1, 17), (48000, 2, 9737),
                                                    (22050, 1, 9737), (48000, 2, 240137)]):
            source = adts / f"{i}.wav"
            codecs.fixture_pcm(source, rate, channels, frames)
            codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(source), "-c:a", "aac",
                        "-profile:a", "aac_low", "-b:a", "128k", "-f", "adts", str(adts / f"{i}.aac")],
                       output / f"adts-{i}-encode.log", cwd=output)
        codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i", "anullsrc=r=8000:cl=mono",
                    "-frames:a", "3", "-c:a", "aac", "-profile:a", "aac_low", "-f", "adts",
                    str(adts / "silence-8000.aac")], output / "adts-silence-encode.log", cwd=output)
        (output / "adts-fixture-sha256.json").write_text(json.dumps({p.name: codecs.sha(p) for p in adts.iterdir()}, indent=2) + '\n')
    locked_bytes = (ROOT / "engine/Cargo.lock").read_bytes()
    root_manifest = (ROOT / "engine/Cargo.toml").read_text()
    locked = tomllib.loads(locked_bytes.decode())["package"]
    def version(name):
        values = {row["version"] for row in locked if row["name"] == name}
        assert len(values) == 1, (name, values)
        return values.pop()
    services = ROOT / "engine/crates/services"
    manifest = absolute_paths((services / "Cargo.toml").read_text(), services)
    manifest = manifest.replace('name = "migo-services"', 'name = "migo-streaming-transport-probe"', 1)
    manifest = manifest.replace('"test-util"', '"test-util", "rt-multi-thread"')
    extra = '\nsymphonia={version="=0.6.1",default-features=false,features=["mp3","opt-simd"]}\n'
    extra += 'symphonia-bundle-mp3={version="=0.6.1",default-features=false,features=["streaming"]}\n'
    optional = 'symphonia-format-riff={version="=0.6.1",default-features=false,features=["wav"],optional=true}\n'
    optional += 'symphonia-codec-pcm={version="=0.6.1",optional=true}\n'
    optional += 'symphonia-codec-aac={version="=0.6.1",optional=true}\n'
    optional += 'symphonia-format-ogg={version="=0.6.1",optional=true}\n'
    optional += 'symphonia-codec-vorbis={version="=0.6.1",optional=true}\n'
    manifest = manifest.replace('\n[dependencies]\n', '\n[dependencies]\n' + optional, 1)
    for name in ("minimp3-sys", "lewton", "hound", "futures-util"):
        extra += f'{name}="={version(name)}"\n'
    for name in ("alloc", "executor"):
        extra += f'migo-{name}-probe={{path={json.dumps(str(ROOT / f"engine/testing/{name}-probe"))}}}\n'
    if rubato_home is not None:
        extra += 'rubato={version="=5.0.1",default-features=false}\n'
    manifest = manifest.replace('\n[features]', extra + '\n[features]')
    manifest = manifest.replace('\n[features]\n', '\n[features]\nbandlimited-probe=[]\nmetadata-budgets=[]\nwave-streaming-probe=["dep:symphonia-format-riff","symphonia-format-riff/streaming","dep:symphonia-codec-pcm","symphonia/wav","symphonia/pcm"]\n')
    manifest = manifest.replace('\n[features]\n', '\n[features]\nadts-streaming-probe=["dep:symphonia-codec-aac","symphonia-codec-aac/streaming","symphonia/aac"]\n')
    manifest = manifest.replace('\n[features]\n', '\n[features]\nvorbis-streaming-probe=["dep:symphonia-format-ogg","symphonia-format-ogg/streaming","dep:symphonia-codec-vorbis","symphonia/ogg","symphonia/vorbis"]\n')
    # Keep inherited workspace dependency definitions exact, rebasing only paths.
    workspace_deps = root_manifest.split('[workspace.dependencies]\n', 1)[1].split('\n[', 1)[0]
    manifest += '\n[workspace]\n[workspace.package]\nversion="0.0.0"\nlicense="BSL-1.1"\n'
    manifest += '\n[workspace.dependencies]\n' + absolute_paths(workspace_deps, ROOT / "engine")
    manifest += '\n[profile.release]\nopt-level=3\nlto="thin"\ncodegen-units=1\n'
    manifest += '\n[patch.crates-io]\n'
    for name, path in codecs.library_patches(ROOT, output).items():
        manifest += f'{name}={{path={json.dumps(str(path))}}}\n'
    for name, relative in (("ada-url", "ada-url-4.0.0"), ("tungstenite", "tungstenite-0.30.0")):
        manifest += f'{name}={{path={json.dumps(str(ROOT / "engine/third_party" / relative))}}}\n'
    if rubato_home is not None:
        # Only new names get path candidates. Existing versions remain the
        # engine's locked cohort, rather than upgrading shared math/proc macros.
        names = {row["name"] for row in locked}
        new_sources = {}
        for directory in sorted((rubato_home / "registry/src").glob("*/*")):
            cargo = directory / "Cargo.toml"
            if not cargo.exists():
                continue
            row = tomllib.loads(cargo.read_text())["package"]
            if row["name"] not in names:
                inventory = published_source(directory, rubato_home, row)
                retained = output / "resampling-vendor" / directory.name
                shutil.copytree(directory, retained)
                manifest += f'{row["name"]}={{path={json.dumps(str(retained))}}}\n'
                new_sources[row["name"]] = {"version": row["version"], "path": str(retained),
                                            "checksum": inventory["package"], "source_files": inventory["files"]}
        assert new_sources.get("rubato", {}).get("version") == "5.0.1"
        (output / "resampling-paths.json").write_text(json.dumps(new_sources, indent=2) + '\n')
    (package / "Cargo.toml").write_text(manifest)
    (package / "Cargo.lock").write_bytes(locked_bytes)

    source = ['extern crate self as migo_services;\n']
    for module in re.findall(r'^pub mod (\w+);$', (services / "src/lib.rs").read_text(), re.M):
        source.append(f'#[path={json.dumps(str(services / f"src/{module}.rs" if (services / f"src/{module}.rs").exists() else services / f"src/{module}/mod.rs"))}] pub mod {module};\n')
    source.append('pub use error::ServiceError;\n')
    modules = ["decoder", "limits", "resampler", "mp3_fixture", "streaming", "streaming_transport", "off_worker"]
    if rubato_home is not None:
        modules += ["cache", "inner_audio"]
    for module in modules:
        path = ROOT / f"engine/crates/audio/src/{module}.rs"
        if module == "decoder":
            path = ROOT / "engine/crates/audio/src/decoder/mod.rs"
        if module in ("streaming", "streaming_transport", "off_worker"):
            retained = output / "source" / path.name
            shutil.copyfile(path, retained)
            path = retained
        source.append(f'#[path={json.dumps(str(path))}] mod {module};\n')
    for module, filename in (("push_decoder", "streaming.rs"), ("candidate", "candidate.rs"),
                             ("transport_contracts", "transport_contracts.rs")):
        path = output / "source" / (module + ".rs")
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe" / filename, path)
        source.append(f'#[path={json.dumps(str(path))}] mod {module};\n')
    for filename in ("incremental_decoder.rs", "wave_streaming.rs", "adts_streaming.rs", "vorbis_streaming.rs"):
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe" / filename, output / "source" / filename)
    if rubato_home is not None:
        for module in ("bandlimited", "resampled_streaming"):
            path = output / f"source/{module}.rs"
            shutil.copyfile(ROOT / f"scripts/testing/fixtures/audio-codec-probe/{module}.rs", path)
            source.append(f'#[path={json.dumps(str(path))}] mod {module};\n')
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe/resampled_transport_contracts.rs",
                        output / "source/resampled_transport_contracts.rs")
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe/player_transport_contracts.rs",
                        output / "source/player_transport_contracts.rs")
    if wave_streaming:
        for filename in ("wave_transport_contracts.rs", "wave_pcm_contracts.rs"):
            shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe" / filename,
                            output / "source" / filename)
    if adts_streaming:
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe/adts_transport_contracts.rs",
                        output / "source/adts_transport_contracts.rs")
    if vorbis_streaming:
        shutil.copyfile(ROOT / "scripts/testing/fixtures/audio-codec-probe/vorbis_transport_contracts.rs",
                        output / "source/vorbis_transport_contracts.rs")
    source.append('#[global_allocator] static COUNTING:migo_alloc_probe::CountingAllocator=migo_alloc_probe::CountingAllocator::system();\n')
    (package / "src/lib.rs").write_text(''.join(source))
    inputs = []
    for relative in ("engine/crates/audio/src", "engine/crates/services/src", "engine/crates/io/src",
                     "engine/crates/shared/src", "engine/testing/alloc-probe/src", "engine/testing/executor-probe/src"):
        inputs += sorted((ROOT / relative).rglob("*.rs"))
    inputs += [ROOT / "engine/Cargo.toml", ROOT / "engine/Cargo.lock", services / "Cargo.toml"]
    inputs += sorted((ROOT / "scripts/testing/fixtures/audio-codec-probe").glob("*.rs"))
    inputs.append(ROOT / "scripts/testing/check_audio_streaming_transport.py")
    inputs.append(ROOT / "scripts/bench/audio-codecs.py")
    if vorbis_streaming:
        inputs += [ROOT / "scripts/testing/check_audio_ogg_streaming.py",
                   ROOT / "scripts/testing/check_audio_metadata_skip.py"]
    for row in json.loads((output / "library-source-provenance.json").read_text()):
        inputs += sorted((ROOT / f"engine/third_party/{row['crate']}-patches").glob("*"))
    checksums = {str(path.relative_to(ROOT)): codecs.sha(path) for path in inputs}
    for path in inputs:
        destination = output / "source-inputs" / path.relative_to(ROOT)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    (output / "source-inputs.json").write_text(json.dumps(checksums, indent=2) + '\n')
    for command, log in ((["rustc", "-Vv"], "rustc-version.txt"), (["cargo", "-V"], "cargo-version.txt"),
                         (["cargo", "metadata", "--offline", "--format-version", "1"], "metadata.jsonl")):
        codecs.run(command, output / log, cwd=package, env=env)
    settings = {"features": "bandlimited-probe,metadata-budgets" if rubato_home is not None else "metadata-budgets",
                "tests": 10 if rubato_home is not None else 6,
                "resampling_cases": 10 if rubato_home is not None else 0}
    if wave_streaming:
        settings["features"] += ",wave-streaming-probe"
        settings["tests"] += 11
        settings["wave_cases"] = 28
        settings["wave_player_cases"] = 15
    if adts_streaming:
        settings["features"] += ",adts-streaming-probe"
        settings["tests"] += 8
        settings["adts_cases"] = 48
        settings["adts_player_cases"] = 8
    if vorbis_streaming:
        settings["features"] += ",vorbis-streaming-probe"
        settings["tests"] += 8
        settings["vorbis_cases"] = 72
        settings["vorbis_player_cases"] = 12
    (output / "build-settings.json").write_text(json.dumps(settings, indent=2) + '\n')
    build(package, output, env, "positive", settings["features"])
    validate(output, env, positive_only)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--positive-only", action="store_true",
                        help="run all positive regressions, explicitly recording zero compiled controls")
    parser.add_argument("--resume-validation", action="store_true",
                        help="reuse a compiled positive cohort after sandbox denied loopback execution")
    parser.add_argument("--rubato-cargo-home", type=Path,
                        help="add the checksum-verified isolated Rubato candidates, without changing Root dependencies")
    parser.add_argument("--wave-streaming", action="store_true",
                        help="also verify WAVE HTTP, PCM, multichannel and resampling contracts")
    parser.add_argument("--adts-streaming", action="store_true",
                        help="also verify AAC-LC HTTP, resampling, player and generated long stream")
    parser.add_argument("--vorbis-streaming", action="store_true",
                        help="also verify borrowed/cross-page Vorbis, maximum window and long-stream player")
    args = parser.parse_args()
    if args.resume_validation:
        env = os.environ.copy()
        env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(args.target_dir.resolve()),
                   CARGO_NET_OFFLINE="true", W09_STREAM_FIXTURES=str(args.output.resolve() / "fixtures"),
                   W09_ADTS_FIXTURES=str(args.output.resolve() / "adts-fixtures"),
                   W09_OGG_FIXTURES=str(args.output.resolve() / "vorbis-input/fixtures"),
                   CC="/usr/bin/clang", CXX="/usr/bin/clang++")
        validate(args.output.resolve(), env, args.positive_only)
    else:
        main(args.output.resolve(), args.target_dir.resolve(), args.fixtures.resolve(),
             args.rubato_cargo_home.resolve() if args.rubato_cargo_home else None,
             args.wave_streaming, args.adts_streaming, args.vorbis_streaming, args.positive_only)
