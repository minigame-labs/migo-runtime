#!/usr/bin/env python3
"""Unknown Vorbis/ADTS lengths formerly required pages plus a final PCM copy.

Use the mature packetizers' trimmed durations within explicit preflight budgets,
then reserve once. Large headers or long scans retain the ordinary paged path.
Run the full candidate in both profiles and actual rebuilt assertion controls;
compiler errors, zero tests, signals and timeouts cannot qualify as rejection.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_known_pcm as known

whole = known.whole
bench = known.bench


def declarations(originals):
    rows = known.candidate_declarations(originals['whole_file.rs'])
    prefix = 'whole_file_candidate::duration::tests::'
    exact = prefix + 'trimmed_library_durations_keep_independent_frame_counts_and_exact_pcm'
    budgets = prefix + 'startup_byte_and_operation_limits_fall_back_without_codec_work_or_pcm_loss'
    invalid = prefix + 'quota_and_corrupt_truncated_or_chained_input_fail_before_pcm_reservation'
    def change(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    path = 'whole_file_duration.rs'
    change('duration-vorbis-trim-count-incremented', path, '.checked_add(event.frames)',
           '.checked_add(event.frames + u64::from(event.frames != 0))', exact)
    change('duration-startup-byte-cap-expanded', path, 'const STARTUP_BYTES: usize = 65536;',
           'const STARTUP_BYTES: usize = 4 * 1024 * 1024;', budgets)
    change('duration-startup-operation-cap-expanded', path, 'const STARTUP_STEPS: u32 = 128;',
           'const STARTUP_STEPS: u32 = 256;', budgets)
    change('duration-total-operation-cap-expanded', path, 'const SCAN_STEPS: u32 = 4096;',
           'const SCAN_STEPS: u32 = 8192;', budgets)
    change('duration-output-quota-deferred', path, '.filter(|n| *n <= limit)', '.filter(|_| true)', invalid)
    change('duration-end-publishes-an-incomplete-scan', path, 'if event.ended {\n            if self.offset',
           'if event.ended || self.frames > 0 {\n            if self.offset', exact)
    change('duration-disabled-in-whole-job', 'whole_file.rs', 'scan: duration::Scan::new(format),', 'scan: None,', exact)
    change('duration-yield-drains-next-operation', 'whole_file.rs', 'duration::Outcome::Yield => {}',
           'duration::Outcome::Yield => { self.phase = Phase::Decode(owner); return self.advance(cancel); },', exact)
    change('duration-preflight-allocates-as-unknown', 'whole_file.rs',
           'PcmCollector::with_expected_samples(Some(samples))', 'PcmCollector::new()', exact)
    change('duration-cancel-skips-scan-owner', 'whole_file.rs', 'if cancel.load(Ordering::Acquire) {',
           'if cancel.load(Ordering::Acquire) && !matches!(&self.phase, Phase::Decode(owner) if owner.scan.is_some()) {',
           prefix + 'cancellation_during_scan_at_completion_and_reservation_frees_owned_input')
    return rows


def main(work, target, rubato, preflight):
    package, env, frozen, libraries, resampling = bench.prepare(work, target, rubato)
    originals = {name: (work / 'source' / name).read_text() for name in ['whole_file.rs','whole_file_duration.rs']}
    rows = declarations(originals)
    profiles = {}
    for profile in (['debug'] if preflight else ['release','debug']):
        name = 'positive-' + profile
        binary = whole.build_candidate(package, work, env, name, release=profile == 'release')
        log = work / (name + '-tests.log')
        assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0, log
        assert re.search(r'test result: ok\. 430 passed; 0 failed; 2 ignored;',log.read_text())
        profiles[profile] = dict(passed=430,ignored=2)
        whole.support.program(work,name,'native')
    receipts = whole.support.faults(work,package,env,originals,[] if preflight else rows,
        lambda name: whole.build_candidate(package,work,env,name),work/'source','native')
    whole.verify_inputs(work,frozen)
    assert all(whole.support.codecs.sha(work / p) == h for p,h in json.loads((work/'retained-sources.json').read_text()).items())
    for row in json.loads((work/'library-source-provenance.json').read_text()):
        assert all(whole.support.codecs.sha(libraries[row['crate']]/p)==h for p,h in row['source_files'].items())
    for name,row in resampling.items():
        assert all(whole.support.codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary=dict(profiles=profiles,compiled_controls=len(receipts),new_controls=0 if preflight else len(rows)-21,
        controls=receipts,preflight=preflight,root_inputs_unchanged=True,source_copies_restored=True,
        product_codec_switched=False)
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print('duration preflight complete',profiles,len(receipts),flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--rubato-cargo-home',type=Path,required=True)
    parser.add_argument('--preflight',action='store_true')
    args=parser.parse_args()
    main(args.output.resolve(),args.target_dir.resolve(),args.rubato_cargo_home.resolve(),args.preflight)
