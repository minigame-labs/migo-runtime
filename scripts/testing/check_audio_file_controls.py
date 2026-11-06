#!/usr/bin/env python3
"""Actual sealed File M4A controls: complete PCM, source ACK and original CPU.

Offline build and immutable SHA-bound execution are separate. Execute needs
loopback for inherited HTTP tests. No Git or snapshot blessing is performed.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, shutil, subprocess
from pathlib import Path
import check_audio_sinc_origin as prior
from collections import Counter

index = prior.index
ROOT = index.ROOT
PREFIX = 'streaming_file::tests::controls::'
SEEK = PREFIX + 'public_file_seek_restart_and_exact_endpoint_preserve_complete_pcm'
LOOP = PREFIX + 'public_loop_cutovers_keep_the_global_sinc_grid_and_finish_the_consumed_cycle'
PNS = PREFIX + 'pns_file_seeks_match_independent_mature_pull_seek_instead_of_continuous_noise'
ACK = PREFIX + 'refusal_and_source_ack_preserve_old_revision_until_the_consumer_accepts'
DEADLINE = PREFIX + 'one_absolute_control_deadline_covers_verified_media_read_before_ack'
REVOKED = PREFIX + 'cached_loops_revalidate_the_original_file_capability_without_new_io'
LATE = PREFIX + 'revocation_between_preparation_and_ack_keeps_the_old_revision'
PHYSICAL = 'streaming_codecs::packet::control::tests::eof_retains_only_bounded_replay_buffers_and_original_cpu_really_frees_them'


def compare_clippy(old, current):
    raw_new, raw_removed = current-old, old-current
    effective = raw_new.copy()
    groups = []
    # Real controls now consume three members of an existing unused group.
    # Keep both raw Counters and permit only this exact reduction of members.
    before = [k for k in raw_removed if k[2:4] == ('dead_code', 'methods `output_start`, `input_origin`, `source_origin`, `input_frames`, and `cycles` are never used')]
    after = [k for k in raw_new if k[2:4] == ('dead_code', 'methods `source_origin` and `input_frames` are never used')]
    if before or after:
        assert len(before) == len(after) == 1
        a, z = before[0], after[0]
        assert raw_removed[a] == raw_new[z] == 1
        assert a[1] == z[1] == 'warning' and set(a[0]) == set(z[0]) == {'src/streaming.rs'}
        assert set(z[4]) < set(a[4])
        effective.subtract({z: 1})
        effective += Counter()
        groups.append(dict(before=a, after=z, reason='Actual File controls consume output_start/input_origin/cycles; the two remaining unused members were already in the original group. No new unused member or allow.'))
    delta = dict(before=sum(old.values()), after=sum(current.values()), new_diagnostics=sum(effective.values()), new=[dict(key=k, count=v) for k,v in effective.items()], raw_counter_new=[dict(key=k, count=v) for k,v in raw_new.items()], raw_counter_removed=[dict(key=k, count=v) for k,v in raw_removed.items()], reworded_original_dead_code_groups=groups)
    assert not effective, delta
    return delta


def controls(package):
    rows = []
    def add(name, path, before, after, test, panic):
        source = (package / 'src' / path).read_text()
        assert before != after and source.count(before) == 1, (name, 'anchor drift')
        rows.append(dict(name=name, path=path, before=before, after=after, test=test, expected_assertion=panic))
    add('file-grid-forgotten-after-eof', 'streaming_codecs/packet_control.rs', 'let period = owner.grid;', 'let period = 1;', SEEK, 'actual controlled file emitted Error')
    add('file-codec-reset-omitted', 'decoder/aac/incremental.rs', 'decoder.reset();', 'let _ = decoder;', PNS, 'controlled file complete suffix PCM')
    add('file-aac-preroll-published', 'decoder/aac/incremental.rs', 'let block = block.slice(skip..);', 'let _ = skip; let block = block.slice(0..);', ACK, 'retained file revision error:')
    add('file-consumer-ack-skipped', 'streaming_source_control.rs', 'tokio::time::timeout_at(deadline, receipt)', 'tokio::time::timeout_at(deadline, async { drop(receipt); Ok::<(), oneshot::error::RecvError>(()) })', ACK, 'file published a new revision before consumer ACK')
    add('file-media-deadline-renewed', 'streaming_file_control.rs', '.resolve_request(request, decoder, context.tx, context.state, deadline)', '.resolve_request(request, decoder, context.tx, context.state, deadline + std::time::Duration::from_secs(30))', DEADLINE, 'file control media read renewed or ignored its original deadline')
    add('file-loop-src-reset-each-cycle', 'streaming_codecs/packet.rs', 'window.local = 0;', 'window.local = 0; owner.pipeline = Some(Stream::new(owner.decoder.rate, owner.target, owner.decoder.channels, None)?);', LOOP, 'controlled file complete suffix PCM')
    add('file-bounded-history-discarded', 'streaming_codecs/packet.rs', 'self.owner.as_mut().unwrap().decoder.retain_history();', '// original bounded input is not retained', PHYSICAL, 'EOF discarded the original bounded encoded window')
    add('file-eof-drops-original-owner', 'streaming_codecs/packet.rs', 'if done && !self.retained {', 'if done {', PHYSICAL, 'EOF dropped the retained source decoder')
    before = '''        if file.as_ref().is_some_and(|file| file.is_closed()) {
            return Err(io_error());
        }'''
    after = '''        if false {
            return Err(io_error());
        }'''
    source = (package / 'src/streaming_file.rs').read_text()
    # Each CPU step checks the capability on both sides of decode.
    second = '''        if file.as_ref().is_some_and(|file| file.is_closed()) {
            pcm.clear();
            return Err(io_error());
        }'''
    assert source.count(before) == source.count(second) == 1
    rows.append(dict(name='file-cached-capability-unchecked', path='streaming_file.rs', before=source, after=source.replace(before, after).replace(second, second.replace('file.as_ref().is_some_and(|file| file.is_closed())', 'false')), test=REVOKED, expected_assertion='cached loop ignored revoked original source capability'))
    add('file-capability-not-rechecked-after-ack', 'streaming_source_control.rs', '''        source.check(context.state, deadline)?;
        let (_, channels)''', '''        let (_, channels)''', LATE, 'revoked File published a new revision after consumer ACK')
    add('file-old-source-suffix-not-restarted', 'streaming_file_control.rs', 'let restart = request.is_some() || terminal;', 'let restart = terminal;', ACK, 'retained file revision error:')
    add('file-typed-cause-not-recorded-before-publication', 'streaming_transport.rs', 'controls.failed(error);', '// typed source cause is not recorded', 'streaming_file::tests::native_file_readiness_keeps_typed_errors_and_source_refusal_without_duplicate_events', 'malformed M4A lost its typed format error')
    return rows


def build(work, target, before, before_clippy):
    os.environ['CARGO_INCREMENTAL'] = '0'
    base = index.base
    package, env, frozen = base.prepare(work, target)
    common = base.capacity.product.whole.support.clock.common
    for name in [index.FIXTURE, str(Path(__file__).relative_to(ROOT))]:
        frozen[name] = common.sha(ROOT / name)
        dest = work / 'source-inputs' / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, dest)
    dest = work / index.FIXTURE
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / index.FIXTURE, dest)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    output = package / 'src/output.rs'
    saved = output.read_text()
    try:
        output.write_text(index.backend.ports(saved))
        assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=600) == 0
    finally:
        output.write_text(saved)
    old = index.wave.diagnostics(before_clippy, before_clippy.parent / 'probe')
    current = index.wave.diagnostics(work / 'clippy.jsonl', package)
    delta = compare_clippy(old, current)
    (work / 'clippy-delta.json').write_text(json.dumps(delta, indent=2) + '\n')
    print('Stable File control Clippy', delta, flush=True)
    programs, steps = [], []
    def binary(name, release=False):
        path = base.capacity.product.src.build(package, work, env, name, release)
        row = dict(name=name, path=str(path), sha256=common.sha(path), fresh=False)
        programs.append(row)
        return row
    for release, label in [(False, 'debug'), (True, 'release')]:
        row = binary('positive-' + label, release)
        steps.append(dict(label='full-' + label, program=row, expected=0))
        for tag, test, dump in [('control-pcm', PREFIX, 'control'), ('origin-pcm', prior.WINDOW, 'origin'), ('local-pcm', index.NATIVE, 'local'), ('file-pcm', index.FULL, 'file'), ('indexed-pcm', index.INDEX_FULL, 'indexed'), ('indexed-large', index.INDEX_LARGE, 'indexed'), ('package-small', prior.product.SMALL, 'package'), ('package-large', prior.product.LARGE, 'package')]:
            steps.append(dict(label=tag+'-'+label, program=row, expected=0, test=test, dump=dump, exact=tag != 'control-pcm'))
        steps.append(dict(label='large-file-'+label, program=row, expected=0, test=index.LARGE))
    actor = package / 'src/audio_thread.rs'
    source = actor.read_text()
    old_actor = (before / 'engine/crates/audio/src/audio_thread.rs').read_text()
    assert old_actor != source
    try:
        actor.write_text(old_actor)
        row = binary('original-file-single-pass-runtime')
        steps.append(dict(label='original-file-single-pass-runtime', program=row, expected=101, test=SEEK, expected_assertion='file seek published before original source ACK', role='original_runtime_red'))
    finally:
        actor.write_text(source)
    for fault in controls(package):
        path = package / 'src' / fault['path']
        source = path.read_text()
        try:
            path.write_text(source.replace(fault['before'], fault['after']))
            row = binary(fault['name'])
            steps.append(dict(label=fault['name'], program=row, expected=101, test=fault['test'], expected_assertion=fault['expected_assertion'], role='fresh_source_fault'))
            print('Fresh File control fault prepared', fault['name'], flush=True)
        finally:
            path.write_text(source)
    base.capacity.product.whole.verify_inputs(work, frozen)
    for name in ['audio', 'shared', 'io', 'services', 'frame-wire']:
        src = ROOT / 'engine/crates' / name / 'src'
        private = package / 'src' if name == 'audio' else package / 'local-deps' / name / 'src'
        assert all(p.read_bytes() == (private / p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()), name
    (work / 'execution-plan.json').write_text(json.dumps(dict(programs=programs, steps=steps, clippy=delta, source_inputs=frozen, private_sources_restored=True), indent=2) + '\n')
    print('Immutable File control plan prepared', flush=True)


def execute(work, baseline):
    plan = json.loads((work / 'execution-plan.json').read_text())
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    for name, digest in plan['source_inputs'].items():
        assert sha(ROOT/name) == sha(work/'source-inputs'/name) == digest, name
    suites, faults, reds = {}, [], []
    for step in plan['steps']:
        program = step['program']
        binary = Path(program['path'])
        assert sha(binary) == program['sha256']
        env = os.environ.copy()
        env['RUST_TEST_THREADS'] = '1'
        if step.get('dump'):
            env[dict(control='MIGO_DUMP_FILE_CONTROL_PCM', origin='MIGO_DUMP_SINC_ORIGIN_PCM', local='MIGO_DUMP_LOCAL_FILE_PCM', file='MIGO_DUMP_FILE_STREAM_PCM', indexed='MIGO_DUMP_INDEXED_FILE_PCM', package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]] = '1'
        cmd = [str(binary)]
        if step.get('test'):
            cmd += (['--exact'] if step.get('exact', True) else []) + [step['test'], '--nocapture']
        with (work/(step['label']+'.log')).open('w') as log:
            result = subprocess.run(cmd, cwd=work/'probe', env=env, stdout=log, stderr=subprocess.STDOUT, timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(result.returncode)+'\n')
        text = (work/(step['label']+'.log')).read_text()
        assert result.returncode == step['expected'], (step['label'], result.returncode)
        if result.returncode:
            assert all(s in text for s in ['running 1 test', '1 failed', 'panicked at', step['test'], step['expected_assertion']]), step['label']
            (faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'], test=step['test'], expected_assertion=step['expected_assertion'], exit=101, sha256=program['sha256']))
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', text)
            assert match
            suites[step['label']] = dict(passed=int(match[1]), ignored=int(match[2]))
        print('Executed', step['label'], result.returncode, flush=True)
    pcm = {}
    for label, prefix, count in [('control-pcm','MIGO_FILE_CONTROL_PCM ',131), ('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36), ('local-pcm','MIGO_LOCAL_FILE_PCM ',12), ('file-pcm','MIGO_FILE_STREAM_PCM ',8), ('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40), ('indexed-large','MIGO_INDEXED_FILE_PCM ',10), ('package-small','MIGO_PACKAGE_FILE_PCM ',12), ('package-large','MIGO_PACKAGE_INDEXED_PCM ',10)]:
        a = index.pcm(work/(label+'-debug.log'), prefix, count)
        b = index.pcm(work/(label+'-release.log'), prefix, count)
        assert a == b, (label, 'dev/O3 bits differ')
        if label != 'control-pcm':
            assert a == index.pcm(baseline/(label+'-debug.log'), prefix, count), (label, 'previous full PCM changed')
        pcm[label] = dict(outputs=len(a), samples=sum(map(len,a.values())), all_dev_o3_bits_equal=True, previous_product_bits_unchanged=label!='control-pcm')
    large = {}
    for label in ['debug', 'release']:
        match = re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)', (work/('large-file-'+label+'.log')).read_text())
        assert match and int(match[1]) == 16777345 and int(match[2]) == 1048576000 and int(match[3]) < 4*1024*1024
        large[label] = dict(samples=int(match[1]), bits=int(match[2]), retained_pcm_bytes=int(match[3]), every_sample_checked=True, tail_and_end_checked=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'], suites=suites, controls=faults, original_red=reds, clippy=plan['clippy'], pcm=pcm, large_file=large, baseline_pcm_path=str(baseline), source_inputs_unchanged=True, private_sources_restored=True), indent=2)+'\n')
    print('Actual File controls and complete PCM verified', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--before-clippy', type=Path)
    parser.add_argument('--baseline-pcm', type=Path)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    if args.execute:
        assert args.baseline_pcm
        execute(args.output.resolve(), args.baseline_pcm.resolve())
    else:
        assert all([args.target_dir, args.before_source, args.before_clippy])
        build(args.output.resolve(), args.target_dir.resolve(), args.before_source.resolve(), args.before_clippy.resolve())
