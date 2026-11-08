#!/usr/bin/env python3
"""Test-only mature time-varying SRC probe. No production cutover or Git."""
from __future__ import annotations
import argparse,hashlib,json,os,re,shutil,subprocess,tarfile,tomllib,math,random,statistics
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
FIXTURE='scripts/testing/fixtures/variable_rate_probe.rs'
HELPER='scripts/testing/fixtures/variable_rate_alloc.c'
HISTORY_PATCH='scripts/testing/fixtures/libsamplerate-0.2.2-retain-history.patch'
ARCHIVE_SHA='3258da280511d24b49d6b08615bbe824d0cacc9842b0e4caf11c52cf2b043893'
ARCHIVE_URL='https://github.com/libsndfile/libsamplerate/releases/download/0.2.2/libsamplerate-0.2.2.tar.xz'

def sha(p):
    with p.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def section(text,name):
    m=re.search(r'^\['+re.escape(name)+r'\]\n(.*?)(?=^\[|\Z)',text,re.M|re.S);assert m,name;return m[1]
def run(cmd,log,cwd,env):
    log.with_suffix(log.suffix+'.command.json').write_text(json.dumps(cmd)+'\n')
    with log.open('w') as out:p=subprocess.run(cmd,cwd=cwd,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=900)
    return p.returncode

def native(work,name,archive,helper,level,env,cutoff_fault=False,sanitize=False,history_fault=False):
    n=work/name;n.mkdir(exist_ok=False);assert sha(archive)==ARCHIVE_SHA
    with tarfile.open(archive) as tar:
        for member in tar:
            assert not Path(member.name).is_absolute() and '..' not in Path(member.name).parts and (member.isfile() or member.isdir())
        tar.extractall(n,filter='data')
    s=n/'libsamplerate-0.2.2';inputs={str(p.relative_to(s)):sha(p) for p in s.rglob('*') if p.is_file()}
    if not history_fault:assert run(['patch','--batch','--forward','-p1','--fuzz=0','-i',str(ROOT/HISTORY_PATCH)],n/'history-patch.log',s,env)==0
    if cutoff_fault:
        p=s/'src/src_sinc.c';text=p.read_text();anchor='filter->index_inc * (src_ratio < 1.0 ? src_ratio : 1.0)';assert text.count(anchor)==5;p.write_text(text.replace(anchor,'filter->index_inc * 1.0'))
    shutil.copy2(helper,n/'probe_alloc.c');flags='-O'+level+' -g1'
    cmd=['cmake','-S',str(s),'-B',str(n/'build'),'-DCMAKE_BUILD_TYPE=Release','-DCMAKE_C_FLAGS_RELEASE='+flags,'-DBUILD_SHARED_LIBS=OFF','-DBUILD_TESTING=OFF','-DLIBSAMPLERATE_EXAMPLES=OFF','-DLIBSAMPLERATE_INSTALL=OFF','-DLIBSAMPLERATE_ENABLE_SINC_FAST_CONVERTER=OFF','-DLIBSAMPLERATE_ENABLE_SINC_MEDIUM_CONVERTER=OFF','-DCMAKE_EXPORT_COMPILE_COMMANDS=ON']
    if sanitize:cmd.append('-DLIBSAMPLERATE_ENABLE_SANITIZERS=ON')
    assert run(cmd,n/'configure.log',ROOT,env)==0
    assert run(['cmake','--build',str(n/'build'),'--parallel','2'],n/'build.log',ROOT,env)==0
    assert run(['cc','-std=c11','-Wall','-Wextra','-Werror','-O'+level,'-g1',*(['-fsanitize=address,undefined','-fno-sanitize-recover=all','-fno-omit-frame-pointer'] if sanitize else []),'-I'+str(s/'include'),'-c',str(n/'probe_alloc.c'),'-o',str(n/'probe_alloc.o')],n/'helper-build.log',ROOT,env)==0
    assert run(['ar','rcs',str(n/'libvariable_rate_alloc.a'),str(n/'probe_alloc.o')],n/'helper-ar.log',ROOT,env)==0
    libraries=[n/'libvariable_rate_alloc.a',n/'build/src/libsamplerate.a'];record=dict(source_url=ARCHIVE_URL,source_archive_sha256=ARCHIVE_SHA,original_source_sha256=inputs,actual_source_sha256={str(p.relative_to(s)):sha(p) for p in s.rglob('*') if p.is_file()},cutoff_fault=cutoff_fault,history_patch_applied=not history_fault,history_patch_sha256=sha(ROOT/HISTORY_PATCH),isolated_native_c_opt=level,libraries={str(p):sha(p) for p in libraries},compiler_objects={str(p):sha(p) for p in (n/'build').rglob('*.o')},generated_config_sha256=sha(n/'build/config.h'),helper_source_sha256=sha(n/'probe_alloc.c'),all_compile_output_paths_new=True,shipping_acceptance=False)
    record['address_undefined_sanitizers']=sanitize
    (n/'receipt.json').write_text(json.dumps(record,indent=2)+'\n')
    e=env.copy();e['CARGO_ENCODED_RUSTFLAGS']='\x1f'.join(['-Lnative='+str(n),'-Lnative='+str(n/'build/src'),'-lstatic=variable_rate_alloc','-lstatic=samplerate','-lm',*sum((['-C','link-arg=-Wl,--wrap='+name] for name in ['malloc','calloc','realloc','free']),[])])
    if sanitize:e['CARGO_ENCODED_RUSTFLAGS']+='\x1f-C\x1flink-arg=-lasan\x1f-C\x1flink-arg=-lubsan';e['MIGO_NATIVE_SANITIZED_TARGET']='x86_64-unknown-linux-gnu'
    return e

