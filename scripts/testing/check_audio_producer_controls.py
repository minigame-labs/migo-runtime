#!/usr/bin/env python3
"""Prove one persistent source driver retains ownership across ordered controls.

The single-pass path once ended at EOF and could not accept a seek while PCM
filled its queue. Retain a shared driver, validate the real file/HTTP/decoder
handshake, and rebuild faults in backpressure, stale errors and owner storage.
Release positives and mostly debug fault programs use the same frozen sources.
Three 36-minute playback fixtures retain their optimized-profile watchdog; the
debug suite omits them. Their prefetch fault also runs optimized, with a passing
same-source baseline. Signals, process timeouts and compilation failures fail.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

import check_audio_mp4_file as file
import check_audio_source_seek as seek

ROOT = file.ROOT
codecs = file.codecs


def declarations(originals):
    rows = []
    def scoped(name, path, signature, before, after, test):
        source = originals[path]
        start = source.index(signature)
        closing = '\n    }' if signature.startswith('    ') else '\n}'
        end = source.index(closing, start) + len(closing)
        body = source[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, body, body.replace(before, after), test))
    def whole(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    prefix = 'producer_control::tests::'
    driver = 'streaming_transport.rs'
    control = 'producer_control.rs'
    owner = originals['off_worker.rs']
    inline = owner
    for before, after in [('value: Box<T>,', 'value: T,'), ('value: Box::new(value),', 'value,'), ('step(value.as_mut())', 'step(&mut value)')]:
        assert inline.count(before) == 1
        inline = inline.replace(before, after)
    rows.append(('inline-payload-in-every-handoff', 'off_worker.rs', owner, inline,
                 'off_worker::tests::handoff_future_size_is_independent_of_the_owned_payload'))
    whole('per-step-owner-allocation', 'off_worker.rs', '        let Self { mut value, cpu } = self;',
          '        std::hint::black_box(Box::new(0_u8));\n        let Self { mut value, cpu } = self;',
          'source_seek_contracts::cached_seek_adds_no_caller_allocation_beyond_one_existing_cpu_handoff')
    whole('rejected-control-loses-pending-pcm', driver, '        if let Some(request) = command.take() {',
          '        if let Some(request) = command.take() {\n            pending = None;', prefix+'rejected_seek_under_backpressure_keeps_the_exact_old_pending_pcm')
    whole('interrupted-read-not-recovered', driver, '                    controls.recover_read(&mut source)?;', '',
          prefix+'rejected_control_during_a_pending_http_body_recovers_the_exact_wait')
    whole('refused-control-renews-idle-timeout', driver, '        if let Some(request) = command.take() {',
          '        if let Some(request) = command.take() {\n            read_deadline = None;', prefix+'rejected_controls_do_not_renew_a_stalled_media_reads_idle_deadline')
    whole('eof-retires-source-owner', driver, '        if finished {\n            if !open {',
          '        if finished {\n            if true {', prefix+'file_and_http_keep_one_actor_for_eof_seek_restart_and_exact_pcm_suffixes')
    whole('eof-starts-idle-read-polling', driver, '        if finished {',
          '        if finished {\n            finished = false; need_read = true; continue;', prefix+'rejected_control_keeps_a_blocked_done_and_eof_wait_has_no_read_or_cpu_polling')
    source = originals[control]
    a = source.index('let accepted = while_active(')
    acceptance = source.index('if !matches!(accepted, Some(Ok(Ok(()))))', a)
    end = source.index('}', acceptance) + 1
    rows.append(('publication-before-consumer-acceptance', control, source[a:end], 'drop(receipt);',
                 prefix+'full_output_queue_accepts_seek_without_publishing_before_consumer_ack'))
    whole('acceptance-deadline-renewed', control, 'tokio::time::timeout_at(deadline, receipt)',
          'tokio::time::timeout_at(deadline + std::time::Duration::from_secs(10), receipt)', prefix+'prepared_acceptance_uses_the_original_absolute_deadline')
    scoped('consumer-source-identity-unchecked', 'inner_audio.rs', '    fn accept_stream_revision_as(',
           '!Arc::ptr_eq(current, source)', 'false', prefix+'prepared_receipt_checks_player_source_identity_before_clearing_pcm')
    scoped('ordinary-consumer-enables-persistent-producer', driver, 'pub(crate) async fn run_source_controlled<',
           'if !state.retains_source()', 'if false', prefix+'ordinary_consumers_cannot_accidentally_enable_a_persistent_source')
    scoped('accepted-error-uses-initial-revision', driver, 'pub(crate) async fn run_source_controlled<',
           'revision: publication.revision(),\n                message:', 'revision: crate::streaming::StreamRevision::INITIAL,\n                message:', prefix+'errors_after_an_accepted_seek_use_the_active_revision')
    whole('cached-control-reads-before-draining', driver, '            need_read = restart;', '            need_read = true;',
          prefix+'cached_eof_seek_drains_retained_decoder_before_reading_the_source_again')
    rows.append(('abandoned-acceptance-revives-mutated-owner', control, source[acceptance:end],
                 'if !matches!(accepted, Some(Ok(Ok(())))) { return Ok(Some(ControlResult::Unchanged(result.decoder))); }',
                 prefix+'abandoned_prepared_acceptance_cancels_the_source_instead_of_reusing_mutated_decoder'))
    scoped('control-receiver-skips-first-request', control, '    async fn next(',
           'self.requests.recv().await', '{ let _ = self.requests.recv().await; self.requests.recv().await }',
           prefix+'control_requests_are_processed_in_order_and_cannot_skip_an_acceptance')
    scoped('paused-player-drains-past-prefetch', 'inner_audio.rs', '    fn stream_prefetch_full(',
           'samples != 0 &&', 'false &&',
           'isomp4_source_contracts::transport::long_m4a_player_exceeds_whole_cache_quota_with_bounded_pcm')
    scoped('unpolled-expired-receipt-accepted', 'inner_audio.rs', '    fn accept_as(',
           'if tokio::time::Instant::now() >= self.deadline {', 'if false {',
           prefix+'an_expired_receipt_rejects_before_mutating_pcm_even_when_the_waiter_has_not_polled')
    return rows


def main(work, target, rubato, positive_only):
    env, frozen, _, libraries, resampling = file.prepare(work, target, rubato)
    env.update(CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    package = work/'probe'
    lib = package/'src/lib.rs'
    fixtures = ROOT/'scripts/testing/fixtures/audio-codec-probe'
    for name in ['inner_audio.rs','stream_revision_tests.rs','seek_tests.rs','forward_seek_tests.rs','stream_timeline_tests.rs']:
        original=ROOT/'engine/crates/audio/src'/name
        saved=work/'source'/name;shutil.copy2(original,saved)
        if name=='inner_audio.rs':
            lib.write_text(lib.read_text().replace(json.dumps(str(original)),json.dumps(str(saved))))
    for name in ['producer_control', 'decoder_seek_contracts', 'source_seek_contracts']:
        path = work/'source'/(name+'.rs');shutil.copy2(fixtures/path.name, path)
        prefix='#[cfg(test)] 'if name.endswith('contracts')else''
        lib.write_text(lib.read_text()+f'\n{prefix}#[path={json.dumps(str(path))}] mod {name};\n')
    shutil.copy2(fixtures/'producer_contracts.rs', work/'source/producer_contracts.rs')
    extras = list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_http_range_source.py', ROOT/'scripts/testing/check_gitignore_contract.py']
    extras += [ROOT/'engine/crates/audio/src'/name for name in ['inner_audio.rs','stream_revision_tests.rs','seek_tests.rs','forward_seek_tests.rs','stream_timeline_tests.rs']]
    for path in extras:
        rel=str(path.relative_to(ROOT));frozen[rel]=codecs.sha(path)
        saved=work/'source-inputs'/rel;saved.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,saved)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    retained={str(path.relative_to(work)):codecs.sha(path)for tree in [package/'src',work/'source']for path in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained,indent=2)+'\n')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'))
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];known={p['name']for p in locked};pinned={identity(p)for p in locked}
    assert all(p['name']not in known or identity(p)in pinned for p in tomllib.loads((package/'Cargo.lock').read_text())['package']),'dependency identity drift'
    originals={name:(work/'source'/name).read_text()for name in ['streaming_transport.rs','producer_control.rs','off_worker.rs','inner_audio.rs']}
    faults=declarations(originals)
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    checks=[('actor','producer_control::tests::',22),('owner','off_worker::tests::',4),('source-seek','source_seek_contracts::',10),('decoder-seek','decoder_seek_contracts::',9),('file','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('transport','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('range-interruption','network::http::range::interruption_tests::',6),('file-interruption','local_source::interruption_tests::',7),('adapter-interruption','isomp4_transport::interruption_tests::',3),('common','streaming_transport::tests::',3),('player','inner_audio::',61),('publication','streaming::revision_tests::',5)]
    positives={};binaries={}
    debug_skips={
        'file':'isomp4_source_contracts::transport::local::local_long_player_exceeds_cache_quota_without_accumulating_pcm',
        'package':'isomp4_source_contracts::transport::local::package::package_long_player_exceeds_cache_quota_without_accumulating_pcm',
        'transport':'isomp4_source_contracts::transport::long_m4a_player_exceeds_whole_cache_quota_with_bounded_pcm',
    }
    for profile,release in [('release',True),('debug',False)]:
        binary=file.dsp.source.build(work,env,'positive-'+profile,release=release);binaries[profile]=binary
        for label,prefix,count in checks:
            args=[str(binary),prefix,'--nocapture','--test-threads=1']
            if label=='file':args+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
            if label=='transport':args+=['--skip','isomp4_source_contracts::transport::local::']
            if label=='source':args+=['--skip','isomp4_source_contracts::transport::']
            if not release and label in debug_skips:
                args+=['--skip',debug_skips[label]];count-=1
            log=work/f'positive-{profile}-{label}.log';codecs.run(args,log,cwd=package,env=env)
            assert f'{count} passed; 0 failed'in log.read_text(),(profile,label)
        positives[profile]=sum(row[2]for row in checks)-(0 if release else len(debug_skips))
        print(profile,'positive groups passed',positives[profile],flush=True)
    # Changed Rust inputs must still emit a freshly compiled test executable.
    # Keeping Cargo's unaffected incremental units does not waive fresh=false
    # or the executed-failing-assertion requirement in the shared builder.
    build_debug=lambda w,e,label:file.dsp.source.build(w,e,label,release=False,clean=False)
    receipts=[]
    for name,path,before,after,test in ([]if positive_only else faults):
        source=work/'source'/path;assert originals[path].count(before)==1,(name,'ambiguous mutation')
        try:
            source.write_text(originals[path].replace(before,after))
            release=name=='paused-player-drains-past-prefetch'
            binary=file.dsp.source.build(work,env,name,release=release,clean=False);log=work/f'{name}-test.log'
            with log.open('w')as out:r=subprocess.run([str(binary),'--exact',test,'--nocapture'],cwd=package,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
            observed=log.read_text();assert r.returncode==101 and 'panicked at'in observed and '1 failed'in observed,(name,'requires executed assertion')
            receipts.append({'name':name,'test':test,'profile':'release'if release else'debug','exit':r.returncode,'binary_sha256':codecs.sha(binary)})
        finally:source.write_text(originals[path])
        print('compiled producer fault',name,': executed assertion rejected',flush=True)
    source_controls=0 if positive_only else seek.controls(work,env,build=build_debug)
    dsp_controls=0 if positive_only else file.dsp.controls(work,env,build=build_debug)
    assert all(codecs.sha(ROOT/path)==sha for path,sha in frozen.items()),'Root changed'
    assert all(codecs.sha(work/path)==sha for path,sha in retained.items()),'source copies not restored'
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/path)==sha for path,sha in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'producer_tests':22,'stable_owner_tests':4,'producer_compiled_controls':len(receipts),'producer_controls':receipts,'source_seek_controls':source_controls,'dsp_controls':dsp_controls,'compiled_controls':len(receipts)+source_controls+dsp_controls,'fault_profiles':'debug except optimized long-playback prefetch control','debug_optimized_only_tests':list(debug_skips.values()),'positive_profiles':['release','debug'],'positive_only':positive_only,'libraries':len(provenance),'patches':sum(len(row['patches'])for row in provenance),'frozen_inputs':len(frozen),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_identities_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'positive_binary_sha256':{p:codecs.sha(b)for p,b in binaries.items()},'retired_control':'copy-only source IO future no longer violates the frame bound after stable owner storage; inline payload and per-step owner allocation now have executed controls','scope':'One common single-pass/controlled source loop; ordered candidate file/HTTP seeks, EOF ownership, pending PCM/Done, source-bound consumer acceptance, original deadlines, stable CPU owner and revision errors. Global DSP phase/output clock/authoritative duration/public product cutover/native-device/peak remain open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
