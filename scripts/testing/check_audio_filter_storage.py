#!/usr/bin/env python3
"""Own fixed IIR/Biquad capacities through their actual CPU destruction.

Also retain the full borrowed-IIR input ownership fault matrix. Only a fresh
executable reaching the named business assertion counts as a rejected fault.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_iir as transport

base = transport.base
ROOT = base.ROOT
PREFIX = 'audio_thread::filter_storage_tests::'


def declarations(originals):
    factory = 'factories_charge_nested_history_and_refuse_before_allocation'
    adoption = 'fixed_storage_adoption_keeps_pointer_and_refuses_foreign_ledger'
    reservation = '''        let reservation = host.map_or_else(
            || Ok(Reservation::unmanaged()),
            |host| host.reserve_value_with_bytes::<T>(bytes),
        )?;
        let data = create();'''
    early = '        let data = create();\n' + reservation.replace('\n        let data = create();', '')
    rows = [(n, k, a, z, transport.TEST, message)
            for n, k, a, z, message in transport.declarations(originals)]
    rows += [
        ('filter-capacity-omitted', 'storage', 'host.reserve_value_with_bytes::<T>(bytes)', 'host.reserve_value_with_bytes::<T>(0)', factory, 'IIR factory bypassed storage admission'),
        ('filter-allocation-first', 'storage', reservation, early, factory, 'IIR factory allocated before admission'),
        ('filter-inline-drop', 'retirement', 'if self.lease.is_some() {', 'if false {', 'generic_node_and_closed_host_refusals_preserve_owned_filter_data', 'refused node returned live filter credit'),
        ('filter-adoption-capacity-omitted', 'storage', 'self.0.prepare_with_bytes(host, self.0.capacity_bytes())', 'self.0.prepare(host)', adoption, 'fixed storage adoption omitted actual capacity'),
        ('filter-foreign-owner-accepted', 'storage', 'if !self.0.accepts_physical_ledger(host) {', 'if false {', adoption, 'fixed storage reused foreign physical ledger'),
        ('biquad-sample-rate-cache-stale', 'biquad', '            && sample_rate == self.last_sample_rate', '', 'biquad_rate_alone_invalidates_cached_coefficients', 'biquad reused coefficients from another sample rate'),
        ('biquad-context-rate-lost', 'context', 'BiquadFilterNode::new_with_retirement(\n            node_id,\n            self.channels,\n            self.sample_rate,', 'BiquadFilterNode::new_with_retirement(\n            node_id,\n            self.channels,\n            48000,', 'biquad_factory_binds_parameter_range_to_context_rate', 'biquad factory used device rate instead of context rate'),
    ]
    for name, key, before, after, _, _ in rows:
        assert before != after and originals[key].count(before) == 1, (name, 'anchor drift')
    return rows


def main(work: Path, target: Path, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths = {key: package / name for key, name in [
        ('dispatch', 'src/audio_thread_command_iir.rs'),
        ('node', 'src/nodes/iir_filter.rs'),
        ('inbox', 'src/command_inbox.rs'),
        ('channel', 'local-deps/shared/src/audio_channel.rs'),
        ('storage', 'src/node_storage.rs'),
        ('retirement', 'src/node_retirement.rs'),
        ('biquad', 'src/nodes/biquad_filter.rs'),
        ('context', 'src/context.rs'),
    ]}
    originals = {key: path.read_text() for key, path in paths.items()}
    rows = declarations(originals)
    programs, controls, suites = [], [], {}

    def build(name, release=False):
        binary = product.src.build(package, work, env, name, release)
        programs.append(dict(name=name, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        return binary

    def run(binary, label, selector='', expected=0, assertion=None):
        cmd = [str(binary)] + (['--exact', selector] if selector else []) + ['--nocapture']
        code = common.run(cmd, work / (label + '.log'), package, env, timeout=300)
        (work / (label + '-exit.txt')).write_text(str(code) + '\n')
        text = (work / (label + '.log')).read_text()
        assert code == expected, (label, code)
        if expected:
            assert all(x in text for x in ['running 1 test', 'panicked at', '1 failed', assertion]), label
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', text)
            assert match, label
            suites[label] = dict(passed=int(match[1]), ignored=int(match[2]))

    positive = build('positive-debug')
    base.capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    for test in sorted({row[4] for row in rows}):
        run(positive, 'positive-' + test.rsplit('::', 1)[-1], PREFIX + test if '::' not in test else test)
    assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        run(positive, 'full-debug')
        for name, key, before, after, test, assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before, after))
                binary = build(name)
                selector = PREFIX + test if '::' not in test else test
                run(binary, name, selector, 101, assertion)
                controls.append(dict(name=name, test=selector, exit=101, expected_assertion=assertion, sha256=programs[-1]['sha256']))
                programs[-1]['pruned'] = True
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual filter storage/input fault rejected', name, flush=True)
            finally:
                paths[key].write_text(originals[key])
        release = build('positive-release', True)
        run(release, 'full-release')
    product.whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[key] for key, path in paths.items())
    for dep in ['shared', 'io', 'services', 'frame-wire']:
        root = ROOT / 'engine/crates' / dep / 'src'
        assert all((package / 'local-deps' / dep / p.relative_to(root.parent)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    assert all((package / 'src' / p.relative_to(ROOT / 'engine/crates/audio/src')).read_bytes() == p.read_bytes() for p in (ROOT / 'engine/crates/audio/src').rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Filter storage ownership verified', len(controls), 'actual source faults', flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight)
