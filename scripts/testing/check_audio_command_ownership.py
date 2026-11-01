#!/usr/bin/env python3
"""Dequeue/receiver reset returned quota before cancelled command payload drop.

Keep original admission through lazy handoff, processing and bounded CPU cleanup;
prove FIFO, immediate response cancellation, receiver fencing and real destructor
ordering. Compile the exact core service bodies in a host-audio-only test facet.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import tomllib
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
FIFO = 'audio_thread::tests::the_startup_backlog_runs_before_anything_still_in_the_channel'
CLEAN = 'audio_thread::tests::release_all_barrier_discards_old_backlog_and_full_queue_before_ack'
SHARED = 'audio_channel::tests::shrinking_after_receiver_drop_preserves_live_reservation_accounting'
CORE = 'engine/crates/core/src/services/audio.rs'
TEST_ASSETS = [
    'contracts/runtime/domain-patterns.json',
    'contracts/runtime/minigame-api.json',
    'platforms/android/library/src/main/java/com/migo/runtime/ErrorCode.java',
]


def isolate_dependencies(package):
    origins = {ROOT / 'engine/crates' / name: package / 'local-deps' / name for name in ['shared', 'io', 'services', 'frame-wire']}
    def render(text, folder):
        def replace(match):
            source = (folder / match[1]).resolve()
            destination = origins.get(source, source)
            return 'path = ' + json.dumps(str(destination))
        return re.sub(r'path\s*=\s*"([^"]+)"', replace, text)
    for source, destination in origins.items():
        shutil.copytree(source, destination)
        manifest = destination / 'Cargo.toml'
        manifest.write_text(render(manifest.read_text(), source))
        for rust in source.rglob('*.rs'):
            # copy2 preserves old mtimes. Cargo can share artifact keys across
            # these private workspaces, including a preceding mutant's output.
            # Mark the exact-byte copies newer before any dependency build.
            (destination / rust.relative_to(source)).touch()
            for name in re.findall(r'include_(?:str|bytes)!\(\s*"([^"]+)"', rust.read_text()):
                asset=(rust.parent/name).resolve()
                target=((destination/rust.relative_to(source)).parent/name).resolve()
                if asset.is_file() and not target.exists():
                    assert target.is_relative_to(package.parent), target
                    target.parent.mkdir(parents=True,exist_ok=True)
                    shutil.copy2(asset,target)

    manifest = package / 'Cargo.toml'
    manifest.write_text(render(manifest.read_text(), package))
    # Shared's integration contracts read these at runtime relative to its
    # manifest, rather than through include_str!/include_bytes!.
    for name in TEST_ASSETS:
        target = package.parent / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, target)


def prepare(work, target):
    package, env, frozen = capacity.prepare(work, target)
    isolate_dependencies(package)
    # Copied workspace members expose their existing dev-dependency edges.
    # Materialize only this private lock, then pin every subsequent build.
    assert capacity.product.whole.support.clock.common.run(
        ['cargo', 'check', '--lib', '--offline', '--message-format=json'],
        work/'local-dependencies-check.jsonl', package, env, timeout=300) == 0
    rebuilt = {}
    for line in (work/'local-dependencies-check.jsonl').read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason') == 'compiler-artifact' and '/local-deps/' in row.get('target', {}).get('src_path', ''):
            rebuilt[row['target']['name']] = row['fresh']
    assert len(rebuilt) == 4 and not any(rebuilt.values()), rebuilt
    (work/'local-dependency-freshness.json').write_text(json.dumps(rebuilt,indent=2)+'\n')
    root_lock=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())
    selected=tomllib.loads((package/'Cargo.lock').read_text())
    identities=lambda data:{(p['name'],p['version'],p.get('source'),p.get('checksum')) for p in data['package']}
    assert identities(selected)<=identities(root_lock), 'private lock changed a package identity'
    (work/'local-lock-policy.json').write_text(json.dumps(dict(root_lock_unchanged=True,selected_packages=len(selected['package']),all_identities_from_root=True),indent=2)+'\n')
    for name in TEST_ASSETS:
        frozen[name]=capacity.product.whole.support.codecs.sha(ROOT/name);saved=work/'source-inputs'/name;saved.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,saved)
    frozen[CORE] = capacity.product.whole.support.codecs.sha(ROOT / CORE)
    saved = work / 'source-inputs' / CORE
    saved.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / CORE, saved)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    return package, env, frozen


def build_shared(package, work, env, name, release=False):
    common = capacity.product.whole.support.clock.common
    # Different isolated workspaces can reuse Cargo's shared artifact key.
    # Force an actual rustc invocation without changing the copied source bytes.
    (package / 'local-deps/shared/src/lib.rs').touch()
    command = ['cargo','test','-p','migo-shared','--lib','--no-default-features','--features','vfs,codec','--offline','--locked','--no-run','--message-format=json']
    if release: command.append('--release')
    log = work / (name + '-build.jsonl')
    assert common.run(command, log, package, env, timeout=300) == 0, log
    rows = []
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason')=='compiler-artifact' and row.get('executable') and row.get('target',{}).get('name')=='shared': rows.append(row)
    assert len(rows)==1 and rows[0]['fresh'] is False, log
    binary = work / (name + '-tests')
    shutil.copy2(rows[0]['executable'], binary)
    return binary


def run_shared(binary, package, env, work, name, test=None, expected=0, assertion=None):
    if test: return capacity.run(binary,package,env,work,name,test,expected,assertion)
    log = work / (name + '.log')
    status = capacity.product.whole.support.clock.common.run([str(binary)],log,package,env,timeout=300)
    (work / (name+'-exit.txt')).write_text(str(status)+'\n')
    assert status==0 and 'test result: ok. 672 passed; 0 failed; 2 ignored;' in log.read_text(), log
    return dict(label=name,passed=672,ignored=2,exit=0)


def legacy_channel(raw, current):
    assert 'pub struct AudioCommandDelivery' not in raw
    start=current.index('/// One admitted command.')
    types=current[start:current.index('pub struct AudioCommandSender',start)]
    start=raw.index('impl Drop for AudioCommandReceiver')
    start=raw.index('    fn drop(&mut self)',start)
    brace=raw.index('{',start); depth=1; end=brace+1
    while depth:
        depth += (raw[end]=='{')-(raw[end]=='}');end+=1
    close=raw[start:end].replace('fn drop(&mut self)','pub fn close(&self)',1)
    shim='''
impl AudioCommandReceiver {
    pub fn try_recv_owned(&self) -> Result<AudioCommandDelivery, crossbeam_channel::TryRecvError> {
        self.try_recv().map(AudioCommandDelivery::unmanaged)
    }
    pub fn discard_data_queue_with(&self, mut discard: impl FnMut(AudioCommandDelivery)) {
        while let Ok(entry)=self.data.try_recv() { discard(AudioCommandDelivery::unmanaged(entry.into_command())); }
    }
    pub fn data_capacity(&self) -> usize { self.usage.max_items }
    pub fn owns_delivery(&self, _: &AudioCommandDelivery) -> bool { true }
'''+close+'\n}\n'
    result=raw+'\n'+types+'\n'+shim
    assert result.startswith(raw)
    return result


def core_facet(package, work):
    original=(ROOT/CORE).read_text()
    transformed='use crate as audio;\n'+original.replace('feature = "host-audio"','test').replace('feature = "embedded-v8"','any()')
    (work/'core-service-original.rs').write_text(original)
    (work/'core-service-facet.rs').write_text(transformed)
    (package/'src/core_service_audio_probe.rs').write_text(transformed)
    source=package/'src/lib.rs';raw=source.read_text();source.write_text(raw+'\n#[cfg(test)]\n#[path = "core_service_audio_probe.rs"]\nmod core_service_audio_probe;\n')
    return source,raw


def main(work,target,before_source=None,preflight=False):
    product=capacity.product
    package,env,frozen=prepare(work,target)
    paths={'audio_thread.rs':package/'src/audio_thread.rs','command_inbox.rs':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={name:path.read_text() for name,path in paths.items()}
    reset='        self.cleanup_state.disconnect_active();'
    origin='        if !std::sync::Arc::ptr_eq(&self.usage, &permit.usage) {'
    rows=[
        ('dequeue-returns-credit','channel','permit: Some(self._permit),','permit: None,',FIFO,'owned dequeue returned item credit before processing','audio'),
        ('bulk-discarded-inline','channel','if self.permit.is_none() || bytes < 4096 {','if true {',CLEAN,'cancelled command payload escaped CPU retirement','audio'),
        ('cancelled-drop-on-async','command_inbox.rs','cpu.retirement().retire(batch).await;','drop(batch);',CLEAN,'cancelled command payload escaped CPU retirement','audio'),
        ('restart-bypasses-owned-discard','command_inbox.rs','        self.receiver\n            .discard_data_queue_with(|delivery| self.retirement.discard(delivery));','        self.receiver.discard_data_queue();',CLEAN,'cancelled command payload escaped CPU retirement','audio'),
        ('receiver-close-resets-live-credit','channel',reset,reset+'\n        self.usage.items.store(0,std::sync::atomic::Ordering::Release);\n        self.usage.bytes.store(0,std::sync::atomic::Ordering::Release);',SHARED,'receiver close returned live command credit','shared'),
        ('discard-credit-before-data','channel','    _payload: Box<dyn Send>,\n    _permit: Option<AudioCommandPermit>,','    _permit: Option<AudioCommandPermit>,\n    _payload: Box<dyn Send>,',SHARED,'discarded payload returned credit before actual destructor','shared'),
        ('foreign-ticket-accepted','channel',origin,'        if false {',SHARED,'foreign reservation crossed transport','shared'),
    ]
    for name,file,before,after,*_ in rows:assert before!=after and originals[file].count(before)==1,(name,'anchor drift')
    programs=[];controls=[];red=[];suites={}
    def record(name,binary,pruned=False,kind='audio'):
        digest=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=digest,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return digest
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    for n,test in enumerate([FIFO,CLEAN]):capacity.run(positive,package,env,work,f'positive-targeted-{n}',test)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    shared=build_shared(package,work,env,'shared-debug');run_shared(shared,package,env,work,'shared-targeted',SHARED)
    source,raw=core_facet(package,work)
    try:
        core=product.src.build(package,work,env,'core-facet-debug',False)
        log=work/'core-facet-debug.log';code=product.whole.support.clock.common.run([str(core),'core_service_audio_probe::tests::','--nocapture'],log,package,env,timeout=120);assert code==0 and '6 passed; 0 failed;' in log.read_text(),log
        (work/'core-facet-debug-exit.txt').write_text(str(code)+'\n');record('core-facet-debug',core,True)
    finally:source.write_text(raw)
    if preflight:
        record('shared-debug',shared,True,'shared')
    else:
        suites['debug']=capacity.run(positive,package,env,work,'full-debug')
        suites['shared-debug']=run_shared(shared,package,env,work,'full-shared-debug');record('shared-debug',shared,True,'shared')
        if before_source:
            old=before_source.read_text();adapted=legacy_channel(old,originals['channel']);(work/'before-channel-original.rs').write_text(old);(work/'before-channel-adapted.rs').write_text(adapted)
            try:
                paths['channel'].write_text(adapted);binary=product.src.build(package,work,env,'before-channel',False)
                red.append(capacity.run(binary,package,env,work,'before-owned-dequeue',FIFO,101,'owned dequeue returned item credit before processing'));record('before-channel',binary,True)
            finally:paths['channel'].write_text(originals['channel'])
        for name,file,before,after,test,assertion,kind in rows:
            try:
                paths[file].write_text(originals[file].replace(before,after));binary=build_shared(package,work,env,name) if kind=='shared' else product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,test,101,assertion);item['binary_sha256']=record(name,binary,True,'shared' if kind=='shared' else 'audio');controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual command ownership fault rejected',name,flush=True)
            finally:paths[file].write_text(originals[file])
        release=product.src.build(package,work,env,'positive-release',True);record('positive-release',release);suites['release']=capacity.run(release,package,env,work,'full-release')
        shared=build_shared(package,work,env,'shared-release',True);suites['shared-release']=run_shared(shared,package,env,work,'full-shared-release');record('shared-release',shared,True,'shared')
        source,raw=core_facet(package,work)
        try:
            core=product.src.build(package,work,env,'core-facet-release',True);log=work/'core-facet-release.log';code=product.whole.support.clock.common.run([str(core),'core_service_audio_probe::tests::','--nocapture'],log,package,env,timeout=120);assert code==0 and '6 passed; 0 failed;' in log.read_text(),log;(work/'core-facet-release-exit.txt').write_text(str(code)+'\n');record('core-facet-release',core,True)
        finally:source.write_text(raw)
    product.whole.verify_inputs(work,frozen)
    assert all((package/'src'/p.relative_to(ROOT/'engine/crates/audio/src')).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates/audio/src').rglob('*') if p.is_file())
    for name in ['shared','io','services','frame-wire']:
        assert all((package/'local-deps'/name/p.relative_to(ROOT/'engine/crates'/name)).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates'/name/'src').rglob('*') if p.is_file())
    result=dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight,legacy_adaptation='New API bridge delegates to original raw receive/drop methods; original source bytes remain an exact prefix.',core_facet='Only host-audio/embedded-v8 cfg selectors and crate alias adapted; all service and test bodies unchanged')
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Command ownership verified',len(controls),'faults,',len(red),'old-code red checks,',len(suites),'full suites',flush=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-source',type=Path);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_source,args.preflight)
