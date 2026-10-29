#!/usr/bin/env python3
"""W09 bounded M4A index/library acceptance, not HTTP or product playback.

The pull reader needed shared timing/cursor logic before the new borrowed
sample index could be trusted. Exercise actual codecs, source-frame and
independent FFmpeg layout oracles, bounded metadata and executable mutants.
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
import tomllib

ROOT = Path(__file__).resolve().parents[2]
def module(name: str, path: Path):
    spec=importlib.util.spec_from_file_location(name,path)
    loaded=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded
codecs=module("migo_mp4_index_codecs",ROOT/"scripts/bench/audio-codecs.py")
timeline=module("migo_mp4_index_timeline",ROOT/"scripts/testing/check_audio_codec_timeline.py")

def fixtures(directory: Path) -> None:
    directory.mkdir()
    U=lambda n:struct.pack('>I',n)
    Q=lambda n:struct.pack('>Q',n)
    def atom(kind,body):return U(8+len(body))+kind+body
    def top(data):
        rows=[];pos=0
        while pos<len(data):
            size=int.from_bytes(data[pos:pos+4],'big');kind=data[pos+4:pos+8]
            assert size>=8 and pos+size<=len(data)
            rows.append((pos,kind,data[pos:pos+size]));pos+=size
        assert pos==len(data)
        return rows
    PATH=(b'moov',b'trak',b'mdia',b'minf',b'stbl')
    def table(data,kind):
        rows=timeline.payloads(data,PATH+(kind,));assert len(rows)==1
        return rows[0]
    def chunked(data,reordered=False,co64=False):
        sizes=table(data,b'stsz');assert int.from_bytes(sizes[4:8],'big')==0
        sizes=list(struct.unpack('>'+str((len(sizes)-12)//4)+'I',sizes[12:]))
        assert len(sizes)>4
        stco=table(data,b'stco');assert int.from_bytes(stco[4:8],'big')==1
        old_offset=int.from_bytes(stco[8:12],'big')
        chunks=[sizes[:1],sizes[1:4],sizes[4:]]
        lengths=[sum(c) for c in chunks]
        offsets=[0,lengths[0],lengths[0]+lengths[1]]
        if reordered:offsets=[lengths[2],lengths[2]+lengths[0],0]
        groups=U(0)+U(3)+b''.join(U(i+1)+U(len(c))+U(1) for i,c in enumerate(chunks))
        def changed(delta):
            pack=Q if co64 else U
            offsets_bytes=U(0)+U(3)+b''.join(pack(old_offset+n+delta) for n in offsets)
            result=timeline.rewrite(data,{PATH+(b'stsc',):groups,PATH+(b'stco',):offsets_bytes})
            if co64:
                assert result.count(b'stco')==1
                result=result.replace(b'stco',b'co64',1)
            return result
        result=changed(0)
        before=next(p for p,k,d in top(data) if k==b'mdat')
        after=next(p for p,k,d in top(result) if k==b'mdat')
        result=changed(after-before)
        if reordered:
            packet_bytes=data[old_offset:old_offset+sum(lengths)]
            a=packet_bytes[:lengths[0]];b=packet_bytes[lengths[0]:lengths[0]+lengths[1]];c=packet_bytes[-lengths[2]:]
            result=result[:after+8]+c+a+b+result[after+8+len(packet_bytes):]
        return result

    manifest=[]
    for number,(rate,channels,frames) in enumerate([(48000,1,17),(44100,2,9737),(48000,1,240137)]):
        wav=directory/f'{number}.wav';codecs.fixture_pcm(wav,rate,channels,frames)
        encoded={}
        for mode in ['front','tail']:
            path=directory/f'{number}-{mode}.m4a'
            args=['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-y','-i',str(wav),'-c:a','aac','-profile:a','aac_low','-movie_timescale',str(rate)]
            if mode=='front':args+=['-movflags','+faststart']
            subprocess.run(args+[str(path)],check=True, timeout=30)
            encoded[mode]=path.read_bytes()
            manifest.append({'file':path.name,'frames':frames,'channels':channels,'rate':rate})
        if number:
            for name,perm,wide in [('split',False,False),('reorder',True,False),('co64',False,True)]:
                path=directory/f'{number}-{name}.m4a';path.write_bytes(chunked(encoded['front'],perm,wide))
                manifest.append({'file':path.name,'frames':frames,'channels':channels,'rate':rate})
            # Independently prove that test-only sample-table surgery preserves
            # valid file interpretation; compare FFmpeg output before/after.
            pcm=[]
            for mode in ['front','split','reorder','co64']:
                result=subprocess.run(['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-i',str(directory/f'{number}-{mode}.m4a'),'-f','f32le','-'],check=True, timeout=30,stdout=subprocess.PIPE)
                pcm.append(result.stdout)
            assert all(p==pcm[0] for p in pcm)
    (directory/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print('Generated',len(manifest),'valid AAC-LC index fixtures; FFmpeg chunk-layout oracle agrees')

    # Random-access AAC PNS may use a different noise realization after reset.
    # Keep the original encoder corpus and add a deterministic no-PNS corpus
    # for sample-level seek comparisons; do not relax them into count-only tests.
    seek_manifest=[]
    for number,(rate,channels,frames) in enumerate([(48000,1,17),(44100,2,9737),(48000,1,240137)]):
        encoded={}
        for mode in ['front','tail']:
            path=directory/f'{number}-seek-{mode}.m4a'
            args=['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-y','-i',str(directory/f'{number}.wav'),'-c:a','aac','-aac_pns','0','-profile:a','aac_low','-movie_timescale',str(rate)]
            if mode=='front':args+=['-movflags','+faststart']
            subprocess.run(args+[str(path)],check=True,timeout=30)
            encoded[mode]=path.read_bytes()
            seek_manifest.append({'file':path.name,'frames':frames,'channels':channels,'rate':rate,'pns':False})
        if number:
            for name,perm,wide in [('split',False,False),('reorder',True,False),('co64',False,True)]:
                path=directory/f'{number}-seek-{name}.m4a';path.write_bytes(chunked(encoded['front'],perm,wide))
                seek_manifest.append({'file':path.name,'frames':frames,'channels':channels,'rate':rate,'pns':False})
            pcm=[]
            for mode in ['front','split','reorder','co64']:
                result=subprocess.run(['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-i',str(directory/f'{number}-seek-{mode}.m4a'),'-f','f32le','-'],check=True,timeout=30,stdout=subprocess.PIPE)
                pcm.append(result.stdout)
            assert all(p==pcm[0] for p in pcm)
    # Derive alternate media clocks and a nonstandard edit from unchanged AAC
    # payloads. Box sizes/offsets stay fixed; the library remains the only parser.
    original=(directory/'1-seek-front.m4a').read_bytes()
    for label,factor,start in [('scaled',2,1024),('edit',1,1377)]:
        mdhd=bytearray(timeline.payloads(original,timeline.MDHD)[0]);assert mdhd[0]==0
        for offset in [12,16]:mdhd[offset:offset+4]=U(int.from_bytes(mdhd[offset:offset+4],'big')*factor)
        stts=bytearray(timeline.payloads(original,timeline.STTS)[0])
        for offset in range(12,len(stts),8):stts[offset:offset+4]=U(int.from_bytes(stts[offset:offset+4],'big')*factor)
        elst=bytearray(timeline.payloads(original,timeline.ELST)[0]);assert elst[0]==0
        elst[12:16]=struct.pack('>i',start*factor)
        modified=timeline.rewrite(original,{timeline.MDHD:bytes(mdhd),timeline.STTS:bytes(stts),timeline.ELST:bytes(elst)})
        path=directory/f'1-seek-{label}.m4a';path.write_bytes(modified)
        outputs=[]
        for data in [original,modified]:
            result=subprocess.run(['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-ignore_editlist','1','-i','pipe:0','-f','f32le','-'],input=data,check=True,timeout=30,stdout=subprocess.PIPE)
            outputs.append(result.stdout)
        assert outputs[0]==outputs[1],'derived presentation changed raw AAC PCM'
        # FFmpeg's raw skip_samples behavior on an alternate media clock is
        # not a presentation-frame oracle. Compare unedited payload PCM here;
        # the packet/frame contracts independently verify both edit timelines.
        assert outputs[1], 'derived edit must contain decoded audio'
        seek_manifest.append({'file':path.name,'frames':9737 if label=='scaled' else 9384,'channels':2,'rate':44100,'pns':False,'media_factor':factor,'media_start':start})
    (directory/'seek-manifest.json').write_text(json.dumps(seek_manifest,indent=2)+'\n')
    print('Generated',len(seek_manifest),'additional no-PNS AAC seek fixtures; FFmpeg layout oracle agrees')

    src=(directory/'1-tail.m4a').read_bytes()
    meta=atom(b'meta',bytes(4)+atom(b'ilst',atom(b'\xa9nam',atom(b'data',struct.pack('>II',1,0)+b'x'*(1024*1024)))))
    movie=timeline.payloads(src,(b'moov',))[0]
    (directory/'1-metadata.m4a').write_bytes(timeline.rewrite(src,{(b'moov',):movie+atom(b'udta',meta)}))


def build(work: Path, output: Path, env: dict, name: str, package: str,
          mutated: str | None = None, *, test_target: str = "index") -> Path:
    for i, selected in enumerate(dict.fromkeys([mutated or package, package])):
        codecs.run(["cargo", "clean", "--offline", "--locked", "-p", selected],
                   output / f"{name}-clean-{i}.log", cwd=work, env=env)
    command = ["cargo", "test", "--offline", "--locked", "--release", "--no-run",
               "--message-format=json", "-p", package]
    if package == "migo-mp4-index-probe":
        command += ["--test", test_target]
    else:
        command += ["--lib"]
        if package == "symphonia-format-isomp4":
            command += ["--features", "streaming"]
    log = output / f"{name}-build.jsonl"
    codecs.run(command, log, cwd=work, env=env)
    artifacts = [json.loads(line) for line in log.read_text().splitlines()
                 if line.startswith('{"reason":"compiler-artifact"')]
    tests = [row for row in artifacts if row.get("executable") and row["profile"]["test"]]
    assert len(tests) == 1 and tests[0]["fresh"] is False, "test binary must actually rebuild"
    if mutated:
        assert any(row["target"]["name"] == mutated.replace("-", "_")
                   and row["fresh"] is False for row in artifacts), "mutated library was not rebuilt"
    if package == "migo-mp4-index-probe":
        for selected in ["symphonia-core", "symphonia-codec-aac", "symphonia-format-isomp4", "symphonia-metadata"]:
            path = work / "vendor" / f"{selected}-0.6.1"
            assert any(str(path) in row["package_id"] for row in artifacts), selected
    binary = output / f"{name}-tests"
    shutil.copy2(tests[0]["executable"], binary)
    return binary


def controls(work: Path, env: dict) -> int:
    # Every control changes a retained library copy, compiles it, and must fail
    # an executed Rust assertion. Compiler errors/timeouts are never red proof.
    mp4 = "symphonia-format-isomp4"
    core = "symphonia-core"
    probe = "migo-mp4-index-probe"
    limits = "exact_index_packet_limits_partial_atoms_and_sticky_bad_extents_are_checked"
    real = "real_front_tail_split_co64_and_reordered_chunks_keep_packet_and_pcm_timeline"
    profile = "inconsistent_counts_and_multiple_tracks_are_rejected_by_the_selected_profile"
    slice_seek = "io::buf_reader::tests::verify_slice_seek_is_clamped_and_preserves_real_positions"
    declarations = [
        ("index-cap-removed", mp4, "src/streaming.rs", "if input.len() > cap {", "if false {", probe, limits),
        ("packet-cap-removed", mp4, "src/streaming.rs", "size <= options.max_packet_bytes", "true", probe, limits),
        ("multiple-tracks-accepted", mp4, "src/streaming.rs", "moov.traks.len() != 1", "moov.traks.is_empty()", probe, profile),
        ("timing-count-unchecked", mp4, "src/streaming.rs", "if timing_count != count {", "if false {", probe, profile),
        ("packet-trim-end-removed", mp4, "src/streaming.rs", "        packet.trim_end = self.trim_end;", "", probe, real),
        ("packet-pts-zero", mp4, "src/streaming.rs", "pts: info.ts,", "pts: Timestamp::ZERO,", probe, real),
        ("chunk-start-hint-removed", mp4, "src/stream.rs", "sample_in_chunk == 0", "false", probe, real),
        ("index-failure-not-sticky", mp4, "src/streaming.rs", "            self.failed = true;", "", probe, limits),
        ("packet-copied", mp4, "src/streaming.rs", "        let mut packet = PacketRef::new(",
         "        std::hint::black_box(data.to_vec());\n        let mut packet = PacketRef::new(",
         probe, "indexed_packets_decode_and_convert_without_steady_rust_allocation"),
        ("metadata-value-parsed", mp4, "src/streaming.rs",
         "MetadataOptions::default().limit_total_bytes(Limit::Maximum(0))", "MetadataOptions::default()",
         probe, "unused_metadata_is_skipped_before_value_allocation"),
        ("slice-absolute-unclamped", core, "src/io/buf_reader.rs",
         "self.pos = pos.min(self.buf.len() as u64) as usize;", "self.pos = pos as usize;", core, slice_seek),
        ("slice-relative-underflows", core, "src/io/buf_reader.rs",
         "self.pos.saturating_add_signed(delta)", "self.pos.checked_add_signed(delta).unwrap()", core, slice_seek),
        ("timing-rescans-previous-rows", mp4, "src/atoms/stts.rs",
         "        if sample_num < cursor.first_sample {",
         "        cursor.row = 0; cursor.first_sample = 0; cursor.first_ts = 0;\n        if sample_num < cursor.first_sample {",
         mp4, "atoms::stts::cursor_contracts::sequential_timing_visits_entries_linearly_and_seeks_stay_correct"),
        ("co64-offset-truncated", mp4, "src/streaming.rs", "offset: data.pos,", "offset: u64::from(data.pos as u32),",
         probe, "co64_sample_positions_keep_the_full_absolute_offset"),
        ("object-extent-unchecked", mp4, "src/streaming.rs", "if self.options.object_len.is_some_and(|len| end > len) {", "if false {",
         probe, limits),
    ]
    seek = "index_seek_matches_pull_descriptors_and_full_packet_preroll_after_eof"
    invalid = "invalid_seek_preserves_cursor_and_corrupt_extents_remain_failed"
    wide = "co64_seek_preserves_offsets_above_4gib_and_preroll_sample_positions"
    declarations += [
        ("seek-always-starts-at-zero", mp4, "src/streaming.rs", "self.state.seek_media_ts(required_ts)?", "self.state.seek_media_ts(Timestamp::ZERO)?", probe, seek),
        ("seek-skips-media-clock-mapping", mp4, "src/demuxer.rs", "Some(timeline) => timeline.seek_media_ts(ts),", "Some(_timeline) => Ok(ts),", probe, seek),
        ("seek-reports-media-ticks", mp4, "src/demuxer.rs", "Some(timeline) => timeline.presentation_ts(timing.ts)?,", "Some(_timeline) => Timestamp::new(timing.ts as i64),", probe, seek),
        ("seek-does-not-move-sample-cursor", mp4, "src/demuxer.rs", "self.next_sample = sample_num;", "self.next_sample = 0;", probe, seek),
        ("seek-does-not-move-byte-cursor", mp4, "src/demuxer.rs", "self.next_sample_pos = position;", "self.next_sample_pos = 0;", probe, seek),
        ("seek-truncates-co64-byte-cursor", mp4, "src/demuxer.rs", "self.next_sample_pos = position;", "self.next_sample_pos = u64::from(position as u32);", probe, wide),
        ("seek-reports-preroll-as-required", mp4, "src/demuxer.rs", "Ok(SeekedTo { track_id: self.track_id, required_ts, actual_ts })", "Ok(SeekedTo { track_id: self.track_id, required_ts: actual_ts, actual_ts })", probe, seek),
        ("seek-ignores-object-bound", mp4, "src/demuxer.rs", "if object_len.is_some_and(|len| end > len) {", "if false {", probe, invalid),
        ("seek-hides-packet-end-overflow", mp4, "src/demuxer.rs", 'position.checked_add(u64::from(desc.size))\n            .ok_or(Error::DecodeError("isomp4: seek sample end overflow"))?', "position.saturating_add(u64::from(desc.size))", probe, invalid),
        ("seek-error-not-sticky", mp4, "src/streaming.rs", "self.failed = result.as_ref().is_err_and(|error| !matches!(error, Error::SeekError(_)));", "self.failed = false;", probe, invalid),
        ("seek-bypasses-failed-index", mp4, "src/streaming.rs", 'pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {\n        if self.failed {', 'pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {\n        if false {', probe, invalid),
        ("invalid-target-erases-cursor", mp4, "src/streaming.rs", "        let result = self.seek_inner(ts);", "        self.state.reset();\n        let result = self.seek_inner(ts);", probe, invalid),
        ("invalid-target-poisons-index", mp4, "src/streaming.rs", "self.failed = result.as_ref().is_err_and(|error| !matches!(error, Error::SeekError(_)));", "self.failed = result.is_err();", probe, invalid),
        ("seek-allocates-on-every-call", mp4, "src/streaming.rs", "    pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {", "    pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {\n        std::hint::black_box(vec![0_u8; 64]);", probe, "index_seek_and_first_descriptor_have_no_steady_heap_activity"),
        ("seek-omits-codec-preroll", mp4, "src/audio_timeline.rs", "(self.start + ts).saturating_sub(self.block_frames)", "self.start + ts", probe, "deterministic_aac_seek_preroll_matches_full_pcm_suffix"),
    ]
    for name, library, relative, before, after, package, test in declarations:
        path = work / "vendor" / f"{library}-0.6.1" / relative
        original = path.read_bytes()
        text = original.decode()
        assert text.count(before) == 1, (name, "mutation anchor changed")
        path.write_text(text.replace(before, after))
        try:
            binary = build(work, work, env, name, package, mutated=library)
            log = work / f"{name}-test.log"
            with log.open("w") as stream:
                status = subprocess.run([str(binary), "--exact", test, "--nocapture"],
                                        cwd=work, env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=90).returncode
            observed = log.read_text()
            assert status == 101 and "panicked at" in observed and "1 failed" in observed, (name, "not a runtime assertion rejection")
        finally:
            path.write_bytes(original)
        print(f"actual MP4 index mutation {name}: executable assertion rejected", flush=True)
    return len(declarations)


def main(output: Path, target: Path, *, positive_only: bool = False) -> None:
    assert not output.exists(), "use a fresh result directory"
    output.mkdir(parents=True)
    initial_lock = (ROOT / "engine/Cargo.lock").read_bytes()
    inputs = [Path(__file__).resolve(), ROOT / "scripts/bench/audio-codecs.py",
              ROOT / "scripts/testing/check_audio_codec_timeline.py",
              ROOT / "scripts/testing/fixtures/audio-codec-probe/isomp4_index_contracts.rs",
              ROOT / "scripts/testing/check_gitignore_contract.py",
              ROOT / "engine/Cargo.toml", ROOT / "engine/Cargo.lock",
              ROOT / "engine/testing/alloc-probe/Cargo.toml"]
    inputs += sorted((ROOT / "engine/testing/alloc-probe/src").rglob("*.rs"))
    for patchset in (ROOT / "engine/third_party").glob("symphonia-*-patches"):
        inputs += [p for p in patchset.rglob("*") if p.is_file()]
    frozen = {str(p.relative_to(ROOT)): codecs.sha(p) for p in inputs}
    (output / "source-inputs.json").write_text(json.dumps(frozen, indent=2) + "\n")
    libraries = codecs.library_patches(ROOT, output)
    recorded = json.loads((output / "library-source-provenance.json").read_text())
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    env.pop("ANDROID_NDK", None)
    env.pop("ANDROID_NDK_HOME", None)
    codecs.run(["rustc", "-Vv"], output / "rustc-version.txt", cwd=output, env=env)
    codecs.run(["ffmpeg", "-version"], output / "ffmpeg-version.txt", cwd=output, env=env)
    fixtures(output / "fixtures")
    package = output / "probe"
    (package / "tests").mkdir(parents=True)
    shutil.copy2(ROOT / "scripts/testing/fixtures/audio-codec-probe/isomp4_index_contracts.rs", package / "tests/index.rs")
    manifest = '[workspace]\nresolver="2"\nmembers=["probe","vendor/symphonia-core-0.6.1","vendor/symphonia-format-isomp4-0.6.1"]\n\n[patch.crates-io]\n'
    manifest += ''.join(f'{name}={{path={json.dumps(str(path))}}}\n' for name, path in libraries.items())
    (output / "Cargo.toml").write_text(manifest)
    (package / "Cargo.toml").write_text(f'''[package]
name="migo-mp4-index-probe"
version="0.0.0"
edition="2024"
[dependencies]
symphonia-core="=0.6.1"
symphonia-format-isomp4={{version="=0.6.1",features=["streaming"]}}
symphonia-codec-aac="=0.6.1"
migo-alloc-probe={{path={json.dumps(str(ROOT / "engine/testing/alloc-probe"))}}}
''')
    # Resolve new candidate libraries from the product cohort, rather than
    # upgrading unrelated cached transitive versions in an isolated lock.
    (output / "Cargo.lock").write_bytes(initial_lock)
    codecs.run(["cargo", "metadata", "--offline", "--format-version", "1"], output / "resolve.log", cwd=output, env=env)
    versions = {}
    for row in tomllib.loads(initial_lock.decode())["package"]:
        versions.setdefault(row["name"], set()).add(row["version"])
    selected = tomllib.loads((output / "Cargo.lock").read_text())["package"]
    assert all(row["name"] not in versions or row["version"] in versions[row["name"]]
               for row in selected), "existing dependency version drift"
    identity = lambda row: (row["name"], row["version"], row.get("source"), row.get("checksum"))
    locked = {identity(row) for row in tomllib.loads(initial_lock.decode())["package"]}
    assert all(row["name"] not in versions or identity(row) in locked for row in selected), "existing dependency source drift"
    binaries = {}
    for name, selected, expected in [("core", "symphonia-core", 72), ("format", "symphonia-format-isomp4", 12),
                                     ("index", "migo-mp4-index-probe", 13)]:
        binary = build(output, output, env, f"positive-{name}", selected)
        log = output / f"positive-{name}-tests.log"
        codecs.run([str(binary), "--nocapture", "--test-threads=1"], log, cwd=output, env=env)
        assert f"{expected} passed; 0 failed" in log.read_text(), (name, "expected contracts did not execute")
        binaries[name] = codecs.sha(binary)
    negatives = 0 if positive_only else controls(output, env)
    for row in recorded:
        path = libraries[row["crate"]]
        assert timeline.source_tree(path) == row["source_files"], (row["crate"], "mutated source not restored")
    assert all(codecs.sha(ROOT / path) == checksum for path, checksum in frozen.items()), "Root inputs changed"
    assert (ROOT / "engine/Cargo.lock").read_bytes() == initial_lock, "production lock changed"
    summary = {"core_tests": 72, "format_tests": 12, "real_index_tests": 13, "valid_codec_fixtures": 12, "deterministic_seek_fixtures": 14,
               "compiled_controls": negatives, "positive_only": positive_only, "binary_sha256": binaries,
               "libraries": len(recorded), "patches": sum(len(row["patches"]) for row in recorded),
               "selected_library_cargo_paths_verified": ["core", "AAC", "ISOMP4", "metadata"],
               "library_sources_restored": True, "root_inputs_unchanged": True,
               "root_lock_sha256": codecs.sha(ROOT / "engine/Cargo.lock"),
               "scope": "host bounded index and actual AAC decode only; no incremental packetizer/HTTP/Range/fallback/product/native/device/performance acceptance"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--positive-only", action="store_true")
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), positive_only=args.positive_only)
