#!/usr/bin/env python3
"""Isolated mature playback-rate quality/CPU decision; no production cutover or Git."""
from __future__ import annotations
import argparse,hashlib,json,os,re,shutil,subprocess,tomllib,statistics,random,math
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
FIXTURE='scripts/testing/fixtures/playback_rate_probe.rs'
ADAPTER='engine/crates/audio/src/bandlimited.rs'
TESTS='tests::'

def sha(p):
    with p.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()

def section(text,name):
    match=re.search(r'^\['+re.escape(name)+r'\]\n(.*?)(?=^\[|\Z)',text,re.M|re.S)
    assert match,name
    return match[1]

def run(cmd,path,cwd,env):
    with path.open('w') as out:r=subprocess.run(cmd,cwd=cwd,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=900)
    return r.returncode

def env(target):
    e=os.environ.copy()
    for name in ['RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_BUILD_TARGET']:e.pop(name,None)
    e.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_INCREMENTAL='0',CARGO_NET_OFFLINE='true',CARGO_TARGET_DIR=str(target))
    return e

def prepare(work,target):
    assert not work.exists();work.mkdir(parents=True);p=work/'probe';(p/'src').mkdir(parents=True)
    names=[FIXTURE,ADAPTER,str(Path(__file__).relative_to(ROOT)),'scripts/testing/check_gitignore_contract.py','engine/Cargo.toml','engine/Cargo.lock','engine/.cargo/config.toml']
    inputs={name:sha(ROOT/name) for name in names}
    for name in names:
        saved=work/'source-inputs'/name;saved.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,saved)
    shutil.copy2(ROOT/FIXTURE,p/'src/main.rs');shutil.copy2(ROOT/ADAPTER,p/'src/bandlimited.rs')
    shim='''pub mod bandlimited;
pub fn bind_end<R: rubato::Resampler<f32>>(owner: &mut bandlimited::Stream<R>, frames: u64) -> Result<(), String> { owner.set_output_end(frames) }
pub fn quiet<R: rubato::Resampler<f32>>(owner: &bandlimited::Stream<R>, points: usize) -> Result<usize, String> { owner.quiet_input_frames(points) }
'''
    (p/'src/lib.rs').write_text(shim)
    text=(ROOT/'engine/Cargo.toml').read_text();patch=section(text,'patch.crates-io');patch=re.sub(r'path\s*=\s*"([^"]+)"',lambda m:'path = '+json.dumps(str((ROOT/'engine'/m[1]).resolve())),patch)
    profiles=''.join('['+name+']\n'+section(text,name) for name in ['profile.dev','profile.dev.package."*"','profile.release','profile.release.build-override','profile.release.package."*"'])
    # Preserve the actual Root dev/release policy. O3 is separately labelled.
    assert '[profile.release]' in profiles
    manifest=f'''[package]
name = "migo-playback-rate-probe"
version = "0.1.0"
edition = "2024"
[workspace]
[dependencies]
libc = "0.2"
rubato = {{ path = "{ROOT/'engine/third_party/rubato-5.0.1'}", default-features = false }}
[dev-dependencies]
migo-alloc-probe = {{ path = "{ROOT/'engine/testing/alloc-probe'}", default-features = false }}
[[bin]]
name = "playback-rate-probe"
path = "src/main.rs"
{profiles}
[profile.dev.package.migo-alloc-probe]
opt-level = 0
[profile.probe-o3]
inherits = "release"
opt-level = 3
lto = false
strip = false
[profile.probe-o3.package."*"]
opt-level = 3
codegen-units = 1
[profile.probe-o3.build-override]
opt-level = 3
codegen-units = 1
[patch.crates-io]
{patch}
'''
    (p/'Cargo.toml').write_text(manifest);shutil.copy2(ROOT/'engine/Cargo.lock',p/'Cargo.lock');e=env(target)
    assert run(['cargo','check','--all-targets','--offline','--message-format=json'],work/'prepare-check.jsonl',p,e)==0
    identities=lambda d:{(x['name'],x['version'],x.get('source'),x.get('checksum')) for x in d['package']}
    old=identities(tomllib.loads((ROOT/'engine/Cargo.lock').read_text()));selected=identities(tomllib.loads((p/'Cargo.lock').read_text()));assert selected-{('migo-playback-rate-probe','0.1.0',None,None)}<=old
    (work/'lock-policy.json').write_text(json.dumps(dict(root_unchanged=sha(ROOT/'engine/Cargo.lock')==inputs['engine/Cargo.lock'],identities_from_root_except_probe=True,selected_packages=len(selected)),indent=2)+'\n')
    (work/'source-inputs.json').write_text(json.dumps(inputs,indent=2)+'\n');(work/'generated-inputs.json').write_text(json.dumps({str(q.relative_to(p)):sha(q) for q in [p/'src/lib.rs',p/'Cargo.toml',p/'Cargo.lock']},indent=2)+'\n')
    return p,e

