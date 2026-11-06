#!/usr/bin/env python3
"""Bounded HTTP control reads into existing admitted buffers.

Separate offline builds from SHA-bound loopback execution. Preserve all prior
audio PCM; do not select HTTP M4A, bless snapshots, or perform Git operations.
"""
from __future__ import annotations
import argparse, json, os, re, shutil
from pathlib import Path
import check_audio_file_controls as prior

index = prior.index
ROOT = index.ROOT
OWNED = 'streaming_codecs::tests::range_buffer::admitted_http_index_uses_original_capacity_without_another_full_body_vec_and_frees_on_cpu'
PREFIX = 'network::http::range::borrowed_tests::'


def controls(source):
    rows = []
    def add(name, before, after, test, panic):
        assert source.count(before) == 1 and before != after, (name, 'anchor drift')
        rows.append(dict(name=name, before=before, after=after, test=PREFIX+test, expected_assertion=panic))
    add('range-borrowed-write-lost', 'output[at..at + bytes.len()].copy_from_slice(bytes);', 'let _ = (at, bytes);', 'borrowed_reads_fill_exact_windows_and_share_counters_with_vec_reads', 'borrowed range did not fill the complete target')
    add('range-borrowed-cancel-not-latched', 'self.failed = true;\n        let result = async {', 'let result = async {', 'dropped_borrowed_wait_retires_the_same_reader', 'dropped borrowed control future left its reader reusable')
    add('range-control-offset-lost', 'let start = offset + received as u64;', 'let start = received as u64;', 'borrowed_reads_fill_exact_windows_and_share_counters_with_vec_reads', 'borrowed control rejected a valid absolute source window')
    add('range-control-deadline-renewed', '.open_range(start, end, deadline, true)', '.open_range(start, end, Instant::now() + std::time::Duration::from_secs(2), true)', 'borrowed_short_responses_keep_one_absolute_deadline', 'borrowed control renewed the original deadline')
    add('range-control-byte-precheck-lost', '''            || self
                .bytes
                .checked_add(len as u64)
                .is_none_or(|n| n > self.limits.max_total_bytes)''', '', 'vec_and_borrowed_reads_cannot_mint_byte_or_request_credit', 'borrowed control preflight budget allowed network IO')
    add('range-control-early-success', 'while received < len {', 'while received < len.saturating_sub(4) {', 'borrowed_reads_fill_exact_windows_and_share_counters_with_vec_reads', 'borrowed range did not fill the complete target')
    return rows


