#!/usr/bin/env python3
"""Validate actual product AAC jobs, complete PCM and fresh runtime source faults.

No candidate-only decoder, network service, Git command or Root mutation is used.
Every fault must rebuild the actual private product and hit one named assertion.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import check_audio_command_ownership as base
import check_audio_oboe_callback as backend
import check_audio_waveshaper_storage as wave

ROOT = base.ROOT
PREFIX = 'decoder::aac::tests::'
PCM = PREFIX + 'every_product_sample_and_submitted_rate_matches_independent_pull_and_src'


def verify_provenance():
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    ledger = json.loads((ROOT / 'engine/third_party/symphonia-aac-sources.json').read_text())
    for name, row in ledger.items():
        folder = ROOT / 'engine/third_party' / (name + '-' + row['version'])
        actual = {str(p.relative_to(folder)): sha(p) for p in sorted(folder.rglob('*')) if p.is_file()}
        assert actual == row['source_files'], (name, 'published patched source inventory changed')
        for patch in row['patches']:
            assert sha(ROOT / patch['file']) == patch['sha256'], patch['file']
        identity = (ROOT / 'engine/third_party' / (name + '-patches') / 'UPSTREAM').read_text().split()
        assert identity == [name, row['version'], row['archive_sha256']], name
    folder = ROOT / 'engine/crates/audio/src/decoder/aac-fixtures'
    fixtures = json.loads((folder / 'sha256.json').read_text())
    assert all(sha(folder / name) == value for name, value in fixtures.items())
    return dict(vendors={name: len(row['source_files']) for name, row in ledger.items()}, frozen_fixture_files=len(fixtures))


def declarations(originals):
    rows = []
    def add(name, path, before, after, test, message):
        assert before != after and originals[path].count(before) == 1, (name, 'anchor drift')
        rows.append(dict(name=name, path=path, before=before, after=after,
                         test=test if '::' in test else PREFIX + test, expected_assertion=message))
    job = 'decoder/aac/mod.rs'
    source = 'decoder/aac/m4a_source.rs'
    cancel = 'cancellation_in_each_codec_source_collection_and_src_phase_releases_actual_input'
    malformed = 'malformed_sources_and_profiles_fail_closed_with_bounded_work'
    quota = 'declared_native_and_converted_quotas_and_work_errors_are_terminal'
    add('aac-dispatch-missing', 'decoder/whole.rs', 'super::is_adts(data) || super::is_m4a(data)',
        'false', 'decoder::whole::tests::aac_adts_reaches_the_product_dispatcher', 'product dispatcher rejected AAC-LC ADTS')
    add('aac-cancel-ignored', job, 'cancel.load(Ordering::Acquire)', 'false', cancel, 'AAC cancellation was ignored')
    add('aac-cancel-retains-input', job, 'self.phase = Phase::Failed(ErrorCode::Cancelled);',
        'return Err(EngineError::from_detail(ErrorCode::Cancelled,"injected"));', cancel, 'AAC cancellation retained the original input')
    add('aac-dispatch-cancel-not-forwarded', 'decoder/whole.rs', 'Self::Aac(job) => job.step(cancel),',
        'Self::Aac(job) => job.step(&AtomicBool::new(false)),',
        'decoder::whole::tests::cancellation_and_incomplete_finish_release_each_product_owner', 'product AAC cancellation was not forwarded')
    add('aac-pcm-silenced', job, 'std::convert::identity', '|_| 0.0', PCM, 'AAC complete native PCM changed')
    add('aac-copy-step-expanded', job, 'const COPY_SAMPLES: usize = 8192;', 'const COPY_SAMPLES: usize = 16384;',
        'each_input_and_pcm_copy_remains_bounded_across_long_indexed_sources', 'AAC step exceeded its bounded operation contract')
    add('aac-submitted-target-lost', job, 'ResampleJob::new(audio, rate)?', 'ResampleJob::new(audio, 44100)?',
        PCM, 'AAC submitted target changed')
    add('aac-work-budget-lost', job, 'if self.steps > MAX_STEPS {', 'if false {', quota, 'AAC work budget ignored')
    add('aac-converted-quota-lost', job, 'if output > self.limit as u128 {', 'if false {', quota, 'AAC converted quota ignored')
    add('aac-encoded-input-leaked', job, 'drop((input, codec, pcm));', 'std::mem::forget(input); drop((codec,pcm));',
        'incomplete_finish_never_publishes_and_input_retires_before_collection_or_src', 'AAC encoded input survived final collection')
    add('m4a-atom-budget-expanded', source, 'const MAX_ATOMS: u32 = 32;', 'const MAX_ATOMS: u32 = 64;',
        malformed, 'AAC malformed source published PCM')
    add('m4a-skipped-body-budget-expanded', source, 'const MAX_SKIPPED: u64 = 4 * 1024 * 1024;',
        'const MAX_SKIPPED: u64 = 8 * 1024 * 1024;', malformed, 'AAC metadata budget failed after the source scan')
    add('m4a-oversized-index-parsed', source, 'if end - start > IndexOptions::default().max_index_bytes as u64 {',
        'if false {', malformed, 'AAC oversized index reached its parse phase')
    add('m4a-presentation-trims-ignored', 'decoder/aac/m4a.rs', '.gapless(true)', '.gapless(false)',
        PCM, 'AAC product job failed its complete PCM or ownership contract')
    add('aac-incomplete-finish-published', job, 'Phase::Complete(audio) => Ok(audio),',
        'Phase::Decode(_) => Ok(DecodedAudio{samples:Vec::new(),sample_rate:48000,channels:1}),\n            Phase::Complete(audio) => Ok(audio),',
        'incomplete_finish_never_publishes_and_input_retires_before_collection_or_src', 'AAC incomplete finish published PCM')
    return rows


def pcm(log):
    rows = {}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_AAC_PCM '): continue
        _, name, rate, channels, values = line.split(' ', 4)
        assert name not in rows, name
        rows[name] = dict(rate=int(rate), channels=int(channels), bits=json.loads(values))
    assert len(rows) == 66, (log, len(rows))
    return rows


def main(work, target, before_clippy):
    provenance = verify_provenance()
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths = {name: package / 'src' / name for name in ['decoder/aac/mod.rs', 'decoder/aac/m4a_source.rs',
        'decoder/aac/m4a.rs', 'decoder/whole.rs']}
    originals = {name: p.read_text() for name, p in paths.items()}
    controls = declarations(originals)
    output = package / 'src/output.rs'
    saved = output.read_text()
    try:
        output.write_text(backend.ports(saved))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],
            work/'clippy.jsonl', package, env, timeout=600) == 0
    finally:
        output.write_text(saved)
    old = wave.diagnostics(before_clippy, before_clippy.parent / 'probe')
    new = wave.diagnostics(work/'clippy.jsonl', package)
    delta = dict(before=sum(old.values()), after=sum(new.values()), new_diagnostics=sum((new-old).values()),
        new=[dict(key=k,count=v) for k,v in (new-old).items()])
    (work/'clippy-delta.json').write_text(json.dumps(delta, indent=2)+'\n')
    assert not (new-old), delta
    print('Stable-source Clippy Counter has no new diagnostics', delta, flush=True)
    programs, suites, rejected = [], {}, []
    def build(name, release=False):
        binary = product.src.build(package, work, env, name, release)
        programs.append(dict(name=name,path=str(binary),sha256=common.sha(binary),pruned=False))
        return binary
    def run(binary, label, test=None, expected=0, message=None, dump=False):
        command = [str(binary)] + (['--exact',test,'--nocapture'] if test else [])
        selected = env.copy()
        if dump: selected['MIGO_DUMP_AAC_PCM'] = '1'
        code = common.run(command, work/(label+'.log'), package, selected, timeout=900)
        (work/(label+'-exit.txt')).write_text(str(code)+'\n')
        text = (work/(label+'.log')).read_text()
        assert code == expected, (label,code)
        if expected:
            assert all(x in text for x in ['running 1 test','panicked at','1 failed',test,message]), label
        else:
            result = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text)
            assert result, label
            suites[label] = dict(passed=int(result[1]),ignored=int(result[2]))
    binary = build('positive-debug')
    base.capacity.profile.verify(work/'positive-debug-build.jsonl', json.loads((work/'debug-profile-policy.json').read_text()), work/'debug-profile-artifacts.json')
    run(binary,'full-debug')
    run(binary,'complete-pcm-debug',PCM,dump=True)
    first = pcm(work/'complete-pcm-debug.log')
    for row in controls:
        path = paths[row['path']]
        try:
            path.write_text(originals[row['path']].replace(row['before'],row['after']))
            binary = build(row['name'])
            run(binary,row['name'],row['test'],101,row['expected_assertion'])
            rejected.append({k:v for k,v in row.items() if k not in ['before','after']} | dict(exit=101,sha256=programs[-1]['sha256']))
            programs[-1]['pruned'] = True
            binary.unlink()
            (work/'completed-controls.json').write_text(json.dumps(rejected,indent=2)+'\n')
            print('Fresh product AAC source fault rejected:',row['name'],flush=True)
        finally:
            path.write_text(originals[row['path']])
    binary = build('positive-release',True)
    run(binary,'full-release')
    run(binary,'complete-pcm-release',PCM,dump=True)
    assert pcm(work/'complete-pcm-release.log') == first, 'complete dev/O3 PCM bit comparison'
    product.whole.verify_inputs(work, frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        source = ROOT/'engine/crates'/name/'src'
        private = package/'src' if name=='audio' else package/'local-deps'/name/'src'
        assert all((private/p.relative_to(source)).read_bytes()==p.read_bytes() for p in source.rglob('*') if p.is_file()),name
    result = dict(programs=programs,suites=suites,controls=rejected,clippy=delta,provenance=provenance,
        pcm=dict(complete_outputs=len(first),complete_samples=sum(len(row['bits']) for row in first.values()),
            all_dev_o3_bits_equal=True,independent_pull_and_full_ffmpeg_adts_checked=True),
        root_inputs_unchanged=True,private_sources_restored=True,root_dev_profile_verified=True,
        isolated_o3_not_shipping=True)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n')
    print('AAC product native validation complete',len(rejected),'fresh source faults',flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--before-clippy',type=Path,required=True)
    args = parser.parse_args()
    main(args.output.resolve(),args.target_dir.resolve(),args.before_clippy.resolve())
