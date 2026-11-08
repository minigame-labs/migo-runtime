#!/usr/bin/env python3
"""Current production sinc: bounded absolute-grid windows and unchanged full PCM.

Build offline. --execute uses only SHA-bound test ELFs; old HTTP tests require
loopback permission. No Git, snapshot blessing or source rewrite during execute.
"""
from __future__ import annotations
import argparse,json,os,re,shutil,subprocess,hashlib
from pathlib import Path
import check_audio_package_file as product
index=product.index
ROOT=index.ROOT
PREFIX='streaming_sinc::origin_tests::'
WINDOW=PREFIX+'known_end_windows_match_the_original_full_track_grid_and_complete_tail'
GRID=PREFIX+'every_cascade_grid_is_checked_before_any_filter_is_built'
LARGE=PREFIX+'positions_above_f64_integer_precision_run_real_kernels_on_the_same_grid'
BYPASS=PREFIX+'bypass_discards_only_preroll_and_preserves_capacity_and_overflow_refusals'
EOF=PREFIX+'unknown_end_and_exact_endpoint_keep_a_separate_real_eof'

def controls(source):
    rows=[]
    def add(name,a,z,test,panic):
        assert a!=z and source.count(a)==1,(name,'anchor drift')
        rows.append(dict(name=name,before=a,after=z,test=test,expected_assertion=panic))
    add('sinc-input-origin-lost','input_frames: input_origin,','input_frames: 0,',WINDOW,'SRC input clock lost absolute origin')
    add('sinc-output-origin-lost','output_frames: output_start,','output_frames: 0,',WINDOW,'SRC output clock lost publication start')
    add('sinc-intermediate-grid-unchecked','for rate in &rates {','for rate in rates.last() {',GRID,'SRC accepted an origin off an intermediate grid')
    add('sinc-origin-uses-f64','let scaled = u128::from(origin) * u128::from(target);','let scaled = (origin as f64 * f64::from(target)) as u128;',LARGE,'absolute SRC lost integer-only origin arithmetic')
    add('sinc-origin-overflow-truncated','u64::try_from(scaled / u128::from(source)).map_err(|_| invalid("SRC absolute origin overflow"))','Ok((scaled / u128::from(source)) as u64)',GRID,'SRC accepted an invalid absolute window')
    add('sinc-filter-origin-reset-omitted','.reset_from(input_origin, output_start)','.reset_from(0, 0)',WINDOW,'absolute SRC rejected a complete window')
    add('sinc-final-preroll-published','let output_start = if index + 1 == self.rates.len() {','let output_start = if false {',WINDOW,'absolute SRC rejected a complete window')
    a='''                let output_start = if index + 1 == self.rates.len() {
                    self.output_start
                } else {
                    map_origin(self.input_origin, self.source, target)?
                };'''
    add('sinc-every-layer-discards-to-final-clock',a,'                let output_start = self.output_start;',WINDOW,'absolute SRC rejected a complete window')
    add('sinc-endpoint-rebased-to-window','u128::from(frames) * u128::from(rate)','u128::from(frames.saturating_sub(self.input_origin)) * u128::from(rate)',WINDOW,'absolute SRC suffix length changed')
    add('sinc-bypass-preroll-published','&input[discard..consumed]','&input[..consumed]',BYPASS,'bypass published its absolute preroll')
    add('sinc-input-boundary-invents-eof','self.finished = self.eof;','self.finished = self.eof || self.source_end == Some(frames);',EOF,'absolute SRC invented EOF at the publication boundary')
    return rows

