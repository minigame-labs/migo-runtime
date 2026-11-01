#!/usr/bin/env python3
"""Admit all six FFT capacities before creation/replacement and retire their owner.

The test-only frozen analyser supplies the independent prior PCM/FFT reference.
Only fresh builds executing the specified Rust business assertion count as faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base

ROOT = base.ROOT
PREFIX = 'nodes::analyser::workspace_tests::'
FIR = 'context::steady_state_allocation::a_graph_change_on_the_render_path_never_reaches_the_heap'


def main(work: Path, target: Path, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths = {key: package/'src'/name for key, name in [('node','nodes/analyser.rs'),('workspace','nodes/analyser_workspace.rs'),('retirement','node_retirement.rs'),('fir','nodes/wave_shaper.rs')]}
    originals = {key:path.read_text() for key,path in paths.items()}
    constructor = '''        let reservation = workspace::Workspace::reserve(host, fft_size)?;
        let workspace = reservation.finish(workspace::Workspace::new(fft_size));'''
    constructor_early = '''        let data = workspace::Workspace::new(fft_size);
        let reservation = workspace::Workspace::reserve(host, fft_size)?;
        let workspace = reservation.finish(data);'''
    rows = [
        ('capacity-omitted','workspace','let bytes = size * 40;','let bytes = 0;','factory_admits_all_six_capacities_before_allocating_and_final_drop_uses_cpu','analyser factory bypassed workspace admission'),
        ('factory-allocation-first','node',constructor,constructor_early,'factory_admits_all_six_capacities_before_allocating_and_final_drop_uses_cpu','FFT factory allocated before admission'),
        ('resize-allocation-first','workspace','        let reservation = Self::reserve(host, size)?;\n        let mut next = Self::new(size);','        let mut next = Self::new(size);\n        let reservation = Self::reserve(host, size)?;','refused_resize_preserves_all_six_pointers_and_smoothing_state','FFT resize allocated before admission'),
        ('inline-workspace-drop','retirement','if self.lease.is_some() {','if false {','resize_matches_frozen_old_analyser_and_keeps_both_workspaces_until_cpu_drop','overlapping FFT workspaces lost physical credit'),
        ('unchanged-FFT-replaced','node','if size == self.fft_size {','if false {','resize_matches_frozen_old_analyser_and_keeps_both_workspaces_until_cpu_drop','unchanged FFT allocated workspace'),
        ('time-history-lost','workspace','next.time_domain_buffer[..keep].copy_from_slice(&previous.time_domain_buffer[..keep]);','next.time_domain_buffer[..keep].fill(0.0);','resize_matches_frozen_old_analyser_and_keeps_both_workspaces_until_cpu_drop','FFT workspace changed complete samples or history'),
        ('adoption-capacity-omitted','node','.prepare_with_bytes(host, self.workspace.capacity_bytes())?;','.prepare(host)?;','prebuilt_workspace_is_adopted_without_copy_and_foreign_ledger_is_refused','adopted FFT workspace omitted actual capacity'),
        ('repeated-FIR-loses-history','fir','    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {','    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {\n        self.up_state.clear();\n        self.down_state.clear();',FIR,'parameter replacement PCM or filter history changed'),
    ]
    for name,key,before,after,_,_ in rows:
        assert before != after and originals[key].count(before)==1,(name,'anchor drift')
    programs, controls = [], []
    def build(name,release=False):
        binary=product.src.build(package,work,env,name,release)
        programs.append(dict(name=name,path=str(binary),sha256=product.whole.support.codecs.sha(binary),pruned=False))
        return binary
    def run(binary,label,selector='',expected=0,assertion=None):
        cmd=[str(binary)]+(['--exact',selector]if selector else [])+['--nocapture']
        code=common.run(cmd,work/(label+'.log'),package,env,timeout=180)
        (work/(label+'-exit.txt')).write_text(str(code)+'\n');text=(work/(label+'.log')).read_text();assert code==expected,(label,code)
        if expected:assert all(x in text for x in ['running 1 test','panicked at','1 failed',assertion]),label
        else:assert re.search(r'test result: ok\. [1-9][0-9]* passed; 0 failed;',text),label
    positive=build('positive-debug')
    base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    for test in sorted({r[4]for r in rows}):run(positive,'positive-'+test,PREFIX+test if '::'not in test else test)
    assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        run(positive,'full-debug')
        for name,key,before,after,test,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after));binary=build(name)
                selector=PREFIX+test if '::'not in test else test
                run(binary,name,selector,101,assertion);controls.append(dict(name=name,test=selector,exit=101,expected_assertion=assertion,sha256=programs[-1]['sha256']))
                programs[-1]['pruned']=True;binary.unlink();print('Actual FFT workspace fault rejected',name,flush=True)
            finally:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release')
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    result=dict(programs=programs,controls=controls,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,preflight=preflight)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('FFT workspace ownership verified',len(controls),'actual source faults',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
