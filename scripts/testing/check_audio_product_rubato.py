#!/usr/bin/env python3
"""Linear whole-file SRC aliased, and one default sinc stage failed large ratios.

Select bounded mature Rubato stages on the original rational clip clock. Compare
complete PCM and spectrum; execute fresh runtime assertions for faults. Device
and legacy progressive DSP are separate migrations, never a shipping claim.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import tomllib
import tempfile
import tarfile

import check_audio_product_wave as wave
whole = wave.whole
ROOT = whole.ROOT
TESTS = 'resample_job::tests::'


def verify_dsp(work, cargo_home):
    ledger = json.loads((ROOT / 'engine/third_party/rubato-sources.json').read_text())
    assert ledger['rubato']['version'] == '5.0.1' and len(ledger) == 7
    proof = {}
    patches = ROOT / 'engine/third_party/rubato-source-patches'
    declared = {p for row in ledger.values() for p in row.get('source_patches', [])}
    assert declared == {p.name for p in patches.glob('*.patch')}
    with tempfile.TemporaryDirectory(prefix='migo-dsp-source-') as temporary:
        stage=Path(temporary)
        for name,row in ledger.items():
            label=name+'-'+row['version']
            cached=next((cargo_home/'registry/src').glob('*/'+label))
            published=whole.transport.published_source(cached,cargo_home,dict(name=name,version=row['version']))
            assert published['package']==row['archive_sha256'] and published['files']==row['files']
            archive=next((cargo_home/'registry/cache').glob('*/'+label+'.crate'))
            with tarfile.open(archive) as stream:stream.extractall(stage,filter='data')
        for patch in sorted(patches.glob('*.patch')):
            whole.support.codecs.run(['patch','--batch','--forward','--fuzz=0','-p0','-i',str(patch)],work/(patch.stem+'-replay.log'),cwd=stage)
        for name,row in ledger.items():
            label=name+'-'+row['version'];directory=ROOT/'engine/third_party'/label
            inventory=lambda base:{str(p.relative_to(base)):whole.support.codecs.sha(p)for p in base.rglob('*')if p.is_file()}
            expected=inventory(stage/label);assert inventory(directory)==expected,name
            proof[name]=dict(upstream=row,root_files=expected)
    (work/'dsp-source-proof.json').write_text(json.dumps(proof,indent=2)+'\n')


def declarations(originals):
    rows = []
    job, stream, pipe = 'resample_job.rs', 'bandlimited.rs', 'streaming_sinc.rs'
    exact = TESTS + 'cascaded_rates_keep_the_original_ceil_clock_and_all_reference_pcm'
    spectrum = TESTS + 'bandlimiting_keeps_passband_and_rejects_aliases_at_extreme_downsample_ratios'
    cancel = TESTS + 'cancellation_releases_input_output_and_never_publishes_a_partial_buffer'
    quota = TESTS + 'setup_stages_and_total_work_errors_are_terminal_and_release_owned_pcm'
    bounds = TESTS + 'output_fixed_quanta_obey_actual_library_and_copy_budgets'
    pointer = TESTS + 'converted_pcm_keeps_its_reserved_pointer_without_allocating_through_flush'
    def change(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift', before)
        rows.append((name, path, before, after, test))
    change('src-cancel-ignored', job, 'if cancel.load(Ordering::Acquire) {', 'if false {', cancel)
    change('src-cancel-retains-allocation', job, 'self.phase = Phase::Cancelled;',
           'return Err(EngineError::from_detail(ErrorCode::Cancelled,"injected"));', cancel)
    change('src-cascade-disabled', pipe, 'while u64::from(current) > 4 * u64::from(output) {', 'while false {', spectrum)
    original = originals[pipe]
    initial = '            if let Some(end) = self.endpoint(target)? {\n                filter = filter.with_output_end(end);\n            }\n'
    begin = original.index('            for index in 0..self.layers.len() {')
    end = original.index('        if self.is_preparing() {', begin)
    binding = original[begin:end].rsplit('        }\n', 1)[0]
    assert original.count(initial) == 1 and original.count(binding) == 1
    change('src-original-clock-lost', pipe, original, original.replace(initial, '').replace(binding, ''), exact)
    begin = original.index('        if self.is_preparing() {')
    end = original.index('        if self.rates.is_empty() {', begin)
    setup = original[begin:end]
    changed = setup.replace('if self.is_preparing()', 'while self.is_preparing()').replace('            return Ok(Step {\n                consumed: 0,\n                progress: Progress::Yield,\n            });\n', '')
    change('src-setup-drains-all-layers', pipe, setup, changed, quota)
    # Double advance changes actual work, not just an accounting counter.
    change('src-two-operations-per-step', job, 'let result = self.advance(cancel);',
           'let result = self.advance(cancel).and_then(|done| if done { Ok(true) } else { self.advance(cancel) });', bounds)
    change('src-sample-budget-doubled', pipe, 'pub(crate) const STEP_SAMPLES: usize = 8192;',
           'pub(crate) const STEP_SAMPLES: usize = 16384;', bounds)
    change('src-total-work-unlimited', job, 'if *steps > MAX_STEPS {', 'if false {', quota)
    change('src-terminal-code-lost', job, 'Phase::Failed(error.code)', 'Phase::Failed(ErrorCode::Internal)', quota)
    change('src-completed-pcm-cloned', job, 'samples: std::mem::take(&mut output.samples),',
           'samples: output.samples.clone(),', pointer)
    change('src-delay-retained', stream, 'let skip = self.delay.min(produced);', 'let skip = 0;', exact)
    change('src-real-partial-tail-discarded', stream, 'Indexing::new().partial_len(input_frames)',
           'Indexing::new().partial_len(0)', exact)
    change('src-output-duration-floored', job,
           '(input.frame_count() as u128 * target_rate as u128).div_ceil(input.sample_rate as u128)',
           '(input.frame_count() as u128 * target_rate as u128) / input.sample_rate as u128', exact)
    change('src-output-ceiling-ignored', job,
           'if samples > crate::limits::MAX_AUDIO_PCM_SAMPLES as u128 {', 'if false {',
           TESTS + 'bypass_preserves_the_allocation_and_invalid_pcm_is_rejected_before_output_alloc')
    change('src-heap-in-processing', stream, 'let result = self.step_inner(input, eof, out);',
           'std::hint::black_box(vec![0_u8;64]);\n        let result = self.step_inner(input, eof, out);', pointer)
    change('src-reused-filter-history-retained', stream, 'owner.reset_from(0, 0)?;', 'let _ = &owner;',
           TESTS + 'constructing_an_owner_resets_reused_library_history_and_phase')
    incremental = 'streaming_sinc::tests::'
    change('stream-src-cancel-ignored', pipe, 'if cancel.load(Ordering::Acquire) {', 'if false {',
           incremental + 'incremental_cancellation_frees_all_scratch_and_is_sticky')
    change('stream-src-cancel-retains-scratch', pipe, 'self.layers = Vec::new();\n        self.rates = Vec::new();',
           'let _ = &self.layers;\n        self.rates = Vec::new();',
           incremental + 'incremental_cancellation_frees_all_scratch_and_is_sticky')
    change('stream-src-end-binding-lost', pipe, binding, '',
           incremental + 'unknown_partitions_match_every_raw_library_sample_and_original_clock')
    change('stream-src-terminal-code-lost', pipe, 'self.error = Some(code);', 'self.error = Some(ErrorCode::Internal);',
           incremental + 'incremental_cancellation_frees_all_scratch_and_is_sticky')
    change('stream-src-partial-frames-accepted', pipe, '!input.len().is_multiple_of(self.channels)', 'false',
           incremental + 'invalid_frames_early_eof_capacity_and_clock_overflows_retire_the_owner')
    change('stream-src-whole-file-limit-applied', pipe, '        use bandlimited::{Progress, Step};',
           '        use bandlimited::{Progress, Step};\n        if self.input_frames > crate::limits::MAX_AUDIO_PCM_SAMPLES { return Err(invalid("whole file limit")); }',
           incremental + 'a_continuous_stream_has_no_whole_file_pcm_or_total_step_limit')
    change('stream-src-endpoint-retraction-allowed', stream, 'self.eof || self.finished || frames < self.output_frames', 'self.eof || self.finished',
           incremental + 'endpoint_binding_rejects_retraction_and_cancel_overrides_completion')
    return rows


def build(package, work, env, label, release=False):
    lib=package/'src/lib.rs';lib.write_text(lib.read_text())
    return whole.build_candidate(package,work,env,label,release)


def main(work, target, cargo_home, candidate, minimal, preflight):
    if candidate:
        factory = whole.prepare_candidate if minimal else wave.duration.bench.prepare
        package, env, frozen, libraries, resampling = factory(work, target, cargo_home)
        expected = 583 if minimal else 600
    else:
        package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs())
        expected = 567
    verify_dsp(work, cargo_home)
    files = {name: package / 'src' / name for name in ['resample_job.rs','bandlimited.rs','streaming_sinc.rs']}
    originals = {name: p.read_text() for name,p in files.items()}
    rows = declarations(originals)
    programs, controls, positives = [], [], {}
    def compile_program(label, release=False):
        return build(package,work,env,label,release)
    def record(name, binary, role, prune=False):
        programs.append(dict(name=name, sha256=whole.support.codecs.sha(binary), fresh_rust=True, role=role, pruned=prune))
        if prune: binary.unlink()
    for profile in (['release'] if preflight else ['release','debug']):
        label='positive-'+profile;binary=compile_program(label, profile=='release');log=work/(label+'-tests.log')
        assert whole.support.clock.common.run([str(binary)],log,package,env,timeout=900)==0,log
        assert f'test result: ok. {expected} passed; 0 failed; 2 ignored;' in log.read_text(),log
        assert whole.support.clock.common.run([str(binary),'--list'],work/(label+'-inventory.log'),package,env)==0
        inventory=(work/(label+'-inventory.log')).read_text();assert all(test+': test'in inventory for _,_,_,_,test in rows)
        record(label,binary,'full-positive');positives[profile]=dict(passed=expected,ignored=2)
    if not candidate and not preflight:
        for name,path,before,after,test in rows:
            try:
                files[path].write_text(originals[path].replace(before,after))
                binary=compile_program(name,True);log=work/(name+'-test.log')
                status=whole.support.clock.common.run([str(binary),'--exact',test,'--nocapture'],log,package,env,timeout=120)
                text=log.read_text();assert status==101 and'running 1 test'in text and'panicked at'in text and'1 failed'in text,(name,status,log)
                record(name,binary,'runtime-assertion',True);controls.append(dict(name=name,test=test,exit=status,binary_sha256=programs[-1]['sha256']))
                print('actual SRC assertion rejected:',name,flush=True)
            finally: files[path].write_text(originals[path])
    whole.verify_inputs(work,frozen)
    assert all(p.read_text()==originals[name]for name,p in files.items())
    expected_lib=(ROOT/'engine/crates/audio/src/lib.rs').read_bytes()
    if candidate: expected_lib+=(work/'lib-addition.txt').read_bytes()
    assert (package/'src/lib.rs').read_bytes()==expected_lib
    assert all((package/'src'/p.relative_to(ROOT/'engine/crates/audio/src')).read_bytes()==p.read_bytes()
               for p in(ROOT/'engine/crates/audio/src').rglob('*')if p.is_file()and p.name!='lib.rs')
    if candidate: assert all(whole.support.codecs.sha(work/p)==h for p,h in json.loads((work/'retained-sources.json').read_text()).items())
    if not candidate:
        assert whole.support.clock.common.run(['cargo','tree','--offline','--locked','-e','normal','-p','migo-audio'],work/'runtime-tree.log',package,env)==0
        tree=(work/'runtime-tree.log').read_text();assert 'realfft v'not in tree and'hound v'not in tree
    summary=dict(profiles=positives,controls=controls,compiled_controls=len(controls),programs=programs,
                 root_inputs_unchanged=True,source_copies_restored=True,product_whole_file_sinc_selected=True,
                 device_and_legacy_progressive_dsp_migrated=False,legacy_progressive_dsp_migrated=True,device_dsp_migrated=False,production_pool_migrated=True,shared_incremental_sinc_pipeline=True,
                 candidate=candidate,minimal=minimal,preflight=preflight)
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('SRC complete',positives,len(controls),flush=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--rubato-home',type=Path,default=Path('/tmp/migo-w09-rubato-cargo'))
    parser.add_argument('--candidate',action='store_true');parser.add_argument('--minimal',action='store_true');parser.add_argument('--preflight',action='store_true')
    a=parser.parse_args();assert not a.minimal or a.candidate
    main(a.output.resolve(),a.target_dir.resolve(),a.rubato_home.resolve(),a.candidate,a.minimal,a.preflight)