def build(work,target,before,before_clippy):
    os.environ['CARGO_INCREMENTAL']='0';base=index.base;package,env,frozen=base.prepare(work,target);common=base.capacity.product.whole.support.clock.common
    for n in [index.FIXTURE,str(Path(__file__).relative_to(ROOT))]:
        frozen[n]=common.sha(ROOT/n);p=work/'source-inputs'/n;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/n,p)
    p=work/index.FIXTURE;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/index.FIXTURE,p);(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    out=package/'src/output.rs';saved=out.read_text()
    try:
        out.write_text(index.backend.ports(saved));assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:out.write_text(saved)
    a=index.wave.diagnostics(before_clippy,before_clippy.parent/'probe');z=index.wave.diagnostics(work/'clippy.jsonl',package);assert not(z-a),z-a
    delta=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,raw_removed=[dict(key=k,count=v) for k,v in (a-z).items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');print('Stable current sinc Clippy',delta,flush=True)
    programs=[];steps=[]
    def binary(name,release=False):
        p=base.capacity.product.src.build(package,work,env,name,release);row=dict(name=name,path=str(p),sha256=common.sha(p),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release);steps.append(dict(label='full-'+label,program=row,expected=0))
        for tag,test,dump in [('origin-pcm',WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',product.SMALL,'package'),('package-large',product.LARGE,'package')]:steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    path=package/'src/streaming_sinc.rs';source=path.read_text();faults=controls(source)
    old=(before/'engine/crates/audio/src/streaming_sinc.rs').read_text();assert 'fn new_at(' not in old
    shim='''
impl Stream {
    pub(crate) fn new_at(source:u32,target:u32,channels:u32,end:Option<u64>,origin:u64,start:u64)->EngineResult<Self> {
        let _ = (origin,start);Self::new(source,target,channels,end)
    }
}
#[cfg(test)]
#[path="streaming_sinc_origin_tests.rs"]
mod origin_tests;
'''
    (work/'before-sinc-runtime.rs').write_text(old);(work/'before-api-only-shim.rs').write_text(shim)
    try:
        path.write_text(old+shim);row=binary('original-zero-origin-runtime');steps.append(dict(label='original-zero-origin-runtime',program=row,expected=101,test=WINDOW,expected_assertion='SRC input clock lost absolute origin',role='original_runtime_red'))
    finally:path.write_text(source)
    for f in faults:
        try:
            path.write_text(source.replace(f['before'],f['after']));row=binary(f['name']);steps.append(dict(label=f['name'],program=row,expected=101,test=f['test'],expected_assertion=f['expected_assertion'],role='fresh_source_fault'));print('Fresh origin fault prepared',f['name'],flush=True)
        finally:path.write_text(source)
    # Historical harness regression: a real unrelated refund between the old
    # shared-counter reads must not become part of a bucket's own charge.
    lru=package/'src/cache_lru_tests.rs';current=lru.read_text();previous=(before/'engine/crates/audio/src/cache_lru_tests.rs').read_text()
    old_fee=previous[previous.index('fn fee('):previous.index('#[derive(Debug, PartialEq, Eq)]')].replace('fn fee(', 'fn old_fee(')
    anchor='    let b = l.used_bytes();'
    assert old_fee.count(anchor)==1
    old_fee=old_fee.replace(anchor,'    let pending = l.try_reserve(128, "controlled older owner").unwrap();\n'+anchor+'\n    drop(pending);')
    case='''
#[test]
fn shared_delta_refund_reproducer() {
    let (s,h,l)=setup();
    let expected=fee(&s,8192);
    let wrong=old_fee(&h,&l,8192);
    assert_eq!(wrong,expected,"LRU charge included another owner's real refund");
}
'''
    (work/'before-lru-fee.rs').write_text(old_fee)
    try:
        lru.write_text(current+'\n'+old_fee+case);row=binary('original-lru-shared-counter-race');steps.append(dict(label='original-lru-shared-counter-race',program=row,expected=101,test='cache::lru::tests::shared_delta_refund_reproducer',expected_assertion="LRU charge included another owner's real refund",role='original_harness_red'))
    finally:lru.write_text(current)
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src';assert all(p.read_bytes()==(private/p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable origin execution plan prepared',flush=True)

def execute(work,baseline):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest();env=os.environ.copy();env['RUST_TEST_THREADS']='1'
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};faults=[];reds=[]
    for step in plan['steps']:
        p=step['program'];binary=Path(p['path']);assert sha(binary)==p['sha256'];e=env.copy()
        if step.get('dump'):e[dict(origin='MIGO_DUMP_SINC_ORIGIN_PCM',local='MIGO_DUMP_LOCAL_FILE_PCM',file='MIGO_DUMP_FILE_STREAM_PCM',indexed='MIGO_DUMP_INDEXED_FILE_PCM',package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]]='1'
        cmd=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else [])
        with (work/(step['label']+'.log')).open('w') as log:q=subprocess.run(cmd,cwd=work/'probe',env=e,stdout=log,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(q.returncode)+'\n');text=(work/(step['label']+'.log')).read_text();assert q.returncode==step['expected'],(step['label'],q.returncode)
        if q.returncode:
            assert all(t in text for t in ['running 1 test','1 failed','panicked at',step['test'],step['expected_assertion']]),step['label'];(faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=p['sha256']))
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m;suites[step['label']]=dict(passed=int(m[1]),ignored=int(m[2]))
        print('Executed',step['label'],q.returncode,flush=True)
    pcm={}
    for label,prefix,count in [('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36),('local-pcm','MIGO_LOCAL_FILE_PCM ',12),('file-pcm','MIGO_FILE_STREAM_PCM ',8),('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40),('indexed-large','MIGO_INDEXED_FILE_PCM ',10),('package-small','MIGO_PACKAGE_FILE_PCM ',12),('package-large','MIGO_PACKAGE_INDEXED_PCM ',10)]:
        a=index.pcm(work/(label+'-debug.log'),prefix,count);z=index.pcm(work/(label+'-release.log'),prefix,count);assert a==z,(label,'dev/O3 PCM bits differ')
        if label!='origin-pcm':
            old=index.pcm(baseline/(label+'-debug.log'),prefix,count);assert a==old,(label,'normal production PCM differs from frozen previous checkpoint')
        pcm[label]=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_product_bits_unchanged=label!='origin-pcm')
    large={}
    for label in ['debug','release']:
        m=re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)',(work/('large-file-'+label+'.log')).read_text());assert m and int(m[1])==16777345 and int(m[2])==1048576000 and int(m[3])<4*1024*1024;large[label]=dict(samples=int(m[1]),bits=int(m[2]),retained_pcm_bytes=int(m[3]),every_sample_checked=True,tail_and_end_checked=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'],suites=suites,controls=faults,original_red=reds,clippy=plan['clippy'],pcm=pcm,large_file=large,baseline_pcm_path=str(baseline),source_inputs_unchanged=True,private_sources_restored=True),indent=2)+'\n');print('Current sinc absolute origin and frozen normal PCM verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-clippy',type=Path);p.add_argument('--baseline-pcm',type=Path);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:
        assert a.baseline_pcm;execute(a.output.resolve(),a.baseline_pcm.resolve())
    else:
        assert all([a.target_dir,a.before_source,a.before_clippy]);build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.before_clippy.resolve())
