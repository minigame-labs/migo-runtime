#!/usr/bin/env python3
"""Frozen sealed File Vorbis controls, original CPU and complete PCM. No Git."""
from __future__ import annotations
import argparse,hashlib,json,os,re,shutil,subprocess,tarfile,tempfile
from pathlib import Path
import check_audio_http_mp3_controls as prior
ROOT=prior.ROOT
index=prior.index
sha=prior.sha
DIRECT='streaming_codecs::vorbis::tests::'
PUBLIC='streaming_file::tests::vorbis_controls::'
SEEK=PUBLIC+'public_vorbis_seek_restart_and_exact_endpoint_keep_complete_pcm'
LIB='engine/third_party/symphonia-format-ogg-0.6.1'


def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,panic):
        assert originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,key=key,before=before,after=after,test=test,expected_assertion=panic))
    add('vorbis-request-hidden','binding','if self.0.source_request().is_some() {','if false {',PUBLIC+'vorbis_control_deadline_covers_the_first_original_file_replay_read','packet exceeded its encoded window or ended incomplete')
    add('vorbis-file-offset-shifted','resume','''            FileRequest::Wave(offset) | FileRequest::Mp3(offset) | FileRequest::Vorbis(offset) => {
                offset
            }''','''            FileRequest::Wave(offset) | FileRequest::Mp3(offset) => offset,
            FileRequest::Vorbis(offset) => offset + 1,''',PUBLIC+'vorbis_control_deadline_covers_the_first_original_file_replay_read','Vorbis source request changed')
    add('vorbis-final-clock-plus-one','control','''        self.bind_frames(frames)?;
        self.read_state = ReadState::Pending(self.first_page, Resume::Packet);''','''        self.bind_frames(frames+1)?;
        self.read_state = ReadState::Pending(self.first_page, Resume::Packet);''',DIRECT+'nonzero_page_origin_is_normalized_without_changing_complete_pcm','SRC source ended before its declared duration')
    add('vorbis-granule-origin-lost','control','self.origin = Some(origin);','self.origin = Some(0);',DIRECT+'nonzero_page_origin_is_normalized_without_changing_complete_pcm','SRC source ended before its declared duration')
    add('vorbis-final-crc-ignored','page','if header.crc != crc.crc() { return decode_error("ogg: crc mismatch"); }','if false { return decode_error("ogg: crc mismatch"); }',DIRECT+'malformed_final_page_never_advertises_a_successful_presentation','invalid final Vorbis page produced successful PCM')
    add('vorbis-cache-lost','binding','''        let Some(offset) = self.0.source_request() else {''','''        if std::hint::black_box(true) { return Ok(None); }
        let Some(offset) = self.0.source_request() else {''',PUBLIC+'public_vorbis_loop_cutovers_keep_global_phase_and_tiny_tracks_reuse_the_original_window','controlled file failed while publishing complete suffix')
    add('vorbis-cancel-keeps-capacity','packet','''        if cancel.load(Ordering::Acquire) {
            self.owner = None;''','''        if cancel.load(Ordering::Acquire) {
            let _ = &self.owner;''',DIRECT+'original_cpu_really_destroys_all_retained_vorbis_buffer_capacity','original Vorbis CPU did not really free its full buffer capacity')
    add('vorbis-consecutive-clock-lost','window','let frames = clock.input_frames();','''let frames = owner.decoder.presentation_track().and_then(|t| t.num_frames)
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;''',DIRECT+'consecutive_cutovers_keep_the_validated_clock_while_headers_are_replayed','second Vorbis cutover lost its validated clock before replay headers')
    add('vorbis-seek-src-prefix-lost','binding','const SEEK_FROM_START: bool = true;','const SEEK_FROM_START: bool = false;',SEEK,'public File Vorbis complete suffix changed')
    add('vorbis-tail-crc-burst','binding','const DISCARDS_PER_STEP: usize = 1;','const DISCARDS_PER_STEP: usize = usize::MAX;',DIRECT+'hostile_tail_verifies_only_one_crc_candidate_per_original_cpu_step','one Vorbis CPU step retried multiple invalid CRC candidates')
    return rows


