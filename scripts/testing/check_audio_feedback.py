#!/usr/bin/env python3
"""Old feedback rendering overwrote shared node buffers in random HashMap order.

Execute native WPT equivalents, independent complete sample recurrences, cold
allocation and ownership regressions. Original complete graph/Delay sources and
real source faults must reach the designated runtime assertions, never build reds.
"""
from __future__ import annotations
import argparse,json,re,shutil
from pathlib import Path
import check_audio_command_ownership as base
import check_audio_oboe_callback as prior
wave=prior.wave
ROOT=base.ROOT
PREFIX='context::feedback_tests::'
PCM=PREFIX+'complete_pcm_matches_sample_recurrence_across_orders_blocks_and_automation'
WPT=PREFIX+'wpt_processing_model_cycle_and_delay_cases'
GRAPH='audio_thread::render_buffer_tests::complete_graph_pcm_and_collection_receipt'
OLD=['context.rs','context_construction.rs','context_graph_storage.rs','nodes/delay.rs']

def feedback_pcm(log):
    result={};decoder=json.JSONDecoder()
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_FEEDBACK_PCM '):continue
        _,key,value=line.split(' ',2);pcm,end=decoder.raw_decode(value);clock=json.loads(value[end:].strip());assert key not in result;result[key]=dict(pcm=pcm,clock=clock)
    assert len(result)==192,(log,len(result));return result