def build(work, target, before, baseline):
    os.environ['CARGO_INCREMENTAL'] = '0'
    base = index.base
    package, env, frozen = base.prepare(work, target)
    common = base.capacity.product.whole.support.clock.common
    for name in [index.FIXTURE, str(Path(__file__).relative_to(ROOT))]:
        frozen[name] = common.sha(ROOT/name)
        dest = work/'source-inputs'/name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT/name, dest)
    dest = work/index.FIXTURE
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT/index.FIXTURE, dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    output = package/'src/output.rs'
    saved = output.read_text()
    try:
        output.write_text(index.backend.ports(saved))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'], work/'clippy.jsonl', package, env, timeout=600)==0
    finally:
        output.write_text(saved)
    a = index.wave.diagnostics(baseline/'clippy.jsonl', baseline/'probe')
    z = index.wave.diagnostics(work/'clippy.jsonl', package)
    assert not (z-a), z-a
    delta = dict(before=sum(a.values()), after=sum(z.values()), new_diagnostics=0, raw_removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    assert common.run(['cargo','clippy','-p','migo-services','--lib','--tests','--offline','--locked','--message-format=json'], work/'services-clippy.jsonl', package, env, timeout=600)==0
    service = index.wave.diagnostics(work/'services-clippy.jsonl', package)
    selected = {k:v for k,v in service.items() if any('network/http/range' in n for n in k[0])}
    assert not selected, selected
    delta['services_range_changed_sources'] = dict(before=0, after=0, new_diagnostics=0)
    (work/'clippy-delta.json').write_text(json.dumps(delta, indent=2)+'\n')
    print('Stable current range-buffer Clippy',delta,flush=True)
    programs, steps = [], []
    def binary(name, crate='migo-audio', release=False):
        if crate == 'migo-audio':
            path = base.capacity.product.src.build(package,work,env,name,release)
        else:
            (package/'local-deps/services/src/lib.rs').touch()
            command=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
            if release: command.append('--release')
            log=work/(name+'-build.jsonl')
            assert common.run(command,log,package,env,timeout=600)==0
            rows=[]
            for line in log.read_text().splitlines():
                try: row=json.loads(line)
                except ValueError: continue
                if row.get('reason')=='compiler-artifact' and row.get('executable') and row.get('target',{}).get('name')=='migo_services':rows.append(row)
            assert len(rows)==1 and rows[0]['fresh'] is False
            if release: assert rows[0]['profile']['opt_level']=='3'
            path=work/(name+'-tests')
            shutil.copy2(rows[0]['executable'],path)
        row=dict(name=name,path=str(path),sha256=common.sha(path),fresh=False)
        programs.append(row)
        return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release=release)
        steps.append(dict(label='full-'+label,program=row,expected=0))
        steps.append(dict(label='range-capacity-'+label,program=row,expected=0,test=OWNED))
        for tag,test,dump in [('control-pcm',prior.PREFIX,'control'),('origin-pcm',prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.product.SMALL,'package'),('package-large',prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
        row=binary('services-'+label,'migo-services',release)
        steps.append(dict(label='services-range-'+label,program=row,expected=0,test='network::http::range::',exact=False))
    path=package/'local-deps/services/src/network/http/range.rs'
    source=path.read_text()
    old=(before/'engine/crates/services/src/network/http/range.rs').read_text()
    shim='''
impl Reader {
    pub async fn read_exact_into(&mut self, offset:u64, output:&mut [u8], deadline:Instant)->Result<(),Error> {
        let bytes=self.read_exact_at(offset,output.len(),deadline).await?;
        output.copy_from_slice(&bytes);Ok(())
    }
}
'''
    (work/'original-range-runtime.rs').write_text(old)
    (work/'original-api-only-copy-shim.rs').write_text(shim)
    try:
        path.write_text(old+shim)
        row=binary('original-collecting-range-runtime')
        steps.append(dict(label='original-collecting-range-runtime',program=row,expected=101,test=OWNED,expected_assertion='range read allocated another full-size index buffer',role='original_runtime_red'))
    finally:
        path.write_text(source)
    for fault in controls(source):
        try:
            path.write_text(source.replace(fault['before'],fault['after']))
            row=binary(fault['name'],'migo-services')
            steps.append(dict(label=fault['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
            print('Fresh range-buffer fault prepared',fault['name'],flush=True)
        finally:
            path.write_text(source)
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src'
        private=package/'src' if name=='audio' else package/'local-deps'/name/'src'
        assert all(p.read_bytes()==(private/p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n')
    print('Immutable range-buffer execution plan prepared',flush=True)


def execute(work, baseline):
    prior.execute(work,baseline)
    path=work/'summary.json'
    result=json.loads(path.read_text())
    current=index.pcm(work/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    assert current==index.pcm(baseline/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    result['pcm']['control-pcm']['previous_product_bits_unchanged']=True
    capacity={}
    for label in ['debug','release']:
        text=(work/('range-capacity-'+label+'.log')).read_text()
        match=re.search(r'MIGO_RANGE_BUFFER actual_capacity=(\d+) data_length=(\d+) control_length=(\d+) max_poll_allocated=(\d+) original_cpu_free=true',text)
        assert match and tuple(map(int,match.groups()[:3]))==(4194304,2097216,2097152) and int(match[4])<1048576
        capacity[label]=dict(actual_capacity=int(match[1]),length=int(match[2]),control_bytes=int(match[3]),max_single_poll_allocated=int(match[4]),same_pointer=True,original_cpu_actual_free=True,capacity_credit_held_until_real_free=True)
    result['range_capacity']=capacity
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=873,ignored=3)
    assert result['suites']['services-range-debug']==result['suites']['services-range-release']==dict(passed=30,ignored=0)
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('Admitted HTTP range buffers, complete bytes, original CPU and previous PCM verified',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--target-dir',type=Path)
    p.add_argument('--before-source',type=Path)
    p.add_argument('--baseline-native',type=Path,required=True)
    p.add_argument('--execute',action='store_true')
    a=p.parse_args()
    if a.execute:execute(a.output.resolve(),a.baseline_native.resolve())
    else:
        assert a.target_dir and a.before_source
        build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.baseline_native.resolve())
