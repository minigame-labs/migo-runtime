#!/usr/bin/env python3
"""The product Vorbis decoder retained padding beyond the presentation end.

Select the already verified mature Ogg/Vorbis port, remove Lewton, and prove
exact original-signal frame counts, complete PCM, quotas and malformed-input
refusal. Product and candidate must resolve one pinned codec source cohort.
Faults execute rebuilt Rust assertions; failed compilation is not rejection.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_duration_scan as duration

whole=duration.whole
bench=duration.bench
ROOT=whole.ROOT


def declarations(originals):
    path='decoder/ogg.rs';tests='decoder::ogg::tests::';exact=tests+'product_vorbis_exact_short_tail_and_cross_page_counts'
    rows=[]
    def change(name,before,after,test):
        assert originals[path].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,path,before,after,test))
    change('product-vorbis-gapless-disabled','.gapless(true)','.gapless(false)',exact)
    change('product-vorbis-pcm-zeroed','block.copy_to_slice_interleaved(&mut output.samples[start..]);',
        'output.samples[start..].fill(0.0);',tests+'product_vorbis_full_pcm_matches_frozen_mature_decoder')
    change('product-vorbis-caller-quota-disabled','.filter(|n| *n <= limit)',
        '.filter(|_| true)',tests+'product_vorbis_caller_quota_and_corruption_never_return_partial_pcm')
    change('product-vorbis-work-budget-too-small','const MAX_STEPS: usize = 131072;',
        'const MAX_STEPS: usize = 1;',exact)
    change('product-vorbis-final-packet-not-returned','PacketStep::Packet { consumed } => {',
        'PacketStep::Packet { consumed } => { if offset + consumed == data.len() { return Ok(output); }',exact)
    change('product-vorbis-consumed-prefix-lost','offset += consumed;','offset = consumed;',exact)
    change('product-vorbis-encoded-cap-disabled','data.len() > 16 * 1024 * 1024',
        'false',tests+'product_vorbis_caller_quota_and_corruption_never_return_partial_pcm')
    change('product-vorbis-source-rate-shifted','output.sample_rate = rate;',
        'output.sample_rate = rate + 1;',exact)
    return rows


def build(package,work,env,label,release=False):
    # copytree preserves old source mtimes. Force the local crate to compile
    # again even when Cargo reuses an otherwise identical probe identity.
    lib=package/'src/lib.rs'
    lib.write_text(lib.read_text())
    return whole.build_candidate(package,work,env,label,release=release)


def verify_libraries(work):
    libraries=whole.support.codecs.library_patches(ROOT,work)
    adopted=[]
    for name,materialized in libraries.items():
        product=ROOT/'engine/third_party'/materialized.name
        if not product.is_dir():continue
        left={str(p.relative_to(materialized)):whole.support.codecs.sha(p)for p in materialized.rglob('*')if p.is_file()}
        right={str(p.relative_to(product)):whole.support.codecs.sha(p)for p in product.rglob('*')if p.is_file()}
        assert left==right,('product published-source mismatch',name)
        adopted.append(dict(name=name,files=len(left)))
    assert {row["name"] for row in adopted} == {
        "symphonia-core", "symphonia-metadata", "symphonia-format-ogg",
        "symphonia-codec-vorbis", "symphonia-format-riff", "symphonia-codec-pcm"}
    (work/'adopted-library-proof.json').write_text(json.dumps(adopted,indent=2)+'\n')


def main(work,target,candidate,rubato,preflight):
    if candidate:
        package,env,frozen,libraries,resampling=bench.prepare(work,target,rubato)
        profiles=['debug']if preflight else['release','debug'];expected=430
    else:
        package,env,frozen=whole.support.clock.prepare(work,target,whole.extra_inputs())
        verify_libraries(work)
        profiles=['debug']if preflight else['release','debug'];expected=397
    originals={'decoder/ogg.rs':(package/'src/decoder/ogg.rs').read_text()}
    rows=declarations(originals);results={}
    for profile in profiles:
        label='positive-'+profile
        binary=build(package,work,env,label,release=profile=='release')
        log=work/(label+'-tests.log')
        assert whole.support.clock.common.run([str(binary)],log,package,env,timeout=300)==0,log
        assert re.search(rf'test result: ok\. {expected} passed; 0 failed; 2 ignored;',log.read_text())
        whole.support.program(work,label,'native');results[profile]=dict(passed=expected,ignored=2)
    controls=whole.support.faults(work,package,env,originals,[]if preflight or candidate else rows,
        lambda name:build(package,work,env,name),package/'src','native')
    whole.verify_inputs(work,frozen)
    assert all((package/'src'/name).read_text()==text for name,text in originals.items())
    if candidate:
        assert all(whole.support.codecs.sha(work/p)==h for p,h in json.loads((work/'retained-sources.json').read_text()).items())
    summary=dict(profiles=results,controls=controls,compiled_controls=len(controls),root_inputs_unchanged=True,
        source_copies_restored=True,candidate=candidate,preflight=preflight,product_vorbis_selected=True,
        old_lewton_removed=True,production_pool_migrated=False)
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('product Vorbis complete',results,len(controls),flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--candidate',action='store_true')
    parser.add_argument('--rubato-cargo-home',type=Path)
    parser.add_argument('--preflight',action='store_true')
    args=parser.parse_args()
    assert not args.candidate or args.rubato_cargo_home
    main(args.output.resolve(),args.target_dir.resolve(),args.candidate,args.rubato_cargo_home.resolve()if args.rubato_cargo_home else None,args.preflight)