def binary(work,p,e,name,profile='dev',test=True):
    (p/'src/main.rs').touch();cmd=['cargo','test' if test else 'build','--bin','playback-rate-probe','--offline','--locked','--message-format=json']
    if test:cmd.append('--no-run')
    if profile!='dev':cmd+=['--profile',profile]
    log=work/(name+'-build.jsonl');assert run(cmd,log,p,e)==0,log
    rows=[]
    for line in log.read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name')=='playback-rate-probe' and r.get('executable'):rows.append(r)
    assert len(rows)==1 and rows[0]['fresh'] is False
    dst=work/(name+'-tests' if test else name+'-program');shutil.copy2(rows[0]['executable'],dst)
    return dict(name=name,path=str(dst),sha256=sha(dst),artifact=rows[0],fresh=False,test=test,profile=profile)

def faults(originals):
    rows=[]
    def add(name,key,before,after,selector,panic):
        assert originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,key=key,before=before,after=after,selector='tests::'+selector,panic=panic))
    add('unfiltered-downsample','fixture','&SincInterpolationParameters::default(),\n        quantum,','&SincInterpolationParameters { f_cutoff: Some(1.0 / ratio.min(1.0) as f32), ..Default::default() },\n        quantum,','fixed_rate_sinc_preserves_passband_and_rejects_aliases','rate sinc failed alias rejection')
    add('wrong-fixed-side','fixture','Mode::Output => (\n            FixedAsync::Output,','Mode::Output => (\n            FixedAsync::Input,','fixed_sides_have_the_same_quality_and_complete_time_origin','mature kernel uses wrong fixed side')
    add('floor-endpoint','fixture','(n as u128 * target as u128).div_ceil(source as u128) as usize','((n as u128 * target as u128) / source as u128) as usize','fixed_rates_keep_all_pcm_exactly_equal_to_the_full_mature_oracle','rate clock is not the exact rational endpoint')
    add('startup-delay-lost','adapter','self.delay = self.inner.output_delay();','self.delay = 0;','fixed_rates_keep_all_pcm_exactly_equal_to_the_full_mature_oracle','complete fixed-rate mature PCM changed')
    add('adapter-hot-allocation','adapter','        let need = self\n            .inner\n            .input_frames_next()','        std::hint::black_box(vec![0u8; 32]);\n        let need = self\n            .inner\n            .input_frames_next()','fixed_rates_keep_all_pcm_exactly_equal_to_the_full_mature_oracle','prepared rate adapter allocated in processing')
    add('oversized-quantum','fixture','128.min(((SAMPLES / channels - 130) as f64 * ratio).floor() as usize)','2048','fixed_rates_keep_all_pcm_exactly_equal_to_the_full_mature_oracle','rate kernel exceeds original per-step sample budget')
    add('kernel-heap-leak','fixture','let ratio = target as f64 / source as f64;','std::mem::forget(std::hint::black_box(vec![0u8; 4096]));\n    let ratio = target as f64 / source as f64;','kernel_actual_heap_and_caller_capacity_are_released','rate probe retained kernel heap after destruction')
    return rows


