#!/usr/bin/env python3
"""Keep response custody through Services adoption, refusal and final snapshot Arc.

Execute fresh business assertions for the prior accepted queue contract and the
queue-to-process credit transfer. Shared uses isolated existing resource limits;
Actor drives the actual Services and Shared path with the real CPU executor.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::decode_receiver_exit_releases_queued_permits'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'actor':package/'src/audio_thread.rs','owner':package/'src/decode_result_custody.rs','response':package/'src/decode_response_custody.rs','pcm':package/'local-deps/shared/src/audio_decoded_pcm.rs','registry':package/'local-deps/shared/src/audio_resources.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    drop=method(originals['owner'],"impl<T: Send + 'static> Drop for Owned<T>")
    send=method(originals['response'],'    fn dispose(')
    prepare=method(originals['response'],'pub(super) fn prepare(')
    buffer=method(originals['owner'],'fn integrate_buffer(')
    receiver=method(originals['actor'],'impl<T: DecodeQueuePayload> Drop for DecodeQueueReceiver<T>')
    payload=method(originals['actor'],'impl DecodeQueuePayload for DecodeResult')
    header=method(payload,'    fn retire_parts(')
    rows=[
      ('result-factory-unmanaged','owner','send.retirement = Some(Retirement {','send.retirement=None;let _unused = Some(Retirement {','accepted result destructor escaped CPU workers'),
      ('result-entry-owner-unwired','owner','retired: retired.cloned(),','retired: None,','accepted result destructor escaped CPU workers'),
      ('result-inline-disposal','owner',drop,"impl<T:Send+'static> Drop for Owned<T> {fn drop(&mut self){drop(self.value.take());drop(self.lease.take());}}",'accepted result destructor escaped CPU workers'),
      ('result-disposal-on-async','owner','worker.retirement().retire(value).await;','drop(value);','accepted result destructor escaped CPU workers'),
      ('result-closed-disposal-skipped','owner','worker.retirement().retire(value).await;','if !worker.is_closed(){worker.retirement().retire(value).await;}','accepted result destructor escaped CPU workers'),
      ('result-receiver-credit-reset','actor',receiver,receiver.replace('// Actual queued/retiring owners retain their original physical ticket.','self.usage.items.store(0,std::sync::atomic::Ordering::Release);self.usage.bytes.store(0,std::sync::atomic::Ordering::Release);'),'accepted result credit returned before actual CPU disposal'),
      ('result-dequeue-credit-early','owner','let (head, value) = f(self.value.take().unwrap());','let (head, value) = f(self.value.take().unwrap());self.lease.as_ref().unwrap()._permit.usage.items.store(0,std::sync::atomic::Ordering::Release);self.lease.as_ref().unwrap()._permit.usage.bytes.store(0,std::sync::atomic::Ordering::Release);','accepted result credit returned before actual CPU disposal'),
      ('result-closed-response-inline','owner',buffer,buffer.replace('if resp.is_closed() {','if resp.is_closed() {drop(body.into_value());return;} if false {',1),'accepted result destructor escaped CPU workers'),
      ('result-invalid-context-inline','owner',buffer,buffer.replace('Err(error) => {\n                drop(body);','Err(error) => {\n                drop(body.into_value());',1),'accepted result destructor escaped CPU workers'),
      ('result-failed-send-inline','response',send,'fn dispose(mut self:Box<Self>,samples:Vec<f32>,credit:Option<DecodedPcmCredit>){drop((samples,credit,self.lease.take()));}','accepted result destructor escaped CPU workers'),
      ('result-response-close-deferred','actor',header,'fn retire_parts(self,lease:result_custody::Lease)->Box<dyn Send>{result_custody::boxed(self,lease)}','result receiver exit delayed response close or reopened admission'),
      ('result-success-credit-reset','owner',buffer,buffer.replace('let pcm = super::response_custody::prepare(result.unwrap(), frames, lease);','let usage=lease._permit.usage.clone();let pcm = super::response_custody::prepare(result.unwrap(), frames, lease);').replace('let _ = resp.send(Ok(pcm));','if resp.send(Ok(pcm)).is_ok(){usage.items.store(0,std::sync::atomic::Ordering::Release);usage.bytes.store(0,std::sync::atomic::Ordering::Release);}'),'successful response released queue admission before registry handoff'),
    ]
    response_test='audio_thread::tests::a_live_decode_answers_its_samples_and_leaves_the_context_untouched'
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    adopted=method(originals['response'],'    fn adopted(')
    rows += [
      ('response-owner-bypassed','response',prepare,'pub(super) fn prepare(audio:DecodedAudio,frames:u32,lease:Lease)->DecodedPcm {release(lease);DecodedPcm::new(audio.sample_rate,audio.channels,frames,audio.samples)}','response lost queue credit before registry admission'),
      ('response-inline-final-disposal','response',send,'fn dispose(mut self:Box<Self>,samples:Vec<f32>,credit:Option<DecodedPcmCredit>){drop((samples,credit,self.lease.take()));}',['response final payload destructor escaped CPU workers','unpolled response returned credit before CPU disposal']),
      ('response-async-final-disposal','response','cpu.retirement().retire(value).await;','drop(value);',['response final payload destructor escaped CPU workers','unpolled response returned credit before CPU disposal']),
      ('response-closed-final-disposal-skipped','response','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}',['response final payload destructor escaped CPU workers','unpolled response returned credit before CPU disposal']),
      ('response-transport-credit-leaked','response',adopted,'fn adopted(&mut self) {}','registry adoption retained transport credit'),
      ('response-original-pcm-copied','response','        audio.samples,\n        Box::new(owner),','        audio.samples.clone(),\n        Box::new(owner),','response registry copied original PCM'),
      ('shared-persistent-credit-discarded','pcm','        self.credit = Some(credit);','        drop(credit);','decoded registry double charged physical storage'),
      ('shared-custody-adoption-skipped','pcm','            custody.adopted();','            let _ = custody;','decoded registry never exchanged transport credit'),
      ('shared-custody-dropped-at-construction','pcm','        pcm.custody = Some(custody);','        drop(custody);','decoded registry never exchanged transport credit'),
      ('shared-final-credit-released-early','pcm','custody.dispose(std::mem::take(&mut self.samples), self.credit.take());','drop(self.credit.take());custody.dispose(std::mem::take(&mut self.samples),None);','decoded registry credit returned before sample disposal'),
      ('shared-final-disposal-skipped','pcm','custody.dispose(std::mem::take(&mut self.samples), self.credit.take());','std::mem::forget(custody);std::mem::forget(std::mem::take(&mut self.samples));std::mem::forget(self.credit.take());','decoded final owner lost disposal custody'),
      ('shared-registry-original-pcm-copied','registry','        pcm.adopt_credit(Box::new(process_permit));','        pcm.samples=pcm.samples.clone();pcm.adopt_credit(Box::new(process_permit));','decoded registry copied original PCM'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    def run_fault(binary,package,env,work,name,test,assertion):
        allowed=assertion if isinstance(assertion,list) else [assertion]
        log=work/(name+'.log')
        code=capacity.product.whole.support.clock.common.run([str(binary),'--exact',test,'--nocapture'],log,package,env,timeout=120)
        (work/(name+'-exit.txt')).write_text(str(code)+'\n')
        output=log.read_text()
        assert code==101 and all(marker in output for marker in ['running 1 test','panicked at','1 failed']),(name,code,log)
        matched=[marker for marker in allowed if marker in output]
        assert matched,(name,allowed,log)
        return dict(label=name,test=test,exit=code,expected_assertion=matched[0],allowed_assertions=allowed)
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-result',TEST)
    capacity.run(positive,package,env,work,'positive-response',response_test)
    shared=base.build_shared(package,work,env,'shared-debug',False)
    capacity.run(shared,package,env,work,'positive-shared-response',shared_test)
    if not preflight:suites['shared-debug']=base.run_shared(shared,package,env,work,'full-shared-debug')
    record('shared-debug',shared,True,'shared')
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                kind='shared' if name.startswith('shared-') else 'audio'
                binary=base.build_shared(package,work,env,name,False) if kind=='shared' else product.src.build(package,work,env,name,False)
                test=shared_test if kind=='shared' else response_test if name.startswith('response-') else TEST
                item=run_fault(binary,package,env,work,name,test,assertion);item['binary_sha256']=record(name,binary,True,kind);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual response ownership fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
        for mode,release in [('debug',False),('release',True)]:
            if release:
                binary=product.src.build(package,work,env,'positive-release',True);record('positive-release',binary);suites['audio-release']=capacity.run(binary,package,env,work,'full-release')
            if release:
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Response ownership verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
