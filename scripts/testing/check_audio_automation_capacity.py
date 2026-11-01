#!/usr/bin/env python3
"""Static-gain warmup missed first a-rate gain/delay scratch allocation.

Compare real pre-change nodes with identical cold tests, preserve per-frame
automation and delay tails, and reject fresh allocation/PCM faults at runtime.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
TEST, COMPAT = capacity.TESTS


def declarations(originals):
    rows = [
        ('gain-heap-scratch-reintroduced', 'gain.rs', 'let mut automation_buf = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'let mut automation_buf = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'cold context render allocated'),
        ('delay-heap-scratch-reintroduced', 'delay.rs', 'let mut automation = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'let mut automation = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'cold automation node allocated'),
        ('gain-automation-frozen', 'gain.rs', 'if self.gain.has_automation() {', 'if false {', 'automated gain PCM changed'),
        ('delay-fraction-discarded', 'delay.rs', 'let frac = delay_frames - whole;', 'let frac = 0.0_f32;', 'automated delay PCM changed'),
    ]
    for name,file,before,after,_ in rows: assert before!=after and originals[file].count(before)==1,(name,'anchor drift')
    return rows


def main(work, target, before_directory=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_automation_capacity.py' in frozen
    paths = {n: package / 'src/nodes' / n for n in ['gain.rs', 'delay.rs']}
    originals = {n: p.read_text() for n,p in paths.items()}
    rows = declarations(originals)
    programs, controls, red, suites = [], [], [], {}
    def record(name,binary,pruned=False):
        digest=product.whole.support.codecs.sha(binary)
        programs.append(dict(name=name,path=str(binary),sha256=digest,pruned=pruned))
        if pruned: binary.unlink()
        return digest
    positive=product.src.build(package,work,env,'positive-debug',False)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    record('positive-debug',positive)
    for n,test in enumerate([TEST,COMPAT]): capacity.run(positive,package,env,work,f'positive-targeted-{n}',test)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        if before_directory:
            for file in paths:
                label='before-'+file.removesuffix('.rs')
                old=(before_directory/file).read_text()
                (work/('before-'+file)).write_text(old)
                try:
                    paths[file].write_text(old)
                    binary=product.src.build(package,work,env,label,False)
                    assertion='cold context render allocated' if file=='gain.rs' else 'cold automation node allocated'
                    red.append(capacity.run(binary,package,env,work,label,TEST,101,assertion))
                    capacity.run(binary,package,env,work,label+'-compat',COMPAT)
                    record(label,binary,True)
                finally: paths[file].write_text(originals[file])
        for name,file,before,after,assertion in rows:
            try:
                paths[file].write_text(originals[file].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion)
                item['binary_sha256']=record(name,binary,True);controls.append(item)
                (work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n')
                print('Actual automation scratch fault rejected',name,flush=True)
            finally: paths[file].write_text(originals[file])
        suites['debug']=capacity.run(positive,package,env,work,'full-debug')
        release=product.src.build(package,work,env,'positive-release',True)
        record('positive-release',release)
        suites['isolated-release']=capacity.run(release,package,env,work,'full-release')
    product.whole.verify_inputs(work,frozen)
    root=ROOT/'engine/crates/audio/src'
    assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    result=dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,preflight=preflight,root_dev_profile_verified=True,isolated_release_is_not_shipping=True)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n')
    print('Automation scratch verified',len(controls),'faults',len(red),'old-code reds',len(suites),'full suites',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--before-directory',type=Path)
    parser.add_argument('--preflight',action='store_true')
    args=parser.parse_args()
    main(args.output.resolve(),args.target_dir.resolve(),args.before_directory,args.preflight)
