#!/usr/bin/env python3
"""Borrowed parameter Strings still died on audio after their original effects.

Preserve ordered setter/automation effects and original queue credit through
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
    paths={'parameters':package/'src/audio_thread_command_parameters.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='parameter input escaped CPU retirement'
    credit='parameter credit returned before CPU drop'
    effects='parameter commands changed PCM, automation or analyser'
    cap='delivery.command().queued_payload_bytes() < 4096'
    rows=[
      ('parameter-retirement-bypassed','parameters','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk parameter bypassed input retirement'),
      ('parameter-input-inline','inbox','    pub(crate) fn finish_parameters(&mut self, delivery: AudioCommandDelivery) {\n        self.retirement.discard(delivery);\n    }','    pub(crate) fn finish_parameters(&mut self, delivery: AudioCommandDelivery) {\n        drop(delivery);\n    }',cpu),
      ('parameter-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('parameter-credit-returned-early','channel','            _payload: payload,\n            _permit: permit,','            _payload: payload,\n            _permit: None,',credit),
      ('parameter-byte-credit-returned-early','channel','        let payload: Box<dyn Send> = match command {','        let mut permit=permit;\n        if let Some(credit)=permit.as_mut(){credit.shrink_to(0); }\n        let payload: Box<dyn Send> = match command {',credit),
      ('parameter-effects-skipped','parameters','    match command {','    if true {return true;}\n    match command {',effects),
      ('parameter-unrelated-command-consumed','parameters','        _ => return false,','        _ => {},','parameter dispatcher consumed unrelated command'),
      ('parameter-cancel-time-lost','parameters','ctx.param_cancel_scheduled(*node_id, param_name, *cancel_time)','ctx.param_cancel_scheduled(*node_id, param_name, 0.0)',effects),
      ('parameter-ramp-time-lost','parameters','ctx.param_linear_ramp(*node_id, param_name, *value, *end_time)','ctx.param_linear_ramp(*node_id, param_name, *value, 0.0)',effects),
      ('parameter-oscillator-type-lost','parameters','OscillatorType::from_str(osc_type)','OscillatorType::Sine',effects),
      ('parameter-distance-model-lost','parameters','DistanceModel::from_str(model)','DistanceModel::Inverse',effects),
      ('parameter-oversample-lost','parameters','OversampleType::from_str(oversample)','OversampleType::None',effects),
      ('parameter-analyser-smoothing-lost','parameters','an.set_smoothing_time_constant(*value)','an.set_smoothing_time_constant(0.8)',effects),
      ('parameter-immediate-value-lost','parameters','ctx.set_node_param(*node_id, param_name, *value)','ctx.set_node_param(*node_id, param_name, 0.0)',effects),
      ('parameter-small-input-offloaded','parameters',cap,cap.replace('< 4096','< 0'),'small parameter unnecessarily handed to CPU'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift')
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-parameters',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                if name=='parameter-small-input-offloaded':
                    assert originals['channel'].count('if self.permit.is_none() || bytes < 4096 {')==1
                    paths['channel'].write_text(originals['channel'].replace('if self.permit.is_none() || bytes < 4096 {','if self.permit.is_none() || bytes < 0 {'))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual parameter-input fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
                if name=='parameter-small-input-offloaded':paths['channel'].write_text(originals['channel'])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Command parameters verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
