#!/usr/bin/env python3
"""Refused InnerAudio installation and unadmitted publication dropped PCM on callers.

Keep original queue/completion tickets through actual CPU disposal; successful
publication and installation preserve their original admission and PCM.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::source_load_tests::out_of_order_owned_results_cannot_replace_the_new_source'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'actor':package/'src/audio_thread.rs','owner':package/'src/decode_result_custody.rs','inner':package/'src/decode_inner_custody.rs','player':package/'src/inner_audio.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    transfer=method(originals['owner'],'    pub(super) fn try_transfer')
    dispose=method(originals['inner'],'fn dispose<')
    response_test='audio_thread::source_load_tests::out_of_order_owned_results_cannot_replace_the_new_source'
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    cpu=['inner refused PCM escaped CPU workers','inner refused result credit returned before CPU disposal']
    publication_cpu=['unadmitted PCM escaped CPU workers','unadmitted completion credit returned before CPU disposal','unadmitted completion returned its local slot early']
    reject=method(originals['inner'],'pub(super) fn reject_unadmitted<')
    release=method(originals['inner'],'impl<F: FnOnce()> Drop for Release<F>')
    install=method(originals['inner'],'pub(super) fn integrate(')
    raw="let DecodeResult::InnerAudio{id,load,result,resp}=entry.into_value() else {unreachable!()};integrate_inner_audio_decode_result(players,id,load,result,resp);"
    rows=[
      ('inner-owner-bypassed','owner','super::inner_custody::integrate(entry, players);',raw,cpu),
      ('inner-refusal-inline','owner',transfer,transfer.replace('Err(error) => Err(error),','Err(error) => {drop(self.value.take());Err(error)},'),cpu),
      ('inner-transfer-credit-reset','owner','match transfer(\n            self.value.as_mut().unwrap(),\n            &mut self.lease.as_mut().unwrap().physical,\n        ) {','self.lease.as_ref().unwrap()._permit.usage.items.store(0,std::sync::atomic::Ordering::Release);self.lease.as_ref().unwrap()._permit.usage.bytes.store(0,std::sync::atomic::Ordering::Release);match transfer(\n            self.value.as_mut().unwrap(),\n            &mut self.lease.as_mut().unwrap().physical,\n        ) {',cpu),
      ('inner-success-ticket-leaked','owner',transfer,transfer.replace('release(self.lease.take().unwrap());','std::mem::forget(self.lease.take().unwrap());'),'inner installation retained transport credit after adoption'),
      ('inner-source-fence-skipped','inner','player.shared.owns_load(&load) && !resp.is_closed()','!resp.is_closed()','inner integration changed refusal error precedence'),
      ('inner-duplicate-check-skipped','player','if !self.shared.owns_load(load) || self.shared.is_loaded() {','if !self.shared.owns_load(load) {','inner installation changed typed refusal'),
      ('inner-aggregate-charge-skipped','player','self.aggregate.try_reserve(bytes, "InnerAudio")','self.aggregate.try_reserve(0, "InnerAudio")','inner installation changed typed refusal'),
      ('inner-original-pcm-copied','player','self.replace_source(\n            AudioSource::Owned(std::mem::take(&mut audio.samples)),\n            Some(permit),\n        );','self.replace_source(AudioSource::Owned(audio.samples.clone()),Some(permit));',['inner refused PCM escaped CPU workers','inner installation copied original PCM']),
      ('inner-response-forgotten','inner',install,install.replace('let _ = resp.send(response);','std::mem::forget(resp);drop(response);'),'inner refusal delayed typed response'),
      ('publication-owner-bypassed','inner',reject,"pub(super) fn reject_unadmitted<F:FnOnce()+Send+'static>(result:DecodeResult,code:ErrorCode,detail:&'static str,release:F,_retired:Option<&Retirement>){reject_decode_result_after(result,code,detail,release)}",publication_cpu),
      ('publication-release-before-disposal','inner','_release: Release(Some(release)),','_release: {release();Release::<F>(None)},','unadmitted completion credit returned before CPU disposal'),
      ('publication-disposal-on-async','inner','cpu.retirement().retire(value).await;','drop(value);',publication_cpu),
      ('publication-closed-disposal-skipped','inner','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}',publication_cpu),
      ('publication-release-forgotten','inner',release,release.replace('release();','std::mem::forget(release);'),'unadmitted CPU disposal leaked completion credit'),
      ('publication-success-old-credit-leaked','actor','    let mut physical = retire();','    std::mem::forget(retire);let mut physical:Option<AudioAggregatePermit>=None;','successful publication retained old in-flight credit'),
      ('publication-success-credit-reset','actor','    let mut physical = retire();','    let mut physical=retire();if matches!(&result,DecodeResult::AudioBuffer{..}){result_tx.usage.items.store(0,std::sync::atomic::Ordering::Release);result_tx.usage.bytes.store(0,std::sync::atomic::Ordering::Release);}','successful publication lost result admission'),
      ('publication-small-boundary-changed','inner','result.queued_bytes() >= 4096','result.queued_bytes() >= 4092','unadmitted small/legacy capacity boundary changed'),
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
    capacity.run(positive,package,env,work,'positive-inner-custody',response_test)
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
                test=response_test
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Inner custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