def build(work,target):
    p,e=prepare(work,target)
    assert run(['cargo','clippy','--all-targets','--offline','--locked','--message-format=json'],work/'clippy.jsonl',p,e)==0
    messages=[]
    for line in (work/'clippy.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-message' and any(s.get('is_primary') and Path(s['file_name']).name in ['main.rs','lib.rs'] for s in r['message']['spans']):messages.append(r['message'])
    assert not messages,[(r['level'],r.get('code'),r['message']) for r in messages]
    programs=[binary(work,p,e,'positive-debug'),binary(work,p,e,'positive-o3','probe-o3'),binary(work,p,e,'root-release-test','release'),binary(work,p,e,'root-release','release',False)]
    policy=json.loads((ROOT/'docs/implementation/2026-10-25-runtime-modernization/results/W09-http-vorbis-controls-checkpoint.json').read_text())['root_profile']['expected'];checked={}
    for line in (work/'positive-debug-build.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')!='compiler-artifact':continue
        role=r['target']['name'].replace('_','-')
        if role in policy:
            actual={k:r['profile'][k] for k in ['opt_level','debuginfo','debug_assertions','overflow_checks']};assert actual==policy[role],(role,actual,policy[role]);checked[role]=actual
    assert {'rubato','audioadapter','audioadapter-buffers','audioadapter-sample','migo-alloc-probe'}<=set(checked),checked
    for name,opt in [('positive-debug','0'),('positive-o3','3'),('root-release-test','z'),('root-release','z')]:
        row=next(x for x in programs if x['name']==name);assert row['artifact']['profile']['opt_level']==opt
    (work/'actual-profiles.json').write_text(json.dumps(dict(applicable_root_dev_roles=checked,root_release=dict(opt_level='z',panic='abort',lto='fat',codegen_units=1),isolated_o3=True,shipping_acceptance=False),indent=2)+'\n')
    paths={'fixture':p/'src/main.rs','adapter':p/'src/bandlimited.rs'};originals={k:v.read_text() for k,v in paths.items()}
    for case in faults(originals):
        try:
            paths[case['key']].write_text(originals[case['key']].replace(case['before'],case['after']));(p/'src/lib.rs').touch();row=binary(work,p,e,case['name']);row.update(expected=101,selector=case['selector'],panic=case['panic']);programs.append(row)
        finally:paths[case['key']].write_text(originals[case['key']])
    (p/'src/lib.rs').touch();programs.append(binary(work,p,e,'restored-debug'))
    assert all(v.read_text()==originals[k] for k,v in paths.items())
    (work/'programs.json').write_text(json.dumps(programs,indent=2)+'\n');print('Playback-rate probe built',len(programs),'fresh programs.',flush=True)

def execute(work):
    programs=json.loads((work/'programs.json').read_text());p=work/'probe';e=os.environ.copy();e['RUST_TEST_THREADS']='1';results={}
    for r in programs:
        assert sha(Path(r['path']))==r['sha256']
        if not r['test']:continue
        directory=work/'pcm'/r['name'];directory.mkdir(parents=True,exist_ok=True);e['MIGO_PLAYBACK_RATE_PCM_DIR']=str(directory)
        cmd=[r['path'],'--nocapture'];expected=r.get('expected',0)
        if r.get('selector'):cmd=[r['path'],'--exact',r['selector'],'--nocapture']
        code=run(cmd,work/(r['name']+'.log'),p,e);(work/(r['name']+'-exit.txt')).write_text(str(code)+'\n');assert code==expected,(r['name'],code)
        text=(work/(r['name']+'.log')).read_text()
        if expected:
            assert all(v in text for v in ['running 1 test','1 failed','panicked at',r['selector'],r['panic']]),r['name']
        else:
            assert 'test result: ok. 7 passed; 0 failed; 0 ignored;' in text,r['name']
        results[r['name']]=dict(exit=code,sha256=r['sha256']);print(r['name'],'verified',code,flush=True)
    release=next(r for r in programs if not r['test']);assert run([release['path'],'quality'],work/'quality.json',p,e)==0
    q=json.loads((work/'quality.json').read_text());assert q['fixed_alias_ratio']<0.0001 and q['ratio_only_alias_ratio']>0.5 and q['worst_cutoff_unity_gain']<0.001 and q['proper_unity_gain']>0.99
    pcm={}
    for label in ['positive-debug','positive-o3','root-release-test','restored-debug']:
        d=work/'pcm'/label
        if not d.is_dir():continue
        files={p.name:p.read_bytes() for p in d.glob('*.f32le')};assert len(files)==652,(label,len(files))
        for name,data in files.items():
            if name.endswith('-actual.f32le'):assert files[name.replace('-actual.f32le','-reference.f32le')]==data,name
        pcm[label]=files
    assert pcm['positive-debug']==pcm['positive-o3']
    if 'root-release-test' in pcm:assert pcm['positive-debug']==pcm['root-release-test']
    if 'restored-debug' in pcm:assert pcm['positive-debug']==pcm['restored-debug']
    (work/'pcm-sha256.json').write_text(json.dumps({k:{n:hashlib.sha256(v).hexdigest() for n,v in rows.items()} for k,rows in pcm.items()},indent=2)+'\n')
    (work/'execute-summary.json').write_text(json.dumps(dict(programs=programs,results=results,full_pcm_outputs_each=652,full_pcm_samples_each=sum(len(v)//4 for v in pcm['positive-debug'].values()),all_dev_o3_and_mature_bits_equal=True,quality=q),indent=2)+'\n')


def benchmark(work):
    program=next(r for r in json.loads((work/'programs.json').read_text()) if not r['test']);binary=Path(program['path']);assert sha(binary)==program['sha256'];e=os.environ.copy();d=work/'bench';d.mkdir(exist_ok=False);rows=[]
    cases=[(500,1),(503,2),(1000,2),(1333,2),(2000,1),(2000,8),(1333,32)]
    def one(label,mode,units,channels,frames,iterations):
        output=d/(label+'.json');clock=d/(label+'.time');cmd=['/usr/bin/time','-f','%U %S %M %e','-o',str(clock),str(binary),mode,str(units),str(channels),str(frames),str(iterations)]
        assert run(cmd,output,work/'probe',e)==0,label;r=json.loads(output.read_text());assert r['mode']==mode and r['fixed_side']==mode and r['units']==units and r['channels']==channels and r['input_frames']==frames and r['iterations']==iterations and r['panic']=='abort';assert r['output_frames']==(frames*1000+units-1)//units and r['cpu_ns']>0 and math.isfinite(r['checksum']);u,s,rss,wall=clock.read_text().split();return dict(r,label=label,program_sha256=program['sha256'],process_user_s=float(u),process_system_s=float(s),max_rss_kib=int(rss),process_wall_s=float(wall))
    for units,channels in cases:
        frames=65537 if channels>=8 else 480137;pilot=one(f'pilot-{units}-{channels}','input',units,channels,frames,1);iterations=max(2,min(128,math.ceil(200_000_000/pilot['cpu_ns'])))
        for round_index in range(6):
            order=['input','output','output','input'] if round_index%2==0 else ['output','input','input','output']
            for slot,mode in enumerate(order):
                row=one(f'{units}-{channels}-{round_index}-{slot}',mode,units,channels,frames,iterations);row.update(round=round_index,slot=slot);rows.append(row)
        print('Measured',units,channels,'iterations',iterations,flush=True)
    summary=[];rng=random.Random(9501)
    for units,channels in cases:
        selected=[r for r in rows if r['units']==units and r['channels']==channels];ratios=[]
        for round_index in range(6):
            a=statistics.mean(r['cpu_ns']/r['iterations'] for r in selected if r['round']==round_index and r['mode']=='input');b=statistics.mean(r['cpu_ns']/r['iterations'] for r in selected if r['round']==round_index and r['mode']=='output');ratios.append(b/a)
        draws=sorted(statistics.median(rng.choices(ratios,k=len(ratios))) for _ in range(5000));summary.append(dict(units=units,channels=channels,output_over_input_cpu_median=statistics.median(ratios),paired_round_ratios=ratios,bootstrap_median_ci95=[draws[125],draws[4874]],input_max_rss_kib=statistics.median(r['max_rss_kib'] for r in selected if r['mode']=='input'),output_max_rss_kib=statistics.median(r['max_rss_kib'] for r in selected if r['mode']=='output')))
    (work/'benchmark-results.json').write_text(json.dumps(dict(program=program,profile='actual Root release opt-z/LTO/panic-abort; isolated host only',runs=len(rows),rounds=6,paired_order='ABBA/BAAB',cpu_clock='CLOCK_PROCESS_CPUTIME_ID inside measured loop',rows=rows,cases=summary,device_or_shipping_acceptance=False),indent=2)+'\n')

def main():
    a=argparse.ArgumentParser(description=__doc__);a.add_argument('--phase',choices=['build','execute','bench'],required=True);a.add_argument('--work',type=Path,required=True);a.add_argument('--target',type=Path);args=a.parse_args();w=args.work.resolve()
    if args.phase=='build':assert args.target;build(w,args.target)
    elif args.phase=='execute':execute(w)
    else:benchmark(w)
if __name__=='__main__':main()
