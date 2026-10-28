#!/usr/bin/env python3
"""Compile real service copy admission and isolated executor quota regressions.

Copy no longer collects a whole entry, but its request still reserved 100MiB,
and synchronous paths bypassed credit. Test all four real entrypoints, actual
package scratch and cancellation ownership without exhausting the global pool.
Only executed assertion failures after fresh compilation count as controls.
"""
from __future__ import annotations
import argparse,importlib.util,json,os,re,shutil,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('copy_budget_helpers',ROOT/'scripts/testing/check_audio_cpu_admission.py')
g=importlib.util.module_from_spec(spec);spec.loader.exec_module(g)

def prepare(work,target):
    work.mkdir(parents=True,exist_ok=False);root_manifest=(ROOT/'engine/Cargo.toml').read_text()
    for name in ['shared','io','services']:
        origin=ROOT/'engine/crates'/name;dest=work/'engine/crates'/name;dest.mkdir(parents=True)
        shutil.copytree(origin/'src',dest/'src')
        if(origin/'tests/fixtures').exists():shutil.copytree(origin/'tests/fixtures',dest/'tests/fixtures')
        text=g.rebase((origin/'Cargo.toml').read_text(),origin)
        text+='\n[workspace]\nresolver="2"\n[workspace.package]\n'+g.section(root_manifest,'workspace.package')
        text+='\n[workspace.dependencies]\n'+g.rebase(g.section(root_manifest,'workspace.dependencies'),ROOT/'engine')
        text+='\n[patch.crates-io]\n'+g.rebase(g.section(root_manifest,'patch.crates-io'),ROOT/'engine')
        for local in ['shared','io','services']:text=text.replace(str(ROOT/'engine/crates'/local),str(work/'engine/crates'/local))
        (dest/'Cargo.toml').write_text(text);(dest/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    external=['contracts/runtime/domain-patterns.json','contracts/runtime/minigame-api.json','platforms/android/library/src/main/java/com/migo/runtime/ErrorCode.java']
    for name in external:
        path=work/name;path.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,path)
    package=work/'engine/crates/services';env=os.environ.copy();env.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_TARGET_DIR=str(target),CARGO_NET_OFFLINE='true',CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    runtime=work/'runtime-tmp';runtime.mkdir();env.update(TMPDIR=str(runtime),TMP=str(runtime),TEMP=str(runtime))
    for key in ['ANDROID_NDK','ANDROID_NDK_HOME']:env.pop(key,None)
    assert g.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',package,env)==0
    selected=json.loads(next(l for l in(work/'metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected)
    inputs={Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_cpu_admission.py',ROOT/'engine/Cargo.toml',ROOT/'engine/Cargo.lock'}|{ROOT/p for p in external}
    for name in ['shared','io','services']:
        origin=ROOT/'engine/crates'/name;inputs.add(origin/'Cargo.toml');inputs.update(p for p in(origin/'src').rglob('*')if p.is_file())
        if(origin/'tests/fixtures').exists():inputs.update(p for p in(origin/'tests/fixtures').rglob('*')if p.is_file())
    for row in selected:
        origin=Path(row['manifest_path']).parent
        if row.get('source')is None and origin.is_relative_to(ROOT):
            inputs.add(origin/'Cargo.toml');inputs.update(p for p in(origin/'src').rglob('*')if p.is_file())
            if(origin/'build.rs').exists():inputs.add(origin/'build.rs')
    frozen={str(p.relative_to(ROOT)):g.sha(p)for p in sorted(inputs)}
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for name in frozen:
        dest=work/'source-inputs'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,dest)
    copies={str(p.relative_to(work)):g.sha(p)for name in ['shared','io','services']for p in(work/'engine/crates'/name/'src').rglob('*')if p.is_file()}
    return env,frozen,copies

def build(work,env,name):
    package=work/'engine/crates/services'
    assert g.run(['cargo','clean','--offline','--locked','-p','migo-services','-p','migo-io','-p','migo-shared'],work/f'{name}-clean.log',package,env)==0
    assert g.run(['cargo','test','--offline','--locked','--lib','--no-run','--message-format=json'],work/f'{name}-build.jsonl',package,env)==0,(name,'compile failure is not red evidence')
    artifacts=[]
    for line in(work/f'{name}-build.jsonl').read_text().splitlines():
        try:row=json.loads(line)
        except ValueError:continue
        if row.get('reason')=='compiler-artifact'and row.get('executable')and row.get('target',{}).get('name')=='migo_services':artifacts.append(row)
    assert len(artifacts)==1 and artifacts[0]['fresh']is False
    binary=work/f'{name}-tests';shutil.copy2(artifacts[0]['executable'],binary);return binary

def controls(work,env):
    cases=[
        ('copy-charges-old-100mib','services/fs.rs','        scratch_bytes,','        scratch_bytes: shared::protocol::io_cmd::MAX_READ_LENGTH as usize,','async_copy_fits_only_bounded_scratch_and_returns_its_credit',None),
        ('copy-producer-forgets-scratch','services/fs.rs','        scratch_bytes,','        scratch_bytes: 0,','async_copy_fits_only_bounded_scratch_and_returns_its_credit',None),
        ('copy-bound-omits-encoded-buffer','shared/vfs/package/entry_reader.rs','self.max_encoded_chunk\n            .checked_add(self.max_decoded_chunk)','Some(self.max_decoded_chunk)','async_copy_fits_only_bounded_scratch_and_returns_its_credit',None),
        ('copy-bound-wraps-overflow','shared/vfs/package/entry_reader.rs','self.max_encoded_chunk\n            .checked_add(self.max_decoded_chunk)','Some(self.max_encoded_chunk.wrapping_add(self.max_decoded_chunk))','scratch_and_media_pool_totals_reject_overflow_without_allocating',None),
        ('copy-hint-ignored','io/scheduler.rs','IoRequest::Copy { scratch_bytes, .. } => *scratch_bytes as u64,','IoRequest::Copy { .. } => 0,','async_copy_fits_only_bounded_scratch_and_returns_its_credit',None),
        ('sync-copy-credit-bypassed','io/scheduler.rs','if matches!(req, IoRequest::Copy { .. }) {','if false {','sync_copy_is_admitted_on_pack_before_source_open',None),
        ('copy-route-is-inline','io/scheduler.rs','''IoRequest::Copy { backend, .. } => RouteDecision::Delegated(match backend {
            BackendKind::Pack => PoolKind::Pack,
            BackendKind::Filesystem => PoolKind::Fs,
            BackendKind::Archive => PoolKind::Archive,
        }),''','IoRequest::Copy { .. } => RouteDecision::Inline,','copy_never_uses_the_cached_read_inline_route_even_with_zero_scratch',None),
        ('copy-uses-read-descriptor','services/fs.rs','''    Ok(IoRequest::Copy {
        backend: BackendKind::Pack,
        request,
        priority: PriorityClass::from(request),
        scratch_bytes,
    })''','''    Ok(IoRequest::ReadFile {
        backend: BackendKind::Pack,
        request,
        priority: PriorityClass::from(request),
        estimated_bytes: scratch_bytes,
    })''','sync_materialization_is_admitted_on_pack_before_source_open',None),
        ('copy-credit-owned-by-cancelled-receiver','services/fs.rs','    let request = copy_request(RequestKind::Async)?;','''    let mut request = copy_request(RequestKind::Async)?;
    let bytes = match request {IoRequest::Copy {scratch_bytes,..} => scratch_bytes as u64, _ => unreachable!()};
    let _credit = scheduler.pools().reserve_bytes(bytes).map_err(|error| pool_err(error.into()))?;
    if let IoRequest::Copy {scratch_bytes,..} = &mut request {*scratch_bytes=0;}''','dropping_receiver_and_closing_domain_keep_credit_until_active_copy_finishes','pub async fn copy_pack_file_async('),
        ('media-pool-credit-omitted','io/read_at_package.rs','.checked_add(2 * FILE_CHUNK_BYTES)','.checked_add(0)','scratch_and_media_pool_totals_reject_overflow_without_allocating',None),
    ]
    for name,relative,before,after,test,scope in cases:
        crate,relative=relative.split('/',1);path=work/'engine/crates'/crate/'src'/relative;original=path.read_text();start=original.index(scope)if scope else 0;end=original.index('\n}\n',start)+3 if scope else len(original);part=original[start:end];assert part.count(before)==1,(name,'anchor drift');path.write_text(original[:start]+part.replace(before,after)+original[end:])
        try:
            binary=build(work,env,name);log=work/f'{name}-test.log';status=g.run([str(binary),'--exact','fs::copy_budget_tests::'+test,'--nocapture'],log,work/'engine/crates/services',env,timeout=30);observed=log.read_text()
            assert status==101 and 'panicked at'in observed and '1 failed'in observed and 'timed out'not in observed.lower(),(name,'must reject executed assertion, never compile/timeout',status)
        finally:path.write_text(original)
        print('actual copy admission fault',name,': executed assertion rejected',flush=True)
    return len(cases)

def main(work,target,positive_only):
    env,frozen,copies=prepare(work,target);package=work/'engine/crates/services'
    # Inspect the actual compiled normal IO artifact, not cargo metadata's union
    # of normal+dev features. The quota constructor must stay out of products.
    assert g.run(['cargo','check','--offline','--locked','--lib','--message-format=json'],work/'normal-check.jsonl',package,env)==0
    normal=[]
    for line in(work/'normal-check.jsonl').read_text().splitlines():
        try:row=json.loads(line)
        except ValueError:continue
        if row.get('reason')=='compiler-artifact'and row.get('target',{}).get('name')=='migo_io':normal.append(row)
    assert len(normal)==1 and 'admission-probe'not in normal[0]['features'],'test quota capability leaked into normal IO'
    binary=build(work,env,'positive');assert g.run([str(binary),'fs::copy_budget_tests::','--nocapture','--test-threads=1'],work/'positive.log',package,env)==0
    observed=(work/'positive.log').read_text();assert '9 passed; 0 failed'in observed and observed.count('test fs::copy_budget_tests::')==9
    count=0 if positive_only else controls(work,env)
    assert all(g.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(g.sha(work/p)==h for p,h in copies.items()),'copy not restored'
    summary={'copy_budget_tests':9,'compiled_controls':count,'positive_only':positive_only,'binary_sha256':g.sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'normal_io_features':normal[0]['features'],'probe_feature_absent_from_normal_io':True,'root_lock_sha256':g.sha(ROOT/'engine/Cargo.lock'),'scope':'Actual service async/sync copy/materialization with isolated existing executor, real package buffers, insufficient/admitted/failed/cancelled credit and closed domain. Test-only constructor via dev feature; no product dependencies or native/RSS/device/latency acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
