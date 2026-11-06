#!/usr/bin/env python3
"""Verify shared cache URL capacity ownership through physical CPU destruction.

Compare complete cached-player PCM with the original String-key cache. Faults
count only after fresh compilation and an exact single business assertion.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shutil

import check_audio_mix_buffer as previous

base = previous.base
wave = previous.wave
ROOT = base.ROOT
PREFIX = 'cache::key_storage::tests::'
CACHE_PCM = PREFIX + 'complete_cache_lru_replacement_and_cached_player_pcm_trace'


def cache_pcm_receipt(log):
    decoder = json.JSONDecoder()
    rows = {}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_CACHE_KEY_PCM '):
            continue
        _, key, tail = line.split(' ', 2)
        values = []
        for _ in range(3):
            value, end = decoder.raw_decode(tail)
            values.append(value)
            tail = tail[end:].lstrip()
        assert not tail and key not in rows
        channels, frames = map(int, key.split(':'))
        assert len(values[0]) == channels * frames + channels - 1
        rows[key] = dict(pcm=values[0], state=values[1], order=values[2])
    assert len(rows) == 18, (log, len(rows))
    return rows


def declarations(originals):
    rows = []

    def add(name, key, before, after, test, message):
        assert before != after and originals[key].count(before) == 1, (name, key, 'anchor drift')
        rows.append(dict(name=name, changes=[(key, before, after)], test=PREFIX + test, expected_assertion=message))

    native = 'native_arc_layout_adoption_hash_lookup_and_clones_match_actual_allocation'
    managed = 'managed_cache_charges_one_shared_key_without_copying_original_backing'
    refusal = 'key_refusal_precedes_arc_allocation_and_matches_direct_admission_cost'
    inline = 'inline_credit_returns_after_both_string_and_arc_cell_are_freed'
    bulk = 'bulk_key_clones_keep_credit_until_original_cpu_destruction_after_close'
    lifecycle = 'removal_replace_lru_clear_and_drop_release_one_shared_large_key'
    add('key-length-instead-of-capacity', 'key', '                    .capacity()\n                    .checked_add(Self::cell_bytes())', '                    .len()\n                    .checked_add(Self::cell_bytes())', managed, 'stored cache key omitted physical credit')
    add('key-arc-cell-uncharged', 'key', '.checked_add(Self::cell_bytes())', '.checked_add(0)', managed, 'stored cache key omitted physical credit')
    add('key-arc-before-admission', 'key', '        let data = match host {', '        let _early = std::hint::black_box(Arc::new(Data::Native(String::new())));\n        let data = match host {', refusal, 'cache key Arc allocated before admission')
    add('key-small-capacity-uncharged', 'key', 'host.reserve_inline(bytes)?', 'host.reserve_inline(0)?', inline, 'cache key inline capacity omitted physical credit')
    add('key-small-artificial-handoff', 'key', 'if bytes < 4096 {', 'if bytes == 0 {', inline, 'cache key inline capacity omitted physical credit')
    add('key-bulk-freed-inline', 'key', 'if bytes < 4096 {', 'if true {', bulk, 'cache key freed large string on caller')
    add('key-native-input-copied', 'key', 'None => Data::Native(text),', 'None => Data::Native(text.clone()),', native, 'cache key copied incoming string')
    add('key-managed-input-copied', 'key', 'Data::Cpu(reservation.finish(text))', 'Data::Cpu(reservation.finish(text.clone()))', managed, 'stored cache key copied original backing')
    add('key-clone-allocates', 'key', 'Self(self.0.clone())', 'Self::new(self.as_str().to_owned(), None).unwrap()', native, 'cache key clone allocated')
    add('key-borrowed-hash-mismatch', 'key', 'self.as_str().hash(state);', 'self.as_str().len().hash(state);', native, 'cache key borrowed lookup changed string hashing')
    add('key-equality-uses-owner-address', 'key', 'self.as_str() == other.as_str()', 'Arc::ptr_eq(self.0.as_ref().unwrap(), other.0.as_ref().unwrap())', native, 'cache key equality changed replacement semantics')
    add('key-arc-refund-precedes-cell-free', 'key', 'drop(Arc::into_inner(cell));', 'drop(cell);', inline, 'cache key inline refund preceded Arc deallocation')
    add('key-owned-value-caller-drop', 'retirement', 'if self.lease.is_some() {', 'if false {', bulk, 'cache key freed large string on caller')
    add('key-credit-before-cpu-drop', 'retirement', '            let lease = self.lease.take().unwrap();', '            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);', bulk, 'cache key returned physical credit before CPU drop')
    hit = '''                .expect("cache entry lost its LRU key");'''
    add('cache-hit-copies-lru-url', 'cache', hit, hit + '\n            let key = Key::new(key.as_str().to_owned(), None).unwrap();', 'hit_reuses_large_lru_key_under_full_budget_and_closed_cpu', 'cache hit cloned or re-admitted large LRU URL')
    add('cache-indexes-copy-two-urls', 'cache', 'self.order_index.insert(self.access_counter, url.clone());', 'self.order_index.insert(self.access_counter, Key::new(url.as_str().to_owned(), None).unwrap());', managed, 'cache and LRU retained duplicate URL allocations')
    start = originals['cache'].index('        let url = match Key::new(')
    end = originals['cache'].index('        self.access_counter += 1;', start)
    admission_and_eviction = originals['cache'][start:end]
    while_at = admission_and_eviction.index('        while self.current_size')
    add('cache-evicts-before-key-admission', 'cache', admission_and_eviction, admission_and_eviction[while_at:] + admission_and_eviction[:while_at], 'refused_key_admission_bypasses_cache_before_eviction_or_counter_mutation', 'cache metadata refusal evicted or published an entry')
    add('cache-clear-retains-lru-owner', 'cache', '        self.order_index.clear();', '        // fault: retain LRU key owners', lifecycle, 'cache clear retained LRU key owner')
    assert len(rows) == 18
    return rows


def legacy_cache(source):
    assert 'entries: HashMap<String, CacheEntry>' in source
    assert 'mod key_storage;' not in source
    return source + '''
// Compile new observations against the original uncharged String-key cache.
#[path = "cache_key_storage.rs"]
mod key_storage;
impl GlobalAudioCache {
    pub(crate) fn managed(cpu: migo_io::audio_cpu::AudioCpu, _storage: crate::node_retirement::Host) -> Self {
        Self::with_cpu(cpu)
    }
}
'''


def main(work, target, before_directory, legacy_source, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    for source in [Path(__file__).resolve(), legacy_source]:
        if source.is_relative_to(ROOT):
            name = str(source.relative_to(ROOT))
            frozen[name] = common.sha(source)
            saved = work / 'source-inputs' / name
            saved.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, saved)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    paths = {key: package / 'src' / name for key, name in [('cache', 'cache.rs'), ('key', 'cache_key_storage.rs'), ('retirement', 'node_retirement.rs'), ('tests', 'cache_key_storage_tests.rs'), ('actor', 'audio_thread.rs')]}
    originals = {key: path.read_text() for key, path in paths.items()}
    rows = declarations(originals)
    programs, controls, red, suites = [], [], [], {}

    def build(label, release=False):
        binary = product.src.build(package, work, env, label, release)
        programs.append(dict(name=label, path=str(binary), sha256=common.sha(binary), pruned=False))
        return binary

    def run(binary, label, selector='', expected=0, assertion=None):
        code = common.run([str(binary)] + (['--exact', selector, '--nocapture'] if selector else []), work / (label + '.log'), package, env, timeout=900)
        (work / (label + '-exit.txt')).write_text(str(code) + '\n')
        output = (work / (label + '.log')).read_text()
        assert code == expected, (label, code)
        if expected:
            assert all(value in output for value in ['running 1 test', selector, 'panicked at', '1 failed', assertion]), label
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', output)
            assert match, label
            suites[label] = dict(passed=int(match[1]), ignored=int(match[2]))

    # Stabilized source diagnostic comparison happens before formal controls.
    clippy = ['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json']
    assert common.run(clippy, work / 'clippy.jsonl', package, env, timeout=600) == 0
    before_tests = before_directory / 'engine/crates/audio/src/cache_key_storage_tests.rs'
    try:
        paths['tests'].write_bytes(before_tests.read_bytes())
        assert common.run(clippy, work / 'clippy-before.jsonl', package, env, timeout=600) == 0
    finally:
        paths['tests'].write_text(originals['tests'])
    old, current = wave.diagnostics(work / 'clippy-before.jsonl', package), wave.diagnostics(work / 'clippy.jsonl', package)
    added = current - old
    delta = dict(before=sum(old.values()), after=sum(current.values()), new_diagnostics=sum(added.values()), actor_included=True, new=[dict(key=key, count=count) for key, count in added.items()])
    (work / 'clippy-delta.json').write_text(json.dumps(delta, indent=2) + '\n')
    assert not added, delta
    print('Stable source Clippy compared:', delta, flush=True)
    positive = build('positive-debug')
    base.capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    run(positive, 'full-debug')
    traces = [('cache', CACHE_PCM, cache_pcm_receipt)]
    for name, selector, parser in [('pcm', previous.PCM, previous.pcm_receipt), ('index', previous.INDEX_PCM, previous.index_pcm_receipt), ('media', previous.MEDIA_PCM, previous.media_pcm_receipt), ('inner', previous.INNER_PCM, previous.inner_pcm_receipt), ('context', previous.CONTEXT_PCM, previous.context_pcm_receipt), ('slot', previous.SLOT_PCM, previous.slot_pcm_receipt), ('route', previous.ROUTE_PCM, previous.route_pcm_receipt), ('mix', previous.MIX_PCM, previous.mix_pcm_receipt)]:
        traces.append((name, selector, parser))
    current_traces = {}
    for name, selector, parser in traces:
        run(positive, name + '-pcm-debug', selector)
        current_traces[name] = parser(work / (name + '-pcm-debug.log'))
    if not preflight:
        original_cache = legacy_source.read_text()
        (work / 'original-cache.rs').write_text(original_cache)
        adapted = legacy_cache(original_cache)
        (work / 'original-cache-adapted.rs').write_text(adapted)
        try:
            paths['cache'].write_text(adapted)
            binary = build('original-string-cache')
            for label, test, message in [('before-key-charge', 'managed_cache_charges_one_shared_key_without_copying_original_backing', 'stored cache key omitted physical credit'), ('before-hit-copy', 'hit_reuses_large_lru_key_under_full_budget_and_closed_cpu', 'cache hit cloned or re-admitted large LRU URL')]:
                run(binary, label, PREFIX + test, 101, message)
                red.append(dict(name=label, test=PREFIX + test, exit=101, expected_assertion=message, sha256=programs[-1]['sha256']))
            for name, selector, parser in traces:
                run(binary, name + '-pcm-before', selector)
                assert parser(work / (name + '-pcm-before.log')) == current_traces[name], (name, 'original complete PCM/state/order changed')
            programs[-1]['pruned'] = True
            binary.unlink()
        finally:
            paths['cache'].write_text(originals['cache'])
        for row in rows:
            changed = set()
            try:
                for key, before, after in row['changes']:
                    assert key not in changed
                    changed.add(key)
                    paths[key].write_text(originals[key].replace(before, after))
                binary = build(row['name'])
                run(binary, row['name'], row['test'], 101, row['expected_assertion'])
                controls.append({key: value for key, value in row.items() if key != 'changes'} | dict(exit=101, sha256=programs[-1]['sha256']))
                programs[-1]['pruned'] = True
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Fresh cache key fault rejected:', row['name'], flush=True)
            finally:
                for key in changed:
                    paths[key].write_text(originals[key])
        release = build('positive-release', True)
        run(release, 'full-release')
        for name, selector, parser in traces:
            run(release, name + '-pcm-release', selector)
            assert parser(work / (name + '-pcm-release.log')) == current_traces[name], (name, 'O3 complete PCM/state/order changed')
    product.whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[key] for key, path in paths.items())
    for name in ['audio', 'shared', 'io', 'services', 'frame-wire']:
        origin = ROOT / 'engine/crates' / name / 'src'
        private = package / 'src' if name == 'audio' else package / 'local-deps' / name / 'src'
        assert all((private / path.relative_to(origin)).read_bytes() == path.read_bytes() for path in origin.rglob('*') if path.is_file()), name
    counts = {name: dict(blocks=len(data), samples=sum(len(row['pcm']) for row in data.values())) for name, data in current_traces.items()}
    (work / 'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts, blocks=sum(v['blocks'] for v in counts.values()), samples=sum(v['samples'] for v in counts.values()), all_bits_state_clock_and_order_equal=True, original_cache_body_unchanged=True, original_cache_adapter='Only module registration and factory API forwarding; original cache implementation unchanged'), indent=2) + '\n')
    (work / 'summary.json').write_text(json.dumps(dict(programs=programs, controls=controls, original_red=red, suites=suites, clippy=delta, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight), indent=2) + '\n')
    print('Cache key capacity verified:', len(controls), 'fresh faults;', suites['full-debug'], flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-directory', type=Path, required=True)
    parser.add_argument('--legacy-source', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_directory.resolve(), args.legacy_source.resolve(), args.preflight)
