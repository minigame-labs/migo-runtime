#!/usr/bin/env python3
"""PCM pool overflow,producer teardown and queued publication dropped Vecs inline.

Keep original admission through actual input disposal; preserve PCM recycling and original publication
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='streaming::tests::a_returned_chunk_is_the_buffer_the_next_one_is_written_into'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'buffer':package/'src/stream_pcm_buffer.rs','stream':package/'src/streaming.rs','driver':package/'src/streaming_transport.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    ctor=method(originals['buffer'],'    pub(super) fn new(')
    dispose=method(originals['buffer'],'impl Drop for Buffer')
    publish=method(originals['stream'],'    pub(crate) fn publish_next(')
    acquire=method(originals['stream'],'    pub(crate) async fn acquire(')
    rows=[
      ('pcm-production-pool-unmanaged','driver','let mut pool = PcmPool::with_cpu(cpu);','let mut pool = PcmPool::new();','production PCM loan bypassed CPU disposal'),
      ('pcm-buffer-disposal-unregistered','buffer',ctor,ctor.replace('let retired = cpu.map','let retired=None;let _unused = cpu.map'),'managed PCM buffer freed before CPU disposal'),
      ('pcm-buffer-inline-disposal','buffer',dispose,'impl Drop for Buffer {fn drop(&mut self){drop(self.data.take());}}','managed PCM buffer freed before CPU disposal'),
      ('pcm-buffer-on-async','buffer','cpu.retirement().retire(value).await;','drop(value);','managed PCM buffer freed before CPU disposal'),
      ('pcm-closed-host-cleanup-skipped','buffer','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}','managed PCM buffer freed before CPU disposal'),
      ('pcm-healthy-return-discarded','stream','let _ = self.home.try_send(buffer);','drop(buffer);','managed PCM recycling changed pointer, capacity or empty loan'),
      ('pcm-return-not-cleared','stream','        buffer.clear();','        // mutation: buffer not cleared','managed PCM recycling changed pointer, capacity or empty loan'),
      ('pcm-cache-not-reused','stream',acquire,'pub(crate) async fn acquire(&mut self)->PcmChunk {let slot=self.slots.as_ref().map(|s|s.clone().try_acquire_owned().unwrap());self.allocate(slot)}','managed PCM recycling changed pointer, capacity or empty loan'),
      ('pcm-physical-slot-early','buffer','        if retired.send(self.data.take().unwrap()).is_err() {','        drop(self.data.as_mut().unwrap()._slot.take());\n        if retired.send(self.data.take().unwrap()).is_err() {','PCM physical loan credit returned before actual CPU disposal'),
      ('pcm-publication-revision-lost','stream',publish,publish.replace('self.revision','StreamRevision::INITIAL'),'PCM publication changed Ready format or revision'),
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
    capacity.run(positive,package,env,work,'positive-pcm',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual PCM retirement fault rejected',name,flush=True)
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('PCM retirement verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