def library_clippy(work,env,before):
    c=index.base.capacity.product.whole.support.clock.common
    manifest=(work/'probe/Cargo.toml').read_text();patches=c.section(manifest,'patch.crates-io');counters={};packages={}
    for kind,source in [('before',before/LIB),('current',ROOT/LIB)]:
        p=work/'library-clippy'/kind/'symphonia-format-ogg-0.6.1';shutil.copytree(source,p);packages[kind]=p
        m=p/'Cargo.toml';m.write_text(m.read_text()+'\n[workspace]\n\n[patch.crates-io]\n'+patches);shutil.copy2(ROOT/'engine/Cargo.lock',p/'Cargo.lock')
        assert c.run(['cargo','check','--lib','--features','streaming','--offline','--message-format=json'],work/('ogg-'+kind+'-lock.jsonl'),p,env,timeout=300)==0
        assert c.run(['cargo','clippy','--lib','--tests','--features','streaming','--offline','--locked','--message-format=json'],work/('ogg-'+kind+'-clippy.jsonl'),p,env,timeout=600)==0
        counters[kind]=index.wave.diagnostics(work/('ogg-'+kind+'-clippy.jsonl'),p)
    a,z=counters['before'],counters['current'];assert a==z,dict(added=list((z-a).items()),removed=list((a-z).items()))
    delta=dict(before=sum(a.values()),after=sum(z.values()),added=0,removed=0,exact_counter_equal=True);(work/'ogg-clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');return packages['current'],delta


def verify_vendor(work,before):
    patches=ROOT/'engine/third_party/symphonia-format-ogg-patches';name,version,checksum=(patches/'UPSTREAM').read_text().split();cache=Path(os.environ.get('CARGO_HOME',str(Path.home()/'.cargo')))/'registry/cache';archive=next(cache.glob(f'*/{name}-{version}.crate'));assert sha(archive)==checksum
    c=index.base.capacity.product.whole.support.clock.common
    # Patch on the local filesystem: the NAS cannot preserve GNU patch's uid.
    with tempfile.TemporaryDirectory(prefix='migo-vorbis-published-') as temporary:
        stage=Path(temporary)
        with tarfile.open(archive) as tar:tar.extractall(stage,filter='data')
        for patch in sorted(patches.glob('*.patch')):assert c.run(['patch','--batch','--forward','--fuzz=0','-p0','-i',str(patch)],work/('ogg-'+patch.stem+'.log'),stage,os.environ.copy(),timeout=60)==0
        snapshot=lambda p:{str(q.relative_to(p)):sha(q) for q in p.rglob('*') if q.is_file()}
        expected=snapshot(stage/f'{name}-{version}');assert snapshot(ROOT/LIB)==expected
        shutil.copytree(stage/f'{name}-{version}',work/'published-ogg-replay'/f'{name}-{version}')
    (work/'ogg-published-source-proof.json').write_text(json.dumps(dict(archive_sha256=checksum,files=expected,patches={p.name:sha(p) for p in sorted(patches.glob('*.patch'))}),indent=2)+'\n')


