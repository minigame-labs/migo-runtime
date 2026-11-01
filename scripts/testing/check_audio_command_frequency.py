#!/usr/bin/env python3
"""Borrowed frequency Vecs still died on audio after their original filter results.

Preserve complete magnitude/phase results and original queue credit through
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
    paths={'frequency':package/'src/audio_thread_command_frequency.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='frequency input escaped CPU retirement'
    response='frequency response changed or waited for CPU retirement'
    cap='frequencies\n            .capacity()\n            .saturating_mul(std::mem::size_of::<f32>())\n            < 4096'
    shared_cap='frequencies\n                .capacity()\n                .saturating_mul(std::mem::size_of::<f32>())\n                < 4096'
    call='    let result = query(contexts, index, *node_id, frequencies);'
    rows=[
      ('frequency-retirement-bypassed','frequency','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk frequency input bypassed CPU retirement'),
      ('frequency-input-length-used','frequency',cap,cap.replace('capacity()','len()'),'bulk frequency input bypassed CPU retirement'),
      ('frequency-input-inline','channel','if permit.is_none()\n            || '+shared_cap,'if true',cpu),
      ('frequency-credit-returned-early','channel','            _payload: Box::new(frequencies),\n            _permit: permit,','            _payload: Box::new(frequencies),\n            _permit: None,','frequency credit returned before CPU drop'),
      ('frequency-byte-credit-returned-early','channel','        let AudioCmd::GetFrequencyResponse {\n            frequencies, resp, ..\n        } = command','        let mut permit=permit;\n        if let Some(credit)=permit.as_mut(){credit.shrink_to(0); }\n        let AudioCmd::GetFrequencyResponse {\n            frequencies, resp, ..\n        } = command','frequency credit returned before CPU drop'),
      ('frequency-response-closed','channel','        let _ = resp.send(result);\n        if permit.is_none()\n            || '+shared_cap,'        drop((resp,result));\n        if permit.is_none()\n            || '+shared_cap,response),
      ('frequency-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('frequency-math-skipped','frequency',call,'    let result = Ok(FrequencyResponse::new((Vec::new(),Vec::new())));',response),
      ('frequency-sample-rate-lost','frequency','let sr = ctx.sample_rate() as f64;','let sr = 48000.0;',response),
      ('frequency-iir-lookup-lost','frequency','ctx.with_node_typed::<IIRFilterNode, _>(node_id, |n| {','ctx.with_node_typed::<BiquadFilterNode, _>(node_id, |n| {',response),
      ('frequency-input-truncated','frequency','n.get_frequency_response(sr, frequencies)','n.get_frequency_response(sr, &frequencies[..frequencies.len().min(1)])',response),
      ('frequency-cancel-skips-effect','frequency',call,'    let result = if matches!(delivery.command(), AudioCmd::GetFrequencyResponse{resp,..} if resp.is_closed()) { Ok(FrequencyResponse::new((Vec::new(),Vec::new()))) } else { query(contexts,index,*node_id,frequencies) };','cancelled frequency query skipped original node effect'),
      ('frequency-error-code-lost','frequency','ErrorCode::NotFound,','ErrorCode::InvalidArgument,','frequency error changed'),
      ('frequency-small-input-offloaded','frequency',cap,cap.replace('< 4096','< 0'),'small frequency input unnecessarily handed to CPU'),
      ('frequency-boundary-skipped','frequency',cap,cap.replace('< 4096','<= 4096'),'4096-byte frequency input lost retirement boundary'),
      ('frequency-unrelated-consumed','frequency','        return Some(delivery);\n    };','        return None;\n    };','frequency dispatcher consumed unrelated command'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==(2 if n=='frequency-input-truncated' else 1),(n,'anchor drift')
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-frequency',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                if name=='frequency-small-input-offloaded':
                    assert originals['channel'].count(shared_cap)==1
                    paths['channel'].write_text(originals['channel'].replace(shared_cap,shared_cap.replace('< 4096','< 0')))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual frequency-input fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
                if name=='frequency-small-input-offloaded':paths['channel'].write_text(originals['channel'])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Command frequency inputs verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
