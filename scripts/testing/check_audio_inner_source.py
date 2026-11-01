#!/usr/bin/env python3
"""Replaced and destroyed InnerAudio owned sources freed PCM on audio callers.

Keep original source storage and aggregate credit together through CPU disposal;
retain byte-identical playback and zero-copy managed cache transfer.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='inner_audio::tests::attaching_cached_backing_preserves_stream_playback_state'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'actor':package/'src/audio_thread.rs','player':package/'src/inner_audio.rs','owner':package/'src/inner_source_retirement.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    response_test='inner_audio::tests::attaching_cached_backing_preserves_stream_playback_state'
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    replace=method(originals['player'],'    fn replace_source(')
    destructor=method(originals['player'],'impl Drop for InnerAudioPlayer')
    extract=method(originals['player'],'    pub(crate) fn take_streamed_audio_with_source(')
    dispose=method(originals['owner'],'pub(super) fn dispose(')
    factory=method(originals['actor'],'pub(crate) fn new_inner_audio_player(')
    cpu=['owned source destructor escaped CPU workers','owned source credit returned before CPU disposal','owned source reopened physical admission before disposal']
    rows=[
      ('source-factory-unwired','actor',factory,factory.replace('InnerAudioPlayer::new_with_cpu(id, channels, cpu.clone())','InnerAudioPlayer::new(id, channels)'),cpu),
      ('source-slot-unarmed','player',replace,replace.replace('Some(source_retirement::Slot::new(cpu.clone()))','None'),cpu),
      ('source-replacement-disposal-bypassed','player','source_retirement::dispose(samples, permit, retired)','drop((samples,permit,retired))',cpu),
      ('source-inline-disposal','owner',dispose,'pub(super) fn dispose(samples:Vec<f32>,permit:Option<AudioAggregatePermit>,slot:Option<Slot>){drop(samples);drop(permit);drop(slot);}',cpu),
      ('source-async-disposal','owner','cpu.retirement().retire(value).await;','drop(value);',cpu),
      ('source-closed-disposal-skipped','owner','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}',cpu),
      ('source-permit-returned-before-cpu','owner','_permit: permit,','_permit: {drop(permit);None},','owned source credit returned before CPU disposal'),
      ('source-permit-forgotten','owner','_permit: permit,','_permit: {std::mem::forget(permit);None},','owned source CPU disposal leaked physical credit'),
      ('source-player-drop-unwired','player',destructor,destructor.replace('self.replace_source(AudioSource::Owned(Vec::new()), None);',''),cpu),
      ('source-empty-capacity-inline','player','source_retirement::dispose(samples, Some(permit), retired);','drop((samples,permit,retired));',cpu),
      ('source-cache-transfer-cloned','player',extract,extract.replace('                samples,','                samples:samples.clone(),'),'owned source cache handoff copied PCM'),
      ('source-cache-transfer-suppressed','player',extract,extract.replace('if samples.is_empty() {','if true {'),'owned source cache transfer lost completed backing'),
      ('source-cache-attach-cursor-reset','player','self.replace_source(AudioSource::Cached(audio.clone()), None);','self.replace_source(AudioSource::Cached(audio.clone()), None);self.position=0;','owned source cached attachment changed playback cursor'),
      ('source-byte-unit-boundary-broken','owner','samples.capacity() >= 4096 / size_of::<f32>()','samples.capacity() >= 4096','owned source small or legacy boundary changed'),
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
    capacity.run(positive,package,env,work,'positive-inner-source',response_test)
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Inner source retirement verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
