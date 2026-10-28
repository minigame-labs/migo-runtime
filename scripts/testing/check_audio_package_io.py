#!/usr/bin/env python3
"""Prove retained package payload credit and pooled media with actual IO code.

Job byte credit ends before publication. Package scratch/media outlive a job,
so they need a retained existing ByteTicket through the final owned chunk.
Faults mutate only an isolated IO crate; compile failure is never red evidence.
"""
from __future__ import annotations
import argparse,importlib.util,json,os,re,shutil
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('package_cpu_io',ROOT/'scripts/testing/check_audio_cpu_admission.py');g=importlib.util.module_from_spec(spec);spec.loader.exec_module(g)
def prepare(work,target,extra_inputs=()):
    assert not work.exists();work.mkdir(parents=True);package=work/'probe';package.mkdir();original=ROOT/'engine/crates/io'
    shutil.copytree(original/'src',package/'src');shutil.copytree(original/'tests/fixtures',package/'tests/fixtures')
    root_manifest=(ROOT/'engine/Cargo.toml').read_text();manifest=g.rebase((original/'Cargo.toml').read_text(),original)
    manifest+='\n[workspace]\n[workspace.package]\n'+g.section(root_manifest,'workspace.package')
    manifest+='\n[workspace.dependencies]\n'+g.rebase(g.section(root_manifest,'workspace.dependencies'),ROOT/'engine')
    manifest+='\n[patch.crates-io]\n'+g.rebase(g.section(root_manifest,'patch.crates-io'),ROOT/'engine')
    (package/'Cargo.toml').write_text(manifest);(package/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    inputs=[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_cpu_admission.py',ROOT/'engine/Cargo.toml',ROOT/'engine/Cargo.lock',original/'Cargo.toml',ROOT/'engine/crates/shared/Cargo.toml']
    inputs+=[p for tree in [original/'src',original/'tests/fixtures',ROOT/'engine/crates/shared/src']for p in tree.rglob('*')if p.is_file()]
    inputs.extend(extra_inputs)
    frozen={str(p.relative_to(ROOT)):g.sha(p)for p in inputs};(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        to=work/'source-inputs'/rel;to.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,to)
    retained={str(p.relative_to(package)):g.sha(p)for p in(package/'src').rglob('*')if p.is_file()}
    env=os.environ.copy();env.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_TARGET_DIR=str(target),CARGO_NET_OFFLINE='true',CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    for k in ['ANDROID_NDK','ANDROID_NDK_HOME']:env.pop(k,None)
    assert g.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',package,env)==0
    return env,frozen,retained

def main(work,target):
    env,frozen,retained=prepare(work,target)
    package=work/'probe'
    binary=g.build(package,work,env,'positive');assert g.run([str(binary),'--test-threads=1'],work/'positive.log',package,env)==0
    positive=(work/'positive.log').read_text();match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',positive);assert match;assert positive.count('test read_at_package::tests::')==5
    before='''        let lease = pools
            .reserve_bytes(Self::reservation_bytes(limits)?)
            .map_err(io::Error::other)?;
        let source = pin.open_reader(limits)?;'''
    declarations=[
        ('package-resident-credit-discarded','read_at_package.rs','pool: ChunkPool::new(Some(lease)),','pool: {drop(lease);ChunkPool::new(None)},','persistent_credit_lasts_until_the_final_media_chunk_is_dropped'),
        ('package-opens-before-resident-admission','read_at_package.rs',before,'''        let source = pin.open_reader(limits)?;
        let lease = pools.reserve_bytes(Self::reservation_bytes(limits)?).map_err(io::Error::other)?;''','exhausted_budget_rejects_before_opening_or_allocating_package_scratch'),
        ('retained-chunk-loses-resident-credit','read_at_file.rs','pool: self.0.clone(),','pool: Arc::new(Buffers {state: Mutex::new(BufferState {slots:[None,None],allocated:2}),_lease:None}),','persistent_credit_lasts_until_the_final_media_chunk_is_dropped'),
        ('package-pooled-read-allocates','read_at_package.rs','    pub fn chunk_at(&self, offset: u64, len: usize) -> io::Result<FileChunk> {','    pub fn chunk_at(&self, offset: u64, len: usize) -> io::Result<FileChunk> {\n        std::hint::black_box(vec![0_u8;64]);','package_chunk_pool_has_no_steady_rust_heap_allocation'),
    ]
    for name,file,before,after,test in declarations:
        p=package/'src'/file;text=p.read_text();assert text.count(before)==1,(name,'anchor drift');p.write_text(text.replace(before,after))
        try:
            mutant=g.build(package,work,env,name);log=work/f'{name}-test.log';status=g.run([str(mutant),'--exact','read_at_package::tests::'+test,'--nocapture'],log,package,env,timeout=30);observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion')
        finally:p.write_text(text)
        print('actual package IO fault',name,': executed assertion rejected',flush=True)
    assert all(g.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed';assert all(g.sha(package/p)==h for p,h in retained.items()),'source copy not restored'
    summary={'passed':int(match[1]),'ignored':int(match[2]),'package_io_tests':5,'compiled_controls':len(declarations),'binary_sha256':g.sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':g.sha(ROOT/'engine/Cargo.lock'),'scope':'Host persistent Rust payload reservation through final package media chunk, pooled IO and failure; no native allocator/device/performance acceptance'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve())
