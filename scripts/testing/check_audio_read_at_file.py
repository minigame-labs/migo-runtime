#!/usr/bin/env python3
"""Prove bounded file geometry, mutation guards and recycling in actual Rust.

A read error alone can mask a missing extent guard: the OS may reject an
invalid offset after a payload read was attempted. Contracts also count entry
into the payload operation, and inject a real mutation between read and verify.
"""
from __future__ import annotations
import argparse,importlib.util,json,os,shutil,subprocess,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('file_io_codecs',ROOT/'scripts/bench/audio-codecs.py');codecs=importlib.util.module_from_spec(spec);spec.loader.exec_module(codecs)
def build(work,env,label):
    codecs.run(['cargo','clean','--offline','--locked','--release','-p','migo-read-at-file-probe'],work/f'{label}-clean.log',cwd=work,env=env)
    log=work/f'{label}-build.jsonl';codecs.run(['cargo','test','--offline','--locked','--release','--lib','--no-run','--message-format=json'],log,cwd=work,env=env)
    rows=[]
    for line in log.read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact'and r.get('executable')and r.get('profile',{}).get('test'):rows.append(r)
    assert len(rows)==1 and not rows[0]['fresh'];binary=work/f'{label}-tests';shutil.copy2(rows[0]['executable'],binary);return binary

def main(work,target):
    assert not work.exists();work.mkdir(parents=True);(work/'src').mkdir();(work/'source').mkdir()
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];versions={r['version']for r in locked if r['name']=='parking_lot'};assert len(versions)==1
    (work/'Cargo.toml').write_text('[package]\nname="migo-read-at-file-probe"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nshared={package="migo-shared",path='+json.dumps(str(ROOT/'engine/crates/shared'))+',default-features=false,features=["vfs"]}\nparking_lot="='+versions.pop()+'"\n[dev-dependencies]\nmigo-alloc-probe={path='+json.dumps(str(ROOT/'engine/testing/alloc-probe'))+'}\n[profile.release]\nopt-level=3\n')
    manifest=work/'Cargo.toml';manifest.write_text(manifest.read_text().replace('[dependencies]\n','[dependencies]\nmigo-io={path='+json.dumps(str(ROOT/'engine/crates/io'))+',default-features=false}\n',1))
    (work/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    path=work/'source/read_at_file.rs';shutil.copy2(ROOT/'engine/crates/io/src/read_at_file.rs',path)
    (work/'src/lib.rs').write_text('pub mod pools {pub use migo_io::pools::ByteTicket;}\n#[cfg(test)] #[global_allocator] static COUNTING:migo_alloc_probe::CountingAllocator=migo_alloc_probe::CountingAllocator::system();\n#[path='+json.dumps(str(path))+']mod read_at_file;\n')
    inputs=[Path(__file__).resolve(),ROOT/'engine/crates/io/src/read_at_file.rs',ROOT/'engine/crates/io/src/lib.rs',ROOT/'engine/Cargo.toml',ROOT/'engine/Cargo.lock',ROOT/'scripts/bench/audio-codecs.py',ROOT/'engine/crates/shared/Cargo.toml',ROOT/'engine/testing/alloc-probe/Cargo.toml']
    inputs+=[ROOT/'engine/crates/io/Cargo.toml']+list((ROOT/'engine/crates/io/src').rglob('*.rs'))
    inputs+=list((ROOT/'engine/crates/shared/src').rglob('*.rs'))+list((ROOT/'engine/testing/alloc-probe/src').rglob('*.rs'))
    frozen={str(p.relative_to(ROOT)):codecs.sha(p)for p in inputs}
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        dst=work/'source-inputs'/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,dst)
    env=os.environ.copy();env.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_TARGET_DIR=str(target),CARGO_NET_OFFLINE='true')
    for name in ['ANDROID_NDK','ANDROID_NDK_HOME']:env.pop(name,None)
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=work,env=env)
    positive=build(work,env,'positive');codecs.run([str(positive),'--nocapture','--test-threads=1'],work/'positive.log',cwd=work,env=env);assert '8 passed; 0 failed'in(work/'positive.log').read_text()
    base='read_at_file::tests::';bounds='bounded_controls_rewinds_and_two_media_slots_keep_exact_bytes'
    declarations=[
        ('pre-read-version-guard-removed','        self.verify()?;','', 'mutation_before_read_never_issues_a_payload_read'),
        ('post-read-version-guard-removed','        self.verify()\n    }','        Ok(())\n    }','mutation_after_read_and_after_last_chunk_never_become_successful_eof'),
        ('control-cap-removed','if len > cap','if false',bounds),
        ('extent-overflow-wraps','''offset
            .checked_add(len as u64)
            .is_none_or(|end| end > object_len)''','offset.wrapping_add(len as u64)>object_len',bounds),
        ('third-media-slot-allocated','state.allocated < 2','state.allocated < 3',bounds),
        ('media-buffer-not-recycled','slot.take().unwrap()','{drop(slot.take());vec![0;FILE_CHUNK_BYTES]}','recycled_media_reads_have_no_steady_rust_heap_allocation'),
        ('absolute-offset-truncated-u32','read_exact_at(&self.file, buffer, offset)?;','read_exact_at(&self.file, buffer, u64::from(offset as u32))?;','large_sparse_offsets_are_u64_and_do_not_read_the_prefix'),
    ]
    declarations.append(('media-storage-rezeroed','            *slot = Some(std::mem::take(&mut self.data));','            self.data.fill(0);\n            *slot = Some(std::mem::take(&mut self.data));','short_chunks_hide_retained_bytes_without_rezeroing_storage'))
    original=path.read_text()
    for name,before,after,test in declarations:
        assert original.count(before)==1,(name,'anchor changed');path.write_text(original.replace(before,after))
        try:
            binary=build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w')as stream:r=subprocess.run([str(binary),'--exact',base+test,'--nocapture'],cwd=work,env=env,stdout=stream,stderr=subprocess.STDOUT,timeout=30)
            observed=log.read_text();assert r.returncode==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject actual runtime assertion')
        finally:path.write_text(original)
        print('actual file IO fault',name,': executable assertion rejected',flush=True)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items());assert path.read_text()==original
    summary={'positive_tests':8,'compiled_controls':len(declarations),'binary_sha256':codecs.sha(positive),'source_restored':True,'root_inputs_unchanged':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Actual positioned regular-file IO/recycling/mutation boundaries on host; Windows/native-device/performance not accepted'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve())
