#!/usr/bin/env python3
"""Execute shared audio CPU contracts and compiled controls in an isolated IO crate.

Root source/lock files are read and hashed; mutations only touch the retained copy.
A timeout or compile error never counts as a rejected runtime control.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rebase(text: str, directory: Path) -> str:
    return re.sub(r'path\s*=\s*"([^"]+)"', lambda m: 'path=' + json.dumps(str((directory / m[1]).resolve())), text)


def section(text: str, name: str) -> str:
    return text.split(f'[{name}]\n', 1)[1].split('\n[', 1)[0]


def run(command: list[str], log: Path, package: Path, env: dict, timeout: int = 180) -> int:
    with log.open('w') as output:
        process = subprocess.Popen(command, cwd=package, env=env, stdout=output, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            return process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait()
            raise AssertionError(f'timeout is not an accepted contract outcome: {log}')


def build(package: Path, output: Path, env: dict, name: str) -> Path:
    log = output / f'{name}-build.jsonl'
    # Cargo can reuse a same-name/path-relocated local crate whose copied file
    # mtimes predate a previous mutation artifact. Invalidate only this isolated
    # test package, then require an actually rebuilt compiler artifact.
    assert run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-io'],
               output / f'{name}-clean.log', package, env) == 0
    status = run(['cargo', 'test', '--offline', '--locked', '--lib', '--no-default-features', '--no-run',
                  '--message-format=json'], log, package, env)
    assert status == 0, (name, 'compile failed', log)
    binaries = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('target', {}).get('name') == 'migo_io':
            assert row.get('fresh') is False, 'isolated test crate was not rebuilt'
            binaries.append(Path(row['executable']))
    assert len(binaries) == 1, (name, binaries)
    binary = output / f'{name}-tests'
    shutil.copy2(binaries[0], binary)
    return binary


def main(output: Path, target: Path) -> None:
    assert not output.exists(), 'use a fresh result directory'
    output.mkdir(parents=True)
    package = output / 'probe'
    package.mkdir()
    io = ROOT / 'engine/crates/io'
    shutil.copytree(io / 'src', package / 'src')
    root_manifest = (ROOT / 'engine/Cargo.toml').read_text()
    manifest = rebase((io / 'Cargo.toml').read_text(), io)
    manifest += '\n[workspace]\n[workspace.package]\n' + section(root_manifest, 'workspace.package')
    manifest += '\n[workspace.dependencies]\n' + rebase(section(root_manifest, 'workspace.dependencies'), ROOT / 'engine')
    manifest += '\n[patch.crates-io]\n' + rebase(section(root_manifest, 'patch.crates-io'), ROOT / 'engine')
    (package / 'Cargo.toml').write_text(manifest)
    (package / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    inputs = [ROOT / 'engine/Cargo.toml', ROOT / 'engine/Cargo.lock', io / 'Cargo.toml', Path(__file__).resolve()]
    inputs += sorted((io / 'src').rglob('*.rs'))
    inputs += sorted((ROOT / 'engine/crates/shared/src').rglob('*.rs'))
    checksums = {str(p.relative_to(ROOT)): sha(p) for p in inputs}
    for path in inputs:
        destination = output / 'source-inputs' / path.relative_to(ROOT)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    (output / 'source-inputs.json').write_text(json.dumps(checksums, indent=2) + '\n')
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
               CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    for key in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        env.pop(key, None)
    # Updating the isolated package's workspace members is expected. Root lock is immutable.
    assert run(['cargo', 'metadata', '--offline', '--format-version', '1'], output / 'metadata.jsonl', package, env) == 0
    binary = build(package, output, env, 'positive')
    assert run([str(binary)], output / 'positive-test.log', package, env) == 0
    positive = (output / 'positive-test.log').read_text()
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', positive)
    assert match, 'missing complete positive suite'
    declarations = [
        ('host-limit-removed', 'pools.rs',
         'self.audio_by_host.get(&host).copied().unwrap_or(0) >= config.audio_host_limit', 'false',
         'pools::audio_admission_tests::audio_admission_bounds_both_host_and_process'),
        ('process-limit-removed', 'pools.rs', 'self.audio_in_flight >= config.audio_process_limit', 'false',
         'pools::audio_admission_tests::audio_admission_bounds_both_host_and_process'),
        ('aggregate-cpu-cap-removed', 'pools.rs',
         'pool.is_cpu_bound() && self.active_cpu_bound >= config.cpu_bound_cap', 'false',
         'pools::audio_admission_tests::aggregate_cpu_work_preserves_an_io_slot_across_classes'),
        ('queued-captures-retained', 'audio_cpu.rs', 'self.0.cancel();', '',
         'audio_cpu::tests::queued_cancellation_releases_capture_without_executing'),
        ('owner-drop-does-not-close', 'scheduler.rs',
         'impl Drop for IoScheduler {\n    fn drop(&mut self) {\n        self.close();\n    }\n}',
         'impl Drop for IoScheduler { fn drop(&mut self) {} }',
         'audio_cpu::tests::audio_handle_does_not_keep_scheduler_scope_open'),
    ]
    declarations += [
        ('typed-credit-after-publication', 'pools.rs',
         'task.execute();\n                shared.release_audio_slot(host, pool);\n                task.publish();',
         'task.execute();\n                task.publish();\n                shared.release_audio_slot(host, pool);',
         'pools::audio_admission_tests::typed_audio_task_returns_credit_before_publishing'),
        ('computed-published-too-early', 'audio_cpu.rs',
         'state.phase = Phase::Computed(result);', 'state.phase = Phase::Ready(result);',
         'audio_cpu::tests::computed_result_is_not_observable_until_publication'),
        ('late-result-leaked', 'audio_cpu.rs', 'drop(result);', 'std::mem::forget(result);',
         'audio_cpu::tests::active_cancellation_drops_the_late_owned_result'),
        ('handoff-extra-allocation', 'audio_cpu.rs',
         'let pending = Pending(Arc::new(Task {',
         'std::hint::black_box(vec![0_u8;64]);\n        let pending = Pending(Arc::new(Task {',
         'audio_cpu::tests::warmed_audio_handoff_uses_one_caller_allocation_per_step'),
    ]
    # Each wrapper must return credit before publishing, including a closed result receiver.
    pools = (package / 'src/pools.rs').read_text()
    before = 'release_shared.release_audio_slot(host, pool);\n                let _ = result_tx.send(result);'
    assert pools.count(before) == 5
    controls = []
    for name, relative, before_text, after, test in declarations:
        source = package / 'src' / relative
        original = source.read_text()
        assert original.count(before_text) == 1, (name, 'mutation anchor drift')
        controls.append((name, source, original, original.replace(before_text, after), test))
    for index in range(5):
        pieces = pools.split(before)
        changed = before.join(pieces[:index + 1]) + 'let _ = result_tx.send(result);\n                release_shared.release_audio_slot(host, pool);' + before.join(pieces[index + 1:])
        controls.append((f'credit-after-publication-{index}', package / 'src/pools.rs', pools, changed,
                         'pools::audio_admission_tests::audio_result_drop_can_reenter_admission_before_dispatch_completion'))
    for name, source, original, changed, test in controls:
        try:
            source.write_text(changed)
            mutant = build(package, output, env, name)
            log = output / f'{name}-test.log'
            status = run([str(mutant), '--exact', test, '--nocapture'], log, package, env, timeout=20)
            assert status == 101 and 'panicked at' in log.read_text(), (name, status, 'must fail an executed assertion')
        finally:
            source.write_text(original)
        print(f'compiled control {name}: rejected by runtime assertion', flush=True)
    assert all(sha(ROOT / path) == checksum for path, checksum in checksums.items()), 'Root inputs changed'
    for path in (io / 'src').rglob('*.rs'):
        assert sha(path) == sha(package / 'src' / path.relative_to(io / 'src')), 'isolated mutation was not restored'
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]), 'compiled_controls': len(controls),
               'positive_binary_sha256': sha(binary), 'root_lock_sha256': sha(ROOT / 'engine/Cargo.lock'),
               'root_inputs_unchanged': True, 'isolated_sources_restored': True,
               'scope': 'shared IO CPU admission/cancel/close/fairness contracts; no codec/device/performance acceptance'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
