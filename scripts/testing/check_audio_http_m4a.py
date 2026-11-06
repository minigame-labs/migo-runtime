#!/usr/bin/env python3
"""Actual HTTP M4A Range playback, complete PCM and original CPU custody.

Build offline before executing SHA-bound retained ELFs with loopback fixtures.
No Git, network dependency lookup, spool fallback or shipping performance claim.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, shutil
from pathlib import Path
import check_audio_range_buffer as prior

index = prior.index
ROOT = index.ROOT
PREFIX = 'streaming_http::tests::'
PCM = PREFIX+'product_http_m4a_preserves_every_pcm_bit_front_tail_reorder_co64_and_edits'
LARGE = PREFIX+'range_tail_plays_beyond_whole_file_limit_without_waiting_for_original_body_eof'
CUSTODY = PREFIX+'cancelled_real_index_handoff_retains_storage_and_credit_behind_the_original_cpu'


def controls(originals):
    rows = []
    def add(name, changes, test, panic):
        for path, a, z in changes:
            assert a != z and originals[path].count(a) == 1, (name, path, 'anchor drift')
        rows.append(dict(name=name, changes=changes, test=test, expected_assertion=panic))
    add('http-index-admission-lost', [('http', '.reserve_response_bytes(length as u64)', '.reserve_response_bytes(0)')], PREFIX+'admitted_index_input_is_held_until_the_original_cpu_really_frees_it', 'HTTP index bypassed original byte admission')
    add('http-index-credit-returned-early', [('http', '_ticket: migo_io::pools::ByteTicket,', '_ticket: Option<migo_io::pools::ByteTicket>,'), ('http', '_ticket: ticket,', '_ticket: { drop(ticket); None },')], CUSTODY, 'cancelled HTTP index handoff refunded or freed on IO before original CPU drop')
    add('http-index-ticket-not-captured', [('http', 'let bytes = input;', 'let bytes = input.data;'), ('http', '&bytes.data,', '&bytes,')], CUSTODY, 'cancelled HTTP index handoff refunded or freed on IO before original CPU drop')
    add('http-index-limit-after-allocation', [('http', 'length > IndexOptions::default().max_index_bytes', 'false')], PREFIX+'oversized_tail_index_is_refused_before_the_body_or_allocation', 'oversized HTTP index did not fail before allocation/body')
    add('http-packet-outside-actual-mdat', [('http', 'if span.kind != AtomKind::MediaData\n                    || packet.offset < span.body_offset', 'if false\n                    || packet.offset < span.body_offset')], PREFIX+'indexed_packets_are_checked_against_actual_mdat_and_failure_stays_terminal', 'HTTP source accepted packet bytes outside actual mdat')
    add('http-first-media-chunk-lost', [('http', 'self.pending = Some(first);', 'drop(first); self.pending = None;')], LARGE, 'large indexed HTTP M4A failed')
    add('http-media-keeps-original-suffix', [('http', 'restart: true,', 'restart: false,')], PREFIX+'source_reposition_discards_large_unaccepted_http_suffix_and_preserves_all_pcm', 'coalesced HTTP M4A failed complete PCM')
    add('http-failed-source-recovers', [('http', '''        if self.failed {
            return Err(error("indexed HTTP source retired"));
        }
        if self.terminal {
            return Ok(SourceRead::End);
        }
        self.failed = true;
        self.interrupted = true;
        let result''', '''        if false {
            return Err(error("indexed HTTP source retired"));
        }
        if self.terminal {
            return Ok(SourceRead::End);
        }
        self.failed = true;
        self.interrupted = true;
        let result''')], PREFIX+'indexed_packets_are_checked_against_actual_mdat_and_failure_stays_terminal', 'failed indexed HTTP source resumed on EOF')
    add('http-source-burst-not-charged', [('index', '.checked_sub(1)', '.checked_sub(0)')], index.BURST, 'non-render work minted local source reposition credit')
    return rows


def build(work, target, before, baseline):
    os.environ['CARGO_INCREMENTAL'] = '0'
    base = index.base
    package, env, frozen = base.prepare(work, target)
    common = base.capacity.product.whole.support.clock.common
    for name in [index.FIXTURE, str(Path(__file__).relative_to(ROOT))]:
        frozen[name] = common.sha(ROOT/name)
        dest = work/'source-inputs'/name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT/name, dest)
    dest = work/index.FIXTURE
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT/index.FIXTURE, dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    output = package/'src/output.rs'
    saved = output.read_text()
    try:
        output.write_text(index.backend.ports(saved))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:
        output.write_text(saved)
    a=index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe')
    z=index.wave.diagnostics(work/'clippy.jsonl',package)
    assert not(z-a),(z-a)
    delta=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    assert common.run(['cargo','clippy','-p','migo-services','--lib','--tests','--offline','--locked','--message-format=json'],work/'services-clippy.jsonl',package,env,timeout=600)==0
    a=index.wave.diagnostics(baseline/'services-clippy.jsonl',baseline/'probe')
    z=index.wave.diagnostics(work/'services-clippy.jsonl',package)
    select=lambda c:type(c)({k:v for k,v in c.items() if any(n.endswith('network/http/mod.rs') for n in k[0])})
    a,z=select(a),select(z)
    assert not(z-a),(z-a)
    delta['services_http']=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0)
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    print('Stable HTTP M4A Clippy',delta,flush=True)
    programs,steps=[],[]
    def binary(name,crate='migo-audio',release=False):
        if crate=='migo-audio':
            path=base.capacity.product.src.build(package,work,env,name,release)
        else:
            (package/'local-deps/services/src/lib.rs').touch()
            cmd=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
            if release:cmd.append('--release')
            log=work/(name+'-build.jsonl')
            assert common.run(cmd,log,package,env,timeout=600)==0
            rows=[]
            for line in log.read_text().splitlines():
                try:r=json.loads(line)
                except ValueError:continue
                if r.get('reason')=='compiler-artifact' and r.get('executable') and r.get('target',{}).get('name')=='migo_services':rows.append(r)
            assert len(rows)==1 and rows[0]['fresh'] is False
            if release:assert rows[0]['profile']['opt_level']=='3'
            path=work/(name+'-tests');shutil.copy2(rows[0]['executable'],path)
        row=dict(name=name,path=str(path),sha256=common.sha(path),fresh=False)
        programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release=release)
        steps.append(dict(label='full-'+label,program=row,expected=0))
        steps.append(dict(label='http-pcm-'+label,program=row,expected=0,test=PCM))
        for tag,test,dump in [('control-pcm',prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
        row=binary('services-'+label,'migo-services',release)
        steps.append(dict(label='services-range-'+label,program=row,expected=0,test='network::http::range::',exact=False))
    paths=dict(http=package/'src/streaming_http.rs',index=package/'src/streaming_file_index.rs',dispatcher=package/'src/streaming.rs')
    originals={k:p.read_text() for k,p in paths.items()}
    fixture=package/'src/streaming_http_tests.rs'
    fixture_source=fixture.read_text()
    header_before='assert!(text.contains("accept-encoding: identity\\r\\n"));'
    header_after='assert!(text.contains("accept-encoding: identity\\r\\n") || text.contains("accept-encoding: gzip, br\\r\\n"));'
    assert fixture_source.count(header_before)==1
    # The original Sequential client negotiated gzip/br. Serve the same plain
    # identity body to it so the real old CPU codec refusal, rather than a
    # fixture header assertion, is the negative control. PCM checks stay intact.
    (work/'original-fixture-header-only-shim.json').write_text(json.dumps(dict(path='src/streaming_http_tests.rs',before=header_before,after=header_after,body_or_pcm_checks_changed=False),indent=2)+'\n')
    try:
        paths['dispatcher'].write_text((before/'engine/crates/audio/src/streaming.rs').read_text())
        fixture.write_text(fixture_source.replace(header_before,header_after))
        row=binary('original-sequential-http-dispatcher')
        steps.append(dict(label='original-sequential-http-dispatcher',program=row,expected=101,test=PCM,expected_assertion='M4A requires the indexed streaming source',role='original_runtime_red'))
    finally:
        paths['dispatcher'].write_text(originals['dispatcher'])
        fixture.write_text(fixture_source)
    for fault in controls(originals):
        try:
            for key in {c[0] for c in fault['changes']}:
                text=originals[key]
                for path,a,z in fault['changes']:
                    if path==key:text=text.replace(a,z)
                paths[key].write_text(text)
            row=binary(fault['name'])
            steps.append(dict(label=fault['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
        finally:
            for key in {c[0] for c in fault['changes']}:paths[key].write_text(originals[key])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src'
        assert all(p.read_bytes()==(private/p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n')
    print('Immutable HTTP M4A execution plan prepared',flush=True)


def execute(work, baseline):
    os.environ['MIGO_DUMP_HTTP_M4A_PCM']='1'
    prior.prior.execute(work,baseline)
    path=work/'summary.json';result=json.loads(path.read_text())
    current=index.pcm(work/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    assert current==index.pcm(baseline/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    result['pcm']['control-pcm']['previous_product_bits_unchanged']=True
    a=index.pcm(work/'http-pcm-debug.log','MIGO_HTTP_M4A_PCM ',40)
    b=index.pcm(work/'http-pcm-release.log','MIGO_HTTP_M4A_PCM ',40)
    assert a==b,'HTTP M4A dev/O3 PCM changed'
    canonical=lambda rows:{k.replace('edit-version-one/','edit-v1/'):v for k,v in rows.items()}
    assert canonical(a)==index.pcm(baseline/'indexed-pcm-debug.log','MIGO_INDEXED_FILE_PCM ',40)
    result['http_pcm']=dict(outputs=40,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_indexed_file_bits_equal=True)
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=884,ignored=3)
    assert result['suites']['services-range-debug']==result['suites']['services-range-release']==dict(passed=30,ignored=0)
    assert len(result['controls'])==9 and len(result['original_red'])==1
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('Actual HTTP M4A, complete PCM, capacity, original CPU and previous PCM verified',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--target-dir',type=Path)
    p.add_argument('--before-source',type=Path)
    p.add_argument('--baseline-native',type=Path,required=True)
    p.add_argument('--execute',action='store_true')
    a=p.parse_args()
    if a.execute:execute(a.output.resolve(),a.baseline_native.resolve())
    else:
        assert a.target_dir and a.before_source
        build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.baseline_native.resolve())
