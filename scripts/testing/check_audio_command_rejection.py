#!/usr/bin/env python3
"""Invalid live commands freed bulk input on audio before its admission ended.

Validate real pre-dispatch ownership/type/source checks, immediate typed errors,
CPU-only bulk disposal and unchanged forwarding; faults must execute fresh tests.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::snapshot_buffer_commands_validate_context_and_node_ownership'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'reject':package/'src/audio_thread_command_rejection.rs','context':package/'src/context.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='rejected command payload escaped CPU retirement'
    invalid='invalid command escaped owned rejection'
    rows=[
      ('rejection-bypassed','reject','if let Some(error) = rejection(delivery.command(), contexts, index, players)','if let Some(error) = None::<EngineError>',invalid),
      ('rejected-data-inline','channel','if permit.is_none() || bytes < 4096 {','if true {',cpu),
      ('rejected-credit-returned-early','channel','                _payload: Box::new(payload),\n                _permit: permit,','                _payload: Box::new(payload),\n                _permit: None,','rejected command returned original credit before CPU drop'),
      ('rejected-byte-credit-returned-early','channel','            let _ = resp.send(Err(error));','            let _ = resp.send(Err(error));\n            let mut permit = permit;\n            if let Some(credit) = permit.as_mut() { credit.shrink_to(0); }','rejected command returned original credit before CPU drop'),
      ('rejected-response-closed','channel','            let _ = resp.send(Err(error));','            drop((resp,error));','rejected command lost original response semantics'),
      ('rejected-data-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('stale-load-forwarded','reject','!players.get(id).is_some_and(|p| p.shared.owns_load(load))','false',invalid),
      ('wrong-node-type-forwarded','reject','ctx.node_is::<WaveShaperNode>(*node_id)','true',invalid),
      ('cross-context-snapshot-forwarded','reject','index.get_context(*node_id) == Some(*ctx_id)','true',invalid),
      ('type-inspection-mutates-controls','context',"    pub(crate) fn node_is<T: AudioNodeProcessor + 'static>(\n        &mut self,\n        node_id: AudioNodeId,\n    ) -> bool {","    pub(crate) fn node_is<T: AudioNodeProcessor + 'static>(\n        &mut self,\n        node_id: AudioNodeId,\n    ) -> bool {\n        self.device_controls_pending = true;",'command target inspection changed playback controls'),
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
    capacity.run(positive,package,env,work,'positive-rejection',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after));binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual rejected-payload fault rejected',name,flush=True)
            finally:paths[key].write_text(originals[key])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Command rejection verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