def graph_pcm(log):
    result={};decoder=json.JSONDecoder()
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_RENDER_PCM '):continue
        _,key,value=line.split(' ',2);parts=[]
        for _ in range(3):part,end=decoder.raw_decode(value);parts.append(part);value=value[end:].lstrip()
        assert key not in result;result[key]=dict(pcm=parts[0],collected=parts[1],ended=parts[2],clock=json.loads(value))
    assert len(result)==216,(log,len(result));return result

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'source anchor')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    cycle='cyclic_zero_delay_starts_with_exactly_one_silent_quantum';cycle_message='cyclic Delay did not enforce one quantum before output'
    silent='delay_free_cycle_is_muted_and_its_downstream_keeps_external_audio';silent_message='delay-free cycle leaked audio or muted its downstream'
    add('muted-old-buffer-retained','processing','            buffer.as_mut_slice()[..buffer_size].fill(0.0);','            // Fault: stale muted output remains.',silent,silent_message)
    add('all-kahn-leftovers-muted','processing','        self.classify_cycles(true);','        self.classify_cycles(true);\n        for state in self.in_degree.iter_mut(){state.muted |= state.degree>0;}',silent,silent_message)
    add('delay-cycle-breakers-omitted','processing','self.in_degree[member].feedback_delay = cyclic','self.in_degree[member].feedback_delay = false && cyclic',cycle,cycle_message)
    add('delay-self-cycle-missed','processing','let cyclic = self.processing_order.len() - start > 1 || self_loop;','let cyclic = self.processing_order.len() - start > 1 || (remove_delays && self_loop);','a_delay_self_loop_is_a_cycle_breaker','Delay self-loop lost the minimum quantum')
    add('all-delays-split','processing','self.in_degree[member].feedback_delay = cyclic','self.in_degree[member].feedback_delay = true','an_acyclic_delay_downstream_of_a_muted_cycle_keeps_zero_delay','acyclic downstream delay was incorrectly split or muted')
    add('minimum-delay-not-clamped','delay','.max(minimum)','.max(0.0)',cycle,cycle_message)
    add('feedback-ring-headroom-omitted','delay','let minimum_frames = crate::audio_thread::RENDER_QUANTUM_FRAMES + 2;','let minimum_frames = crate::audio_thread::RENDER_QUANTUM_FRAMES;',cycle,cycle_message)
    add('feedback-reader-one-frame-early','delay','let cursor = (self.write_frame + frame) % self.frames;','let cursor = (self.write_frame + frame + 1) % self.frames;',cycle,cycle_message)
    method=wave.method(originals['delay'],'pub(crate) fn write_feedback(')
    damaged=re.sub(r'=\s*input\s*\.get\(frame \* self.channels \+ channel\)\s*\.copied\(\)\s*\.unwrap_or\(0\.0\);','= 0.0;',method)
    assert damaged!=method
    add('feedback-writer-discards-input','delay',method,damaged,cycle,cycle_message)
    anchor='''        if let Some(destination) = destination.filter(|&n| !self.in_degree[n as usize].muted) {
            self.processing_order.push(destination);
        }'''
    add('dependency-order-reversed','processing',anchor,anchor+'\n        self.processing_order.reverse();','wpt_processing_model_cycle_and_delay_cases','official WPT processing-model sample mismatch')
    add('feedback-tail-old-epsilon','delay','if self.feedback {\n            f32::MIN_POSITIVE','if self.feedback {\n            1e-10','released_feedback_tail_is_not_truncated_at_the_old_epsilon','released cyclic Delay tail was truncated')
    add('scc-late-allocation','processing','        self.classify_cycles(false);','        let _late=std::hint::black_box(Box::new([0u8;32]));\n        self.classify_cycles(false);','cold_cycle_rebuild_and_full_budget_closed_cpu_render_do_not_allocate','cold SCC/Delay graph allocated on render')
    assert len(rows)==12
    return rows

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    paths={name:package/'src'/path for name,path in dict(context='context.rs',processing='context_processing.rs',delay='nodes/delay.rs').items()};originals={k:p.read_text()for k,p in paths.items()};rows=declarations(originals)
    path=Path(__file__).resolve();name=str(path.relative_to(ROOT));frozen[name]=common.sha(path);dest=work/'source-inputs'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest);(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    old_context=package/'src/context.rs';output=package/'src/output.rs';output_saved=output.read_text();changed=[name for name,h in json.loads((before_directory/'root-sha256.json').read_text()).items()if name.startswith('engine/crates/audio/')and name.endswith('.rs')and common.sha(ROOT/name)!=h];saved={name:(package/name.removeprefix('engine/crates/audio/')).read_bytes()for name in changed}
    clippy=['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json']
    try:
        output.write_text(prior.ports(output_saved));assert common.run(clippy,work/'clippy.jsonl',package,env,timeout=600)==0
        for name in changed:(package/name.removeprefix('engine/crates/audio/')).write_bytes((before_directory/name).read_bytes())
        output.write_text(prior.ports((before_directory/'engine/crates/audio/src/output.rs').read_text()));assert common.run(clippy,work/'clippy-before.jsonl',package,env,timeout=600)==0
    finally:
        for name,data in saved.items():(package/name.removeprefix('engine/crates/audio/')).write_bytes(data)
        output.write_text(output_saved)
    old,new=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);delta=dict(before=sum(old.values()),after=sum(new.values()),new_diagnostics=sum((new-old).values()),new=[dict(key=k,count=v)for k,v in (new-old).items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not(new-old),delta
    print('Stable source Clippy compared',delta,flush=True)
    programs=[];suites={};controls=[];reds=[]
    def build(label,release=False):
        binary=product.src.build(package,work,env,label,release);programs.append(dict(name=label,path=str(binary),sha256=common.sha(binary),pruned=False));return binary
    def run(binary,label,test=None,expected=0,message=None,exact=True):
        command=[str(binary)]+((['--exact']if exact else[])+[test,'--nocapture']if test else[]);code=common.run(command,work/(label+'.log'),package,env,timeout=900);(work/(label+'-exit.txt')).write_text(str(code)+'\n');text=(work/(label+'.log')).read_text();assert code==expected,(label,code)
        if expected:assert all(x in text for x in ['running 1 test','panicked at','1 failed',test,message]),label
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m,label;suites[label]=dict(passed=int(m[1]),ignored=int(m[2]))
    positive=build('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    run(positive,'feedback-debug',PREFIX,exact=False)
    if preflight:
        (work/'preflight-summary.json').write_text(json.dumps(dict(clippy=delta,suites=suites,programs=programs),indent=2)+'\n');print('Feedback stable-source preflight passed',flush=True);return
    run(positive,'full-debug');run(positive,'feedback-pcm-debug',PCM);run(positive,'graph-pcm-debug',GRAPH);run(positive,'wpt-debug',WPT)
    current=feedback_pcm(work/'feedback-pcm-debug.log');graph=graph_pcm(work/'graph-pcm-debug.log')
    old_sources={name:(package/'src'/name).read_bytes()for name in OLD}
    try:
        for name in OLD:(package/'src'/name).write_bytes((before_directory/'engine/crates/audio/src'/name).read_bytes())
        original=old_context.read_text();old_context.write_text(original+'\n#[cfg(test)]\n#[path="context_feedback_tests.rs"]\nmod feedback_tests;\n');(work/'original-context-adapted.rs').write_text(old_context.read_text())
        binary=build('original-graph')
        for test,message in [('delay_free_cycle_is_muted_and_its_downstream_keeps_external_audio','delay-free cycle leaked audio or muted its downstream'),('cyclic_zero_delay_starts_with_exactly_one_silent_quantum','cyclic Delay did not enforce one quantum before output'),('every_delay_in_the_cycle_contributes_its_own_quantum','multiple cycle breakers changed the exact feedback latency'),('wpt_processing_model_cycle_and_delay_cases','official WPT processing-model sample mismatch')]:
            label='old-'+test;run(binary,label,PREFIX+test,101,message);reds.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
        run(binary,'graph-pcm-before',GRAPH);old_graph=graph_pcm(work/'graph-pcm-before.log');acyclic=lambda rows:{k:v for k,v in rows.items()if k.split(':')[2]=='0'};assert acyclic(old_graph)==acyclic(graph)
        (work/'old-graph-comparison.json').write_text(json.dumps(dict(acyclic_complete_blocks=len(acyclic(graph)),all_acyclic_pcm_clock_collected_ended_equal=True,corrected_feedback_blocks=len(graph)-len(acyclic(graph)),old_feedback_errors_not_an_equivalence_target=True,old_complete_context_prefix_preserved=True),indent=2)+'\n');programs[-1]['pruned']=True;binary.unlink()
    finally:
        for name,data in old_sources.items():(package/'src'/name).write_bytes(data)
    for row in rows:
        try:
            for key,before,after in row['changes']:assert paths[key].read_text().count(before)==1;paths[key].write_text(paths[key].read_text().replace(before,after))
            binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion']);controls.append({k:v for k,v in row.items()if k!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Fresh feedback source fault rejected',row['name'],flush=True)
        finally:
            for key,_,_ in row['changes']:paths[key].write_text(originals[key])
    release=build('positive-release',True);run(release,'full-release');run(release,'feedback-pcm-release',PCM);run(release,'graph-pcm-release',GRAPH);run(release,'wpt-release',WPT)
    assert feedback_pcm(work/'feedback-pcm-release.log')==current and graph_pcm(work/'graph-pcm-release.log')==graph
    product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src'if name=='audio'else package/'local-deps'/name/'src';assert all((private/p.relative_to(src)).read_bytes()==p.read_bytes()for p in src.rglob('*')if p.is_file()),name
    pcm=dict(independent_recurrence_blocks=len(current),independent_recurrence_samples=sum(len(v['pcm'])for v in current.values()),complete_graph_blocks=len(graph),complete_graph_samples=sum(len(v['pcm'])for v in graph.values()),wpt_cases=3,wpt_samples=1536,dev_o3_all_pcm_and_clocks_equal=True,old_acyclic_all_pcm_clock_collected_ended_equal=True,browser_wpt_harness_executed=False)
    (work/'complete-pcm-comparison.json').write_text(json.dumps(pcm,indent=2)+'\n');(work/'summary.json').write_text(json.dumps(dict(programs=programs,suites=suites,controls=controls,original_red=reds,clippy=delta,pcm=pcm,root_inputs_unchanged=True,private_sources_restored=True,root_dev_profile_verified=True,isolated_o3_not_shipping=True),indent=2)+'\n');print('Feedback graph native validation passed',len(controls),'fresh source faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',required=True,type=Path);p.add_argument('--target-dir',required=True,type=Path);p.add_argument('--before-directory',required=True,type=Path);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
