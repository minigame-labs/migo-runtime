#!/usr/bin/env python3
"""Frequency output arrays lacked physical admission across pending responses.

Keep physical credit through serialization and actual CPU destruction; preserve
CPU disposal; faults must execute fresh real business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::the_startup_backlog_runs_before_anything_still_in_the_channel'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'frequency':package/'src/audio_thread_command_frequency.rs','retirement':package/'src/node_retirement.rs','response':package/'local-deps/shared/src/audio_frequency_response.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    admission='    let admission = ctx\n        .response_retirement_host()\n        .map(|host| Admission::new(host, bytes))\n        .transpose()?;\n'
    compute='    let data = compute(ctx, node_id, frequencies).expect("checked filter node disappeared");\n'
    rows=[
      ('output-physical-admission-bypassed','frequency',admission,'    let admission:Option<Admission>=None;\n','response storage was not fully admitted before allocation','audio'),
      ('output-large-capacity-not-charged','frequency','reserve_value_with_bytes::<Data>(bytes)','reserve_value_with_bytes::<Data>(0)','response storage was not fully admitted before allocation','audio'),
      ('output-small-capacity-not-charged','frequency','bytes + std::mem::size_of::<Small>() + HANDOFF_BYTES','std::mem::size_of::<Small>() + HANDOFF_BYTES','response storage was not fully admitted before allocation','audio'),
      ('output-small-metadata-not-charged','frequency','bytes + std::mem::size_of::<Small>() + HANDOFF_BYTES','bytes + std::mem::size_of::<Small>()','response storage was not fully admitted before allocation','audio'),
      ('output-admission-after-allocation','frequency',admission+compute,compute+admission,'refused output allocated arrays or updated filter coefficients','audio'),
      ('output-large-inline','frequency','if bytes >= 4096 {','if false {','large response arrays freed on caller','audio'),
      ('output-async-destruction','retirement','cpu.retirement().retire(value).await;','drop(value);','large response escaped CPU retirement','audio'),
      ('output-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);','output credit returned before CPU drop','audio'),
      ('output-small-offloaded','frequency','if bytes >= 4096 {','if true {','small output cleanup left caller','audio'),
      ('output-point-limit-bypassed','frequency','if frequencies.len() > limit {','if false {','native response bypassed existing public point limit','audio'),
      ('output-closed-host-admitted','retirement','        self.ensure_open()?;','        // injected: skip closed-host admission','closed host admitted new response storage','audio'),
      ('output-arrays-swapped','response','        self.0.values()','        let (a,b)=self.0.values(); (b,a)','owned output changed complete frequency results','audio'),
      ('output-serializer-loses-tuple','response','self.values().serialize(serializer)','self.values().0.serialize(serializer)','response JSON shape changed','shared'),
      ('output-owner-detached-by-clone','response','Self(Box::new(storage))','{ let (a,b)=storage.values(); Self(Box::new((a.to_vec(),b.to_vec()))) }','response storage was not fully admitted before allocation','audio'),
    ]
    for n,k,a,z,_,_ in rows:assert a!=z and originals[k].count(a)==(3 if n=='output-closed-host-admitted' else 1),(n,'anchor drift',originals[k].count(a))
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-output',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion,kind in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=base.build_shared(package,work,env,name) if kind=='shared' else product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,base.SHARED if kind=='shared' else TEST,101,assertion);item['binary_sha256']=record(name,binary,True,kind);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual response-output fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
        for mode,release in [('debug',False),('release',True)]:
            if release:
                binary=product.src.build(package,work,env,'positive-release',True);record('positive-release',binary);suites['audio-release']=capacity.run(binary,package,env,work,'full-release')
            shared=base.build_shared(package,work,env,'shared-'+mode,release);suites['shared-'+mode]=base.run_shared(shared,package,env,work,'full-shared-'+mode);record('shared-'+mode,shared,True,'shared')
            source,raw=base.core_facet(package,work)
            try:
                binary=product.src.build(package,work,env,'core-facet-'+mode,release);log=work/('core-facet-'+mode+'.log');code=product.whole.support.clock.common.run([str(binary),'core_service_audio_probe::tests::','--nocapture'],log,package,env,timeout=120);assert code==0 and '6 passed; 0 failed;' in log.read_text(),log;(work/('core-facet-'+mode+'-exit.txt')).write_text(str(code)+'\n');record('core-facet-'+mode,binary,True)
            finally:source.write_text(raw)
    product.whole.verify_inputs(work,frozen)
    for k,p in paths.items():assert p.read_text()==originals[k]
    for dep in ['shared','io','services','frame-wire']:
        assert all((package/'local-deps'/dep/p.relative_to(ROOT/'engine/crates'/dep)).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates'/dep/'src').rglob('*') if p.is_file())
    assert all((package/'src'/p.relative_to(ROOT/'engine/crates/audio/src')).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates/audio/src').rglob('*') if p.is_file())
    result=dict(programs=programs,controls=controls,suites=suites,preflight=preflight,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,core_facet='Exact host-audio service/test bodies; cfg selectors and crate alias only; embedded-v8 branches excluded')
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Frequency output ownership verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