def binary(work,p,e,name,profile='dev',test=True):
    (p/'src/main.rs').touch();cmd=['cargo','test' if test else 'build','--bin','variable-rate-probe','--offline','--locked','--message-format=json']
    if test:cmd.append('--no-run')
    if e.get('MIGO_NATIVE_SANITIZED_TARGET'):cmd+=['--target',e['MIGO_NATIVE_SANITIZED_TARGET']]
    if profile!='dev':cmd+=['--profile',profile]
    log=work/(name+'-build.jsonl');assert run(cmd,log,p,e)==0,log
    rows=[]
    for line in log.read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name')=='variable-rate-probe' and r.get('executable'):rows.append(r)
    assert len(rows)==1 and rows[0]['fresh'] is False
    dst=work/(name+'-tests' if test else name+'-program');shutil.copy2(rows[0]['executable'],dst)
    return dict(name=name,path=str(dst),sha256=sha(dst),artifact=rows[0],fresh=False,profile=profile,test=test)

def build(work,target,archive):
    assert not work.exists();(work/'probe/src').mkdir(parents=True);(work/'tmp').mkdir();p=work/'probe'
    inputs={n:sha(ROOT/n) for n in [FIXTURE,HELPER,HISTORY_PATCH,str(Path(__file__).relative_to(ROOT)),'scripts/testing/check_gitignore_contract.py','engine/Cargo.toml','engine/Cargo.lock','engine/.cargo/config.toml']}
    for n in inputs:
        q=work/'source-inputs'/n;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/n,q)
    (work/'source-inputs.json').write_text(json.dumps(inputs,indent=2)+'\n');shutil.copy2(ROOT/FIXTURE,p/'src/main.rs')
    root=(ROOT/'engine/Cargo.toml').read_text();profiles=''.join('['+n+']\n'+section(root,n) for n in ['profile.dev','profile.dev.package."*"','profile.release','profile.release.build-override','profile.release.package."*"'])
    (p/'Cargo.toml').write_text(f'''[package]
name="migo-variable-rate-probe"
version="0.1.0"
edition="2024"
[workspace]
[dependencies]
libc="0.2"
[dev-dependencies]
migo-alloc-probe={{path="{ROOT/'engine/testing/alloc-probe'}",default-features=false}}
[[bin]]
name="variable-rate-probe"
path="src/main.rs"
{profiles}
[profile.dev.package.migo-alloc-probe]
opt-level=0
[profile.probe-o3]
inherits="release"
opt-level=3
lto=false
strip=false
[profile.probe-o3.package."*"]
opt-level=3
codegen-units=1
''');shutil.copy2(ROOT/'engine/Cargo.lock',p/'Cargo.lock');env=os.environ.copy()
    for k in ['RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_BUILD_TARGET']:env.pop(k,None)
    env.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_INCREMENTAL='0',CARGO_NET_OFFLINE='true',CARGO_TARGET_DIR=str(target),TMPDIR=str(work/'tmp'))
    dev=native(work,'native-debug',archive,ROOT/HELPER,'0',env);release=native(work,'native-o3',archive,ROOT/HELPER,'3',env)
    assert run(['cargo','check','--all-targets','--offline','--message-format=json'],work/'prepare-check.jsonl',p,dev)==0
    identities=lambda text:{(r['name'],r['version'],r.get('source'),r.get('checksum')) for r in tomllib.loads(text)['package']}
    assert identities((p/'Cargo.lock').read_text())-{('migo-variable-rate-probe','0.1.0',None,None)}<=identities((ROOT/'engine/Cargo.lock').read_text())
    (work/'lock-policy.json').write_text(json.dumps(dict(root_unchanged=sha(ROOT/'engine/Cargo.lock')==inputs['engine/Cargo.lock'],all_dependency_identities_from_root=True),indent=2)+'\n')
    assert run(['cargo','clippy','--all-targets','--offline','--locked','--message-format=json'],work/'clippy.jsonl',p,dev)==0
    messages=[]
    for line in (work/'clippy.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-message' and any(s.get('is_primary') and Path(s['file_name']).name=='main.rs' for s in r['message']['spans']):messages.append(r['message'])
    assert not messages,[(r['level'],r.get('code'),r['message']) for r in messages]
    programs=[binary(work,p,dev,'positive-debug'),binary(work,p,release,'positive-o3','probe-o3'),binary(work,p,release,'root-release-test','release'),binary(work,p,release,'root-release','release',False)]
    checked={}
    for line in (work/'positive-debug-build.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name') in ['libc','migo_alloc_probe']:
            role=r['target']['name'];expected='2' if role=='libc' else '0';actual={k:r['profile'][k] for k in ['opt_level','debuginfo','debug_assertions','overflow_checks']};assert actual==dict(opt_level=expected,debuginfo=2,debug_assertions=True,overflow_checks=True),(role,actual);checked[role]=actual
    assert len(checked)==2
    for name,opt in [('positive-debug','0'),('positive-o3','3'),('root-release-test','z'),('root-release','z')]:assert next(r for r in programs if r['name']==name)['artifact']['profile']['opt_level']==opt
    assert tomllib.loads((p/'Cargo.toml').read_text())['profile']['release']==tomllib.loads(root)['profile']['release']
    (work/'actual-profiles.json').write_text(json.dumps(dict(root_dev_roles=checked,root_release_manifest_equal=True,root_release_opt='z',native_c_profiles=['0','3'],isolated_o3=True,shipping_acceptance=False),indent=2)+'\n')
    sanitized=native(work,'native-sanitized',archive,ROOT/HELPER,'1',env,sanitize=True);sanrow=binary(work,p,sanitized,'sanitized-debug');sanrow['sanitizers']=True;sanrow['asan_runtime']=subprocess.check_output(['cc','-print-file-name=libasan.so'],text=True).strip();programs.append(sanrow)
    original=(ROOT/FIXTURE).read_text();faults=[
      ('wrong-bench-offer','\"wide\" => 8192 / channels,','\"wide\" => 128,','benchmark_mode_reaches_the_actual_native_input','benchmark mode did not reach native input'),
      ('floor-endpoint','(frames as u128 * 1000).div_ceil(self.rates[0] as u128)','((frames as u128 * 1000) / self.rates[0] as u128)','fixed_complete_pcm_matches_independent_full_source','variable-rate endpoint disagrees with independent C clock'),
      ('early-native-eof','eof: 0,','eof: i32::from(read + available == frames),','fixed_complete_pcm_matches_independent_full_source','native SRC stalled before authoritative endpoint'),
      ('owner-leak','assert!(native(|| unsafe { src_delete(self.0.as_ptr()) }).is_null());','std::hint::black_box(self.0.as_ptr());','native_actual_capacity_and_all_three_pointers_are_released','native kernel heap remained after destruction'),
      ('hot-rust-allocation','while written < end {','while written < end {\n        std::hint::black_box(vec![0u8; 32]);','ratio_and_processing_calls_have_no_rust_or_native_allocations','variable-rate Rust hot path allocated'),
      ('skip-reset','assert_eq!(native(|| unsafe { src_reset(self.0.as_ptr()) }), 0);','std::hint::black_box(self.0.as_ptr());','reset_reuses_native_capacity_and_restores_every_sample','variable-rate source ingestion did not reach EOF'),
      ('invalid-ratio','if !ratio.is_finite() || !(1. / 256. ..=256.).contains(&ratio) {','if ratio.is_finite() && !(1. / 256. ..=256.).contains(&ratio) {','invalid_ratio_is_rejected_before_native_state_changes','invalid native ratio was accepted'),
    ]
    for name,before,after,selector,panic in faults:
        assert original.count(before)==1,(name,'anchor drift');(p/'src/main.rs').write_text(original.replace(before,after));row=binary(work,p,dev,name);row.update(selector='tests::'+selector,panic=panic,expected=101);programs.append(row)
    (p/'src/main.rs').write_text(original)
    bad=native(work,'native-cutoff-fault',archive,ROOT/HELPER,'3',env,True);row=binary(work,p,bad,'unfiltered-dynamic');row.update(selector='tests::variable_cutoff_preserves_high_end_and_rejects_aliases',panic='dynamic native cutoff failed alias rejection',expected=101);programs.append(row)
    old=native(work,'native-original-history',archive,ROOT/HELPER,'3',env,history_fault=True);row=binary(work,p,old,'original-history');row.update(selector='tests::dynamic_complete_pcm_and_clock_survive_fragmentation',panic='variable-rate complete native PCM changed',expected=101);programs.append(row)
    programs.append(binary(work,p,dev,'restored-debug'))
    assert sha(p/'src/main.rs')==inputs[FIXTURE] and sha(ROOT/'engine/Cargo.lock')==inputs['engine/Cargo.lock']
    (work/'programs.json').write_text(json.dumps(programs,indent=2)+'\n');print('Built',len(programs),'fresh variable-rate programs.',flush=True)

def execute(work):
    programs=json.loads((work/'programs.json').read_text());results={};env=os.environ.copy()
    for row in programs:
        assert sha(Path(row['path']))==row['sha256']
        if row.get('sanitizers'):continue  # Same-SHA sanitizer run has its own explicit execution phase.
        if not row['test']:
            log=work/(row['name']+'.log');assert run([row['path']],log,work,env)==0;normal=json.loads(log.read_text());assert normal['panic']=='abort' and normal['released'] and normal['native_bytes']==880228;continue
        env=os.environ.copy()
        if row.get('sanitizers'):env.update(LD_PRELOAD=row['asan_runtime'],ASAN_OPTIONS='detect_leaks=1:halt_on_error=1',UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1');env.pop('LSAN_OPTIONS',None)
        name=row['name'];directory=work/'pcm'/name;directory.mkdir(parents=True,exist_ok=True);env['MIGO_VARIABLE_RATE_PCM_DIR']=str(directory)
        cmd=[row['path'],'--nocapture']
        if row.get('selector'):cmd=[row['path'],'--exact',row['selector'],'--nocapture']
        code=run(cmd,work/(name+'.log'),work,env);(work/(name+'-exit.txt')).write_text(str(code)+'\n');assert code==row.get('expected',0),(name,code);text=(work/(name+'.log')).read_text()
        if code:assert all(v in text for v in ['running 1 test','1 failed','panicked at',row['selector'],row['panic']]),name
        else:assert 'test result: ok. 8 passed; 0 failed; 0 ignored;' in text,name
        results[name]=dict(exit=code,sha256=row['sha256']);print(name,'verified',code,flush=True)
    pcm={}
    for label in ['positive-debug','positive-o3','root-release-test','restored-debug']:
        files={p.name:p.read_bytes() for p in (work/'pcm'/label).glob('*.f32le')};assert files
        for n,data in files.items():
            if n.endswith('-actual.f32le') and n.replace('-actual.f32le','-reference.f32le') in files:assert data==files[n.replace('-actual.f32le','-reference.f32le')],n
        pcm[label]=files
    assert all(v==pcm['positive-debug'] for v in pcm.values())
    (work/'pcm-sha256.json').write_text(json.dumps({k:{n:hashlib.sha256(v).hexdigest() for n,v in files.items()} for k,files in pcm.items()},indent=2)+'\n')
    (work/'execute-summary.json').write_text(json.dumps(dict(programs=programs,results=results,full_pcm_outputs_each=len(pcm['positive-debug']),full_pcm_samples_each=sum(len(v)//4 for v in pcm['positive-debug'].values()),all_complete_dev_o3_z_restored_bits_equal=True,production_integrated=False,sanitizers_pending=True),indent=2)+'\n')

def execute_sanitized(work):
    row=next(r for r in json.loads((work/'programs.json').read_text()) if r.get('sanitizers'));assert sha(Path(row['path']))==row['sha256'];directory=work/'pcm'/row['name'];directory.mkdir(parents=True,exist_ok=True);env=os.environ.copy();env.update(LD_PRELOAD=row['asan_runtime'],ASAN_OPTIONS='detect_leaks=1:halt_on_error=1',UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1',MIGO_VARIABLE_RATE_PCM_DIR=str(directory));env.pop('LSAN_OPTIONS',None)
    code=run([row['path'],'--nocapture'],work/(row['name']+'.log'),work,env);(work/(row['name']+'-exit.txt')).write_text(str(code)+'\n');assert code==0,code;assert 'test result: ok. 8 passed; 0 failed; 0 ignored;' in (work/(row['name']+'.log')).read_text()
    hashes=json.loads((work/'pcm-sha256.json').read_text());files={p.name:p.read_bytes() for p in directory.glob('*.f32le')};selected={n:hashlib.sha256(v).hexdigest() for n,v in files.items()};assert selected==hashes['positive-debug'];hashes[row['name']]=selected;(work/'pcm-sha256.json').write_text(json.dumps(hashes,indent=2)+'\n')
    summary=json.loads((work/'execute-summary.json').read_text());summary['results'][row['name']]=dict(exit=code,sha256=row['sha256'],asan_ubsan_default_leaks_enabled=True);summary['sanitizers_pending']=False;summary['full_positive_profiles']=5;summary['all_complete_profile_bits_equal']=True;(work/'execute-summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('Same-SHA ASan/UBSan/default-leak checks and all8tests/fullPCM passed.',flush=True)

def benchmark(work):
    program=next(r for r in json.loads((work/'programs.json').read_text()) if not r['test']);assert sha(Path(program['path']))==program['sha256'];directory=work/'bench';directory.mkdir(exist_ok=False);rows=[];env=os.environ.copy();cases=[(500,1),(503,2),(1333,2),(2000,1),(2000,8),(1333,32),(0,2)]
    def endpoint(frames,units):
        if units:return (frames*1000+units-1)//units
        clock=n=0;rates=[1000,2000,503,500,1333]
        while clock<frames*1000:clock+=rates[(n//128)%len(rates)];n+=1
        return n
    def one(label,mode,units,channels,frames,iterations):
        output=directory/(label+'.json');timing=directory/(label+'.time');cmd=['/usr/bin/time','-f','%U %S %M %e','-o',str(timing),program['path'],mode,str(units),str(channels),str(frames),str(iterations)];assert run(cmd,output,work,env)==0,label;r=json.loads(output.read_text());assert r['mode']==mode and r['actual_native_offer']==(128 if mode=='small' else 8192//channels) and r['units']==units and r['channels']==channels and r['input_frames']==frames and r['iterations']==iterations and r['output_frames']==endpoint(frames,units) and r['cpu_ns']>0 and r['native_calls']>0 and r['panic']=='abort' and math.isfinite(r['checksum']);u,s,rss,wall=timing.read_text().split();return dict(r,label=label,program_sha256=program['sha256'],process_user_s=float(u),process_system_s=float(s),max_rss_kib=int(rss),process_wall_s=float(wall))
    for units,channels in cases:
        frames=65537 if channels>=8 else 480137;pilot=one(f'pilot-{units}-{channels}','small',units,channels,frames,1);iterations=max(2,min(128,math.ceil(200_000_000/pilot['cpu_ns'])))
        for round_index in range(6):
            order=['small','wide','wide','small'] if round_index%2==0 else ['wide','small','small','wide']
            for slot,mode in enumerate(order):
                r=one(f'{units}-{channels}-{round_index}-{slot}',mode,units,channels,frames,iterations);r.update(round=round_index,slot=slot);rows.append(r)
        print('Measured',units,channels,iterations,flush=True)
    summary=[];rng=random.Random(9501)
    for units,channels in cases:
        group=[r for r in rows if r['units']==units and r['channels']==channels];ratios=[]
        for index in range(6):
            a=statistics.mean(r['cpu_ns']/r['iterations'] for r in group if r['round']==index and r['mode']=='small');b=statistics.mean(r['cpu_ns']/r['iterations'] for r in group if r['round']==index and r['mode']=='wide');ratios.append(b/a)
        draws=sorted(statistics.median(rng.choices(ratios,k=6)) for _ in range(5000));summary.append(dict(units=units,channels=channels,wide_over_small_cpu_median=statistics.median(ratios),paired_round_ratios=ratios,bootstrap_median_ci95=[draws[125],draws[4874]],small_rss_kib_median=statistics.median(r['max_rss_kib'] for r in group if r['mode']=='small'),wide_rss_kib_median=statistics.median(r['max_rss_kib'] for r in group if r['mode']=='wide'),small_native_calls_per_iteration=statistics.median(r['native_calls']/r['iterations'] for r in group if r['mode']=='small'),wide_native_calls_per_iteration=statistics.median(r['native_calls']/r['iterations'] for r in group if r['mode']=='wide')))
    (work/'benchmark-results.json').write_text(json.dumps(dict(program=program,runs=len(rows),pilots=len(cases),cases=summary,rows=rows,ordering='6ABBA/BAAB rounds',timer='CLOCK_PROCESS_CPUTIME_ID; preallocated output; native reset included; constructor/reference excluded',scope='same best-sinc native backend and full PCM; isolated host Root release caller z/native O3',production_integrated=False),indent=2)+'\n')

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--phase',choices=['build','execute','sanitize','bench'],required=True);p.add_argument('--work',type=Path,required=True);p.add_argument('--target',type=Path);p.add_argument('--source-archive',type=Path);a=p.parse_args();work=a.work.resolve()
    if a.phase=='build':assert a.target and a.source_archive;build(work,a.target.resolve(),a.source_archive.resolve())
    elif a.phase=='execute':execute(work)
    elif a.phase=='sanitize':execute_sanitized(work)
    else:benchmark(work)
if __name__=='__main__':main()
