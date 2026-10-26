#!/usr/bin/env python3
"""ID3 metadata budgets: actual APIs, decoded PCM, allocations and bounded fuzz.

All library changes are checksum-pinned archives plus ordered candidate patches
in an isolated workspace. No product lock/dependency selection is changed.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import random
import resource
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("timeline", ROOT / "scripts/testing/check_audio_codec_timeline.py")
timeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timeline)
codecs = timeline.codecs
FEATURES = "simd,metadata-budgets"


def executable(package: Path, output: Path, env: dict, name: str) -> Path:
    work = output / name
    work.mkdir()
    return timeline.executable_tests(package, work, env, features=FEATURES)


def controls(output: Path, env: dict) -> int:
    declarations = [
        ("options-ignored", "symphonia-metadata", "src/id3v2/mod.rs", "limits: Limits::new(opts)",
         "limits: Limits::new(Default::default())", "oversized_tags_are_skipped_without_losing_following_tags"),
        ("decoded-tag-limit-ignored", "symphonia-metadata", "src/id3v2/limits.rs",
         ".is_some_and(|limit| tag.storage_size_upper_bound() > limit)", ".is_some_and(|_limit| false)",
         "decoded_text_expansion_is_charged_to_the_tag_limit"),
        ("picture-preallocation-skip-removed", "symphonia-metadata", "src/id3v2/limits.rs",
         "self.visual != Some(0)", "true", "disabled_pictures_are_skipped_before_their_payload_is_allocated"),
        ("encoded-frame-cap-ignored", "symphonia-metadata", "src/id3v2/limits.rs",
         "&& !self.frame.is_some_and(|limit| size > limit as u64)", "",
         "oversized_frame_skipping_has_a_bounded_temporary_allocation"),
        # ScopedStream already checks this, but UnsyncStream's generic boxed
        # reader allocates before reaching its inner scope. Exercise both paths.
        ("finite-frame-extent-unchecked", "symphonia-metadata", "src/id3v2/frames.rs",
         "if size as u64 > reader.bytes_available() {", "if false {",
         "impossible_frame_extent_does_not_allocate_the_declared_size"),
        ("unsynchronised-skip-counts-encoded-bytes", "symphonia-metadata", "src/id3v2/unsync.rs",
         "    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {\n        for _ in 0..count {\n            self.read_byte()?;",
         "    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {\n        for _ in 0..count {\n            self.inner.read_byte()?;",
         "skipped_unsynchronised_frames_preserve_the_next_tag"),
        ("nested-frame-depth-ignored", "symphonia-metadata", "src/id3v2/limits.rs",
         "&& !self.depth.is_some_and(|limit| depth > limit)", "", "nested_frame_depth_bounds_temporary_copies"),
        ("toc-depth-ignored", "symphonia-metadata", "src/id3v2/mod.rs",
         "if limit_depth.is_some_and(|limit| depth > limit) {", "if false {",
         "toc_graph_depth_is_bounded_independently_of_frame_nesting"),
        ("retention-budget-ignored", "symphonia-metadata", "src/id3v2/limits.rs",
         "Some(left) if size > left => false,", "Some(left) if size > left => true,",
         "cumulative_budget_covers_metadata_vector_spare_capacity"),
        ("toc-output-capacity-ignored", "symphonia-metadata", "src/id3v2/limits.rs",
         "size_of::<String>() + size_of::<ChapterGroupItem>()", "size_of::<String>()",
         "toc_output_slot_capacity_is_charged_before_building"),
        ("decoded-string-storage-undercounted", "symphonia-core", "src/meta.rs",
         "Self::String(value) => arc + std::mem::size_of::<String>() + value.capacity(),",
         "Self::String(value) => arc + std::mem::size_of::<String>() + value.capacity() / 4,",
         "decoded_text_expansion_is_charged_to_the_tag_limit"),
        ("empty-external-revisions-retained", "symphonia-core", "src/formats/probe.rs",
         "if matches!(meta_opts.limit_total_bytes, crate::common::Limit::Maximum(0)) {", "if false {",
         "no_retention_does_not_accumulate_empty_external_revisions"),
        ("footer-not-consumed", "symphonia-metadata", "src/id3v2/mod.rs",
         "    if header.has_footer {\n", "    if false {\n", "footer_is_consumed_before_the_next_media_byte"),
    ]
    for name, crate, relative, before, _, _ in declarations:
        source = output / "vendor" / f"{crate}-0.6.1" / relative
        assert source.read_text().count(before) == 1, (name, "mutation anchor changed")
    for name, crate, relative, before, after, test in declarations:
        source = output / "vendor" / f"{crate}-0.6.1" / relative
        original = source.read_text()
        assert original.count(before) == 1, (name, "mutation anchor changed")
        source.write_text(original.replace(before, after))
        try:
            binary = executable(output / "probe", output, env, name)
            with (output / f"{name}.log").open("w") as log:
                status = subprocess.run([str(binary), "--exact", f"metadata_contracts::{test}", "--nocapture"],
                                        env=env, stdout=log, stderr=subprocess.STDOUT, timeout=30,
                                        preexec_fn=worker_limits).returncode
            assert status == 101, (name, status, "must fail an executed assertion, not a panic signal/OOM/timeout")
            text = (output / f"{name}.log").read_text()
            assert f"panicked at {output / 'source/metadata_contracts.rs'}:" in text and "1 failed" in text, (name, "failure outside contract assertion")
        finally:
            source.write_text(original)
        print(f"actual metadata mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def worker_limits() -> None:
    resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))


def syncsafe(size: int) -> bytes:
    return bytes((size >> shift) & 127 for shift in (21, 14, 7, 0))


def id3(version: int, body: bytes, flags: int = 0) -> bytes:
    result = b"ID3" + bytes([version, 0, flags]) + syncsafe(len(body)) + body
    if flags & 16 and version == 4:
        result += b"3DI" + result[3:10]
    return result


def frame(version: int, name: bytes, body: bytes, flags: int = 0) -> bytes:
    size = (len(body).to_bytes(3, "big") if version == 2 else
            len(body).to_bytes(4, "big") if version == 3 else syncsafe(len(body)))
    return name + size + (b"" if version == 2 else flags.to_bytes(2, "big")) + body


def corpus(output: Path) -> list[Path]:
    directory = output / "fuzz-inputs"
    directory.mkdir()
    seeds = []
    for version in [2, 3, 4]:
        title = frame(version, b"TT2" if version == 2 else b"TIT2", b"\0title")
        seeds += [id3(version, title), id3(version, title, 128), id3(version, title, 64)]
        if version < 4:
            raw = frame(version, b"TT2" if version == 2 else b"TIT2", b"\0" + b"\xff\xe0"*512)
            raw += title
            seeds.append(id3(version, raw.replace(b"\xff", b"\xff\0"), 128))
        for count in [0, 1, 127, 1023, 65535, 1048576]:
            seeds.append(id3(version, frame(version, b"TT2" if version == 2 else b"TIT2", b"\0" + b"x"*count)))
        for encoded in [b"\0", b"\1\xff\xfe\xff\xd8", b"\2\xdc\0", b"\3\xff\xc0\x80"]:
            seeds.append(id3(version, frame(version, b"TT2" if version == 2 else b"TIT2", encoded)))
        if version > 2:
            seeds.append(id3(version, frame(version,b"APIC",b"\0image/png\0\x03\0"+b"p"*131072)))
            seeds.append(id3(version, frame(version,b"TIT2",b"\xff\xff\xff\xffbomb",0x80 if version==3 else 9)))
    title = frame(4,b"TIT2",b"\3footer")
    seeds += [id3(4,title,16), b"", b"ID3", b"not metadata", id3(4,b"TIT2"+b"\x7f"*4+b"\0\0bad")]
    for seed in list(seeds):
        for length in [0, 1, 3, 9, 10, max(0,len(seed)-1)]:
            seeds.append(seed[:length])
    rng = random.Random(0x4D49474F)
    base = id3(4, title + frame(4,b"TXXX",b"\3description\0text"),16)
    for _ in range(128):
        value = bytearray(base)
        for _ in range(rng.randrange(1, 9)):
            value[rng.randrange(len(value))] = rng.randrange(256)
        seeds.append(bytes(value))
    unique = sorted(set(seeds), key=lambda data:(len(data),data))
    paths=[]
    for index, data in enumerate(unique):
        path=directory/f"{index:04}.id3";path.write_bytes(data);paths.append(path)
    (output / "fuzz-corpus.json").write_text(json.dumps({p.name:codecs.sha(p) for p in paths},indent=2)+"\n")
    return paths


def fuzz(binary: Path, output: Path, env: dict) -> dict:
    paths=corpus(output);accepted=rejected=0
    logs=output/"fuzz-logs";logs.mkdir()
    for path in paths:
        child=env.copy();child["W09_METADATA_FUZZ_INPUT"]=str(path)
        log=logs/(path.stem+".log")
        with log.open("w") as stream:
            status=subprocess.run([str(binary),"--exact","metadata_contracts::bounded_metadata_fuzz_case",
                                   "--ignored","--nocapture"],env=child,stdout=stream,stderr=subprocess.STDOUT,
                                  preexec_fn=worker_limits,timeout=10).returncode
        assert status==0,(path.name,status,"bounded fuzz worker crashed or failed")
        text=log.read_text();accepted+=text.count("W09_METADATA_FUZZ accepted");rejected+=text.count("W09_METADATA_FUZZ rejected")
    assert accepted+rejected==len(paths)
    return {"processes":len(paths),"accepted":accepted,"rejected":rejected,"address_space_mib":512,"cpu_limit_seconds":5,
            "scope":"deterministic mutation/truncation smoke corpus, not coverage-guided or exhaustive fuzzing"}


def main(output: Path, target: Path) -> None:
    assert not output.exists(), "use a fresh result directory"
    output.mkdir(parents=True)
    env=os.environ.copy();env.update(RUSTUP_TOOLCHAIN="1.95.0",CARGO_TARGET_DIR=str(target),CARGO_NET_OFFLINE="true")
    original_lock=(ROOT/"engine/Cargo.lock").read_bytes()
    binary=codecs.make_package(ROOT,output,env,True,True)
    manifest=output/"probe/Cargo.toml"
    manifest.write_text(manifest.read_text().replace('[dev-dependencies]','[dev-dependencies]\nsymphonia-metadata={version="=0.6.1",default-features=false,features=["id3v2"]}'))
    source=output/"source";source.mkdir()
    shutil.copyfile(ROOT/"scripts/testing/fixtures/audio-codec-probe/metadata_contracts.rs",source/"metadata_contracts.rs")
    entry=output/"probe/src/main.rs"
    entry.write_text(entry.read_text()+'\n#[cfg(test)] #[path='+json.dumps(str(source/"metadata_contracts.rs"))+'] mod metadata_contracts;\n')
    shutil.copyfile(Path(__file__),output/"validation-driver.py")
    codecs.run(["cargo","metadata","--offline","--format-version","1"],output/"metadata-test-resolution.jsonl",cwd=output/"probe",env=env)
    tests=executable(output/"probe",output,env,"positive")
    codecs.run([str(tests),"metadata_contracts::","--nocapture","--test-threads=1"],output/"tests.log",cwd=output,env=env)
    assert "17 passed" in (output/"tests.log").read_text()
    positive_hash=codecs.sha(tests)
    negative=controls(output,env)
    fuzzed=fuzz(tests,output,env)
    assert codecs.sha(tests)==positive_hash
    assert (ROOT/"engine/Cargo.lock").read_bytes()==original_lock
    inputs=json.loads((output/"source-inputs.json").read_text());assert all(codecs.sha(ROOT/path)==value for path,value in inputs.items())
    for row in json.loads((output/"library-source-provenance.json").read_text()):
        directory=output/"vendor"/f'{row["crate"]}-{row["version"]}'
        assert all(codecs.sha(directory/path)==value for path,value in row["source_files"].items()),"mutation not restored"
    summary={"tests_passed":17,"explicit_subprocess_test_ignored_in_suite":1,"compiled_negative_controls":negative,
             "fuzz":fuzzed,"positive_test_sha256":positive_hash,"full_probe_sha256":codecs.sha(binary),
             "root_lock_unchanged":True,"all_source_mutations_restored":True,"scope":__doc__}
    (output/"summary.json").write_text(json.dumps(summary,indent=2)+"\n");print(json.dumps(summary,indent=2),flush=True)


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True);parser.add_argument("--target-dir",type=Path,required=True)
    args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve())