def build(work,target,before,baseline,fixture_proof):
    os.environ['CARGO_INCREMENTAL']='0';P,E,frozen=index.base.prepare(work,target);c=index.base.capacity.product.whole.support.clock.common
    for name in [str(Path(__file__).relative_to(ROOT)),index.FIXTURE]:
        frozen[name]=sha(ROOT/name);q=work/'source-inputs'/name;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,q)
    fixture=work/index.FIXTURE;fixture.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/index.FIXTURE,fixture)
    proof=json.loads(fixture_proof.read_text());assert sha(ROOT/proof['producer_source'])==proof['producer_source_sha256'];gold={}
    for row in proof['files']:
        p=Path(row['path']);assert sha(p)==row['sha256'];q=work/'runtime-fixtures'/p.name;q.parent.mkdir(exist_ok=True);shutil.copy2(p,q);gold[p.name]=dict(row,path=str(q))
    (work/'runtime-fixture-inputs.json').write_text(json.dumps(dict(role='Actual producer/reference bytes materialized before compilation',sha256={index.FIXTURE:frozen[index.FIXTURE]},vorbis=gold,independent_ffmpeg=proof),indent=2)+'\n');(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    verify_vendor(work,before)
    private=work/'native/symphonia-format-ogg-0.6.1';shutil.copytree(ROOT/LIB,private);manifest=P/'Cargo.toml';text=manifest.read_text();assert str(ROOT/LIB) in text;manifest.write_text(text.replace(str(ROOT/LIB),str(private)))
    standalone,lib_delta=library_clippy(work,E,before)
    out=P/'src/output.rs';s=out.read_text()
    try:
        out.write_text(index.backend.ports(s));assert c.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',P,E,timeout=600)==0
    finally:out.write_text(s)
    a=index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe');z=index.wave.diagnostics(work/'clippy.jsonl',P);assert a==z,dict(added=list((z-a).items()),removed=list((a-z).items()))
    delta=dict(before=sum(a.values()),after=sum(z.values()),added=0,removed=0,exact_counter_equal=True);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    programs=[];steps=[]
    def binary(name,release=False):
        p=index.base.capacity.product.src.build(P,work,E,name,release);row=dict(name=name,path=str(p),sha256=sha(p),fresh=False);programs.append(row);return row
    old_plan=json.loads((baseline/'execution-plan.json').read_text())
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release);prior.capture_native(work,row)
        ogg=[]
        for line in (work/(row['name']+'-build.jsonl')).read_text().splitlines():
            try:artifact=json.loads(line)
            except ValueError:continue
            if artifact.get('reason')=='compiler-artifact' and artifact.get('target',{}).get('name')=='symphonia_format_ogg' and artifact['target']['kind']==['lib']:ogg.append(artifact)
        assert len(ogg)==1
        saved={};directory=work/'as-built-ogg'/row['name'];directory.mkdir(parents=True)
        for name in ogg[0]['filenames']:
            f=Path(name);q=directory/f.name;shutil.copy2(f,q);saved[q.name]=dict(sha256=sha(q),bytes=q.stat().st_size)
        (directory/'receipt.json').write_text(json.dumps(dict(program_sha256=row['sha256'],artifact=ogg[0],files=saved,captured_before_fault_builds=True),indent=2)+'\n')
        for old in old_plan['steps']:
            if old['program']['name']=='positive-'+label:steps.append(dict(old,program=row))
        for suffix,test in [('native',DIRECT),('file',PUBLIC)]:steps.append(dict(label='vorbis-'+suffix+'-'+label,program=row,expected=0,test=test,exact=False,vorbis_dump=label))
        (standalone/'src/lib.rs').touch();cmd=['cargo','test','--lib','--features','streaming','--offline','--locked','--no-run','--message-format=json']+(['--release'] if release else [])
        name='ogg-library-'+label;assert c.run(cmd,work/(name+'-build.jsonl'),standalone,E,timeout=600)==0
        rows=[]
        for line in (work/(name+'-build.jsonl')).read_text().splitlines():
            try:r=json.loads(line)
            except ValueError:continue
            if r.get('reason')=='compiler-artifact' and r.get('executable') and r.get('target',{}).get('name')=='symphonia_format_ogg':rows.append(r)
        assert len(rows)==1 and rows[0]['fresh'] is False
        p=work/(name+'-tests');shutil.copy2(rows[0]['executable'],p);r=dict(name=name,path=str(p),sha256=sha(p),fresh=False);programs.append(r);steps.append(dict(label=name,program=r,expected=0))
    paths={k:P/'src'/n for k,n in [('binding','streaming_codecs/vorbis.rs'),('resume','streaming_file_wave.rs'),('packet','streaming_codecs/packet.rs'),('window','streaming_codecs/packet_control.rs'),('file','streaming_file.rs')]};paths.update(control=private/'src/streaming/vorbis_control.rs',page=private/'src/page.rs');originals={k:p.read_text() for k,p in paths.items()}
    def constructor(text):
        a=text.index('    fn new(');b=text.index('    fn source_request(',a);return a,b,text[a:b]
    a,b,_=constructor(originals['file']);_,_,old=constructor((before/'engine/crates/audio/src/streaming_file.rs').read_text())
    try:
        paths['file'].write_text(originals['file'][:a]+old+originals['file'][b:]);row=binary('original-vorbis-file-constructor');steps.append(dict(label=row['name'],program=row,expected=101,test=SEEK,expected_assertion='actual Vorbis actor did not activate source controls',role='original_runtime_red'))
    finally:paths['file'].write_text(originals['file'])
    for f in declarations(originals):
        try:
            paths[f['key']].write_text(originals[f['key']].replace(f['before'],f['after']));row=binary(f['name']);steps.append(dict(label=row['name'],program=row,expected=101,test=f['test'],expected_assertion=f['expected_assertion'],role='fresh_source_fault'))
        finally:paths[f['key']].write_text(originals[f['key']])
    row=binary('restored-debug');steps.append(dict(label=row['name'],program=row,expected=0,test=PUBLIC,exact=False))
    index.base.capacity.product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k] for k,p in paths.items())
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,ogg_clippy=lib_delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('File Vorbis plan built',len(programs),len(steps),flush=True)


