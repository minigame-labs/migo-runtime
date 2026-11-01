#!/usr/bin/env python3
"""Cache eviction and final player Arc release could destroy large PCM on audio.

Keep original physical credit through actual backing disposal; preserve cache LRU, original PCM
and source timelines; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='cache::tests::eviction_keeps_aggregate_permit_until_last_cached_arc_drops'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'value':package/'src/cache_retirement.rs','cache':package/'src/cache.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    ctor=method(originals['value'],'    pub(super) fn new(')
    dispose=method(originals['value'],'impl Drop for Value')
    cached=method(originals['cache'],'    fn new(')
    rows=[
      ('cache-global-factory-unmanaged','cache','inner: Mutex::new(AudioCache::with_cpu(cpu.clone())),','inner: Mutex::new(AudioCache::new()),','cached PCM destructor escaped CPU workers'),
      ('cache-owner-cpu-unwired','cache','audio: retirement::Value::new(audio, permit, cpu),','audio: retirement::Value::new(audio, permit, None),','cached PCM destructor escaped CPU workers'),
      ('cache-disposal-unregistered','value',ctor,ctor.replace('let retired = cpu','let retired=None;let _unused = cpu'),'cached PCM destructor escaped CPU workers'),
      ('cache-inline-disposal','value',dispose,'impl Drop for Value {fn drop(&mut self){drop(self.data.take());}}','cached PCM destructor escaped CPU workers'),
      ('cache-disposal-on-async','value','cpu.retirement().retire(value).await;','drop(value);','cached PCM destructor escaped CPU workers'),
      ('cache-closed-cleanup-skipped','value','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}','cached PCM destructor escaped CPU workers'),
      ('cache-credit-early','value','        if retired.send(self.data.take().unwrap()).is_err() {','        drop(self.data.as_mut().unwrap()._permit.take());\n        if retired.send(self.data.take().unwrap()).is_err() {','cached PCM physical credit returned before actual CPU disposal'),
      ('cache-pcm-copied','value',ctor,ctor.replace('audio: DecodedAudio','mut audio: DecodedAudio',1).replace('let retired = cpu','audio.samples=audio.samples.clone();let retired = cpu',1),'cache adoption copied or changed original PCM allocation'),
      ('cache-logical-length-admission','value','audio.samples.capacity() >= 4096','audio.samples.len() >= 4096','cached PCM destructor escaped CPU workers'),
      ('cache-timeline-lost','cache',cached,cached.replace('            source_timeline,','            source_timeline:None,'),'cached PCM lost source timeline'),
      ('cache-zero-capacity-async','value','audio.samples.capacity() >= 4096 / size_of::<f32>()','(audio.samples.capacity() == 0 || audio.samples.capacity() >= 4096 / size_of::<f32>())','small or standalone cached PCM lost synchronous disposal'),
      ('cache-original-permit-lost','value','_permit: Some(permit),','_permit: None,','cache adopted PCM without original physical credit'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-cache',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual cache retirement fault rejected',name,flush=True)
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Cache retirement verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
