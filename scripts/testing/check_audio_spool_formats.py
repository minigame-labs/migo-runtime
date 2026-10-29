#!/usr/bin/env python3
"""Exercise one sealed HTTP source across all sequential codecs and M4A.

Sealing already performed native IO. Reader admission must not requeue that
owner or demand byte credit for a second setup. The same source must feed every
codec's existing AudioCPU/DSP/player, not a copied parser or decode loop.
"""
from __future__ import annotations
import argparse,importlib.util,json,shutil,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
local=module('spool_local',ROOT/'scripts/testing/check_audio_local_sources.py')
spool=module('all_spool',ROOT/'scripts/testing/check_audio_spool.py')
codecs=local.codecs

def main(work,target,rubato,positive_only):
    env,frozen,libraries,new=local.prepare(work,target,rubato,[Path(__file__).resolve(),Path(spool.__file__).resolve()])
    package=work/'probe';spool.include(work)
    shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe/spool_format_contracts.rs',work/'source/spool_format_contracts.rs')
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(work/"source/spool_format_contracts.rs"))}] mod spool_format_contracts;\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(l for l in(work/'candidate-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected),'existing dependency drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=local.file.dsp.source.build(work,env,'positive')
    spool.positive(work,env,binary,[('spool-formats','spool_format_contracts::',3),('sequential','local_source_contracts::',5)])
    negative=0 if positive_only else spool.controls(work,env)
    sequential=0 if positive_only else local.controls(work,env)
    anchored=0 if positive_only else local.file.controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==28
    for row in rows:assert local.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for pkg,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{pkg}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'spool_format_tests':3,'spool_format_pcm_comparisons':256,'spool_format_player_cases':16,'spool_http_tests':5,'spool_owner_tests':4,'m4a_spool_pcm_comparisons':32,'local_format_tests':5,'local_format_pcm_comparisons':256,'local_format_player_cases':16,'spool_compiled_controls':negative,'sequential_compiled_controls':sequential,'local_compiled_controls':anchored,'previous_http_and_core_controls_recounted':0,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':28,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Host common sealed HTTP source for MP3/WAVE/AAC-LC ADTS/Vorbis plus M4A indexed regression, direct sealed-owner admission and shared AudioCPU/DSP/player. Not automatic source/format/fallback selection, writable-file atomic snapshot, crash recovery/full controls/product cutover/native/device/maximum-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