def execute(work,baseline):
    plan=json.loads((work/'execution-plan.json').read_text());suites={};negatives=[]
    for name,h in plan['source_inputs'].items():assert sha(ROOT/name)==sha(work/'source-inputs'/name)==h,name
    for step in plan['steps']:
        binary=Path(step['program']['path']);assert sha(binary)==step['program']['sha256']
        env=os.environ.copy();env['RUST_TEST_THREADS']='1'
        for name in ['HTTP_WAVE','HTTP_SPOOL','HTTP_M4A','HTTP_CONTROL','RANGE_CONTROL','WAVE_CONTROL']:env['MIGO_DUMP_'+name+'_PCM']='1'
        if step.get('dump'):env[dict(control='MIGO_DUMP_FILE_CONTROL_PCM',origin='MIGO_DUMP_SINC_ORIGIN_PCM',local='MIGO_DUMP_LOCAL_FILE_PCM',file='MIGO_DUMP_FILE_STREAM_PCM',indexed='MIGO_DUMP_INDEXED_FILE_PCM',package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]]='1'
        for key,folder,var in [('controls_dump','control-pcm','MIGO_MP3_CONTROL_PCM_DIR'),('mp3_dump','mp3-pcm','MIGO_MP3_GAPLESS_PCM_DIR'),('http_mp3_dump','http-mp3-pcm','MIGO_HTTP_MP3_PCM_DIR'),('vorbis_dump','vorbis-pcm','MIGO_VORBIS_CONTROL_PCM_DIR')]:
            if step.get(key):
                directory=work/folder/step[key];directory.mkdir(parents=True,exist_ok=True);env[var]=str(directory)
        if step.get('vorbis_dump'):
            directory=work/'actual-producer-fixtures'/step['vorbis_dump'];directory.mkdir(parents=True,exist_ok=True);env['MIGO_VORBIS_FIXTURE_DIR']=str(directory)
        cmd=[str(binary)]
        if step.get('test'):cmd+=(['--exact'] if step.get('exact',True) else [])+[step['test'],'--nocapture']
        path=work/(step['label']+'.log')
        with path.open('w') as log:r=subprocess.run(cmd,cwd=work/'probe',env=env,stdout=log,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(r.returncode)+'\n');content=path.read_text()
        assert r.returncode==step['expected'],(step['label'],r.returncode)
        if r.returncode:
            assert all(n in content for n in ['running 1 test','1 failed','panicked at',step['test'],step['expected_assertion']]),step['label']
            negatives.append(dict(name=step['label'],test=step['test'],panic=step['expected_assertion'],exit=101,role=step['role'],sha256=step['program']['sha256']))
        else:
            match=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',content);assert match,step['label']
            suites[step['label']]=dict(passed=int(match[1]),ignored=int(match[2]))
        print('Executed',step['label'],r.returncode,flush=True)
    assert suites['full-debug']==suites['full-release']==dict(passed=981,ignored=3)
    assert sum(r['role']=='fresh_source_fault' for r in negatives)==10 and sum(r['role']=='original_runtime_red' for r in negatives)==1
    pcm={}
    for folder,count in [('control-pcm',216),('mp3-pcm',116),('http-mp3-pcm',105)]:
        a={p.name:p.read_bytes() for p in (work/folder/'debug').glob('*.f32le')};b={p.name:p.read_bytes() for p in (work/folder/'release').glob('*.f32le')};old={p.name:p.read_bytes() for p in (baseline/folder/'debug').glob('*.f32le')}
        assert len(a)==count and a==b==old,(folder,len(a),len(b),'previous bytes changed');pcm[folder]=dict(outputs=count,samples=sum(len(v)//4 for v in a.values()),all_dev_o3_bits_equal=True,previous_bits_equal=True)
    for label,prefix,count in [('control-pcm','MIGO_FILE_CONTROL_PCM ',131),('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36),('local-pcm','MIGO_LOCAL_FILE_PCM ',12),('file-pcm','MIGO_FILE_STREAM_PCM ',8),('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40),('indexed-large','MIGO_INDEXED_FILE_PCM ',10),('package-small','MIGO_PACKAGE_FILE_PCM ',12),('package-large','MIGO_PACKAGE_INDEXED_PCM ',10),('http-pcm','MIGO_HTTP_M4A_PCM ',40),('spool-pcm','MIGO_HTTP_SPOOL_PCM ',80),('http-control-pcm','MIGO_HTTP_CONTROL_PCM ',234),('range-control-pcm','MIGO_RANGE_CONTROL_PCM ',116),('wave-control-pcm','MIGO_WAVE_CONTROL_PCM ',131),('http-wave-pcm','MIGO_HTTP_WAVE_PCM ',138),('http-control-pcm','MIGO_HTTP_ORDINARY_PCM ',8)]:
        a=index.pcm(work/(label+'-debug.log'),prefix,count);b=index.pcm(work/(label+'-release.log'),prefix,count)
        assert a==b==index.pcm(baseline/(label+'-debug.log'),prefix,count),(label,'previous complete PCM changed')
        pcm[prefix.strip()]=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_bits_equal=True)
    a={p.name:p.read_bytes() for p in (work/'vorbis-pcm/debug').glob('*.f32le')};b={p.name:p.read_bytes() for p in (work/'vorbis-pcm/release').glob('*.f32le')};assert len(a)==81 and a==b,('Vorbis complete PCM',len(a),len(b))
    pcm['vorbis']=dict(outputs=len(a),samples=sum(len(v)//4 for v in a.values()),all_dev_o3_bits_equal=True,complete_files_reread=True)
    golden=json.loads((work/'runtime-fixture-inputs.json').read_text())['vorbis']
    for profile in ['debug','release']:
        for name,row in golden.items():assert sha(work/'actual-producer-fixtures'/profile/name)==row['sha256'],name
        text=(work/('vorbis-file-'+profile+'.log')).read_text();assert 'MIGO_LARGE_VORBIS_CONTROL samples=16809984' in text and 'MIGO_REAL_LARGE_VORBIS_SERVICE encoded=' in text
    result=dict(programs=plan['programs'],suites=suites,negatives=negatives,clippy=plan['clippy'],ogg_clippy=plan['ogg_clippy'],pcm=pcm,large_vorbis_complete_zero_pcm_checked=True,real_over_16mib_service_file_handoff=True,actual_rust_generated_fixtures_match_independent_ffmpeg=True,source_replay_complexity='O(track), original File/CPU/ACK/SRC, no new index or speed claim',source_inputs_unchanged=True)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Sealed File Vorbis controls verified',flush=True)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--phase',choices=['build','execute'],required=True);p.add_argument('--work',type=Path,required=True);p.add_argument('--target',type=Path);p.add_argument('--before',type=Path);p.add_argument('--baseline',type=Path,required=True);p.add_argument('--fixture-proof',type=Path);a=p.parse_args();work=a.work.resolve()
    if a.phase=='build':
        assert a.target and a.before and a.fixture_proof;build(work,a.target,a.before,a.baseline.resolve(),a.fixture_proof)
    else:execute(work,a.baseline.resolve())
if __name__=='__main__':main()
