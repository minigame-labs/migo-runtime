#!/usr/bin/env python3
"""Buffer EOF and duration at a block boundary required an extra render quantum.

Exercise natural completion, unbound-source terminal state, complete last-block
PCM and immediate graph collection. Recompile real source faults and restore an
optional old source verbatim; only actual single-test assertion failures count.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
NATURAL, COLLECTION = capacity.TESTS
FENCE = 'device_prepare::pull::session::resources::owner::graph::tests::natural_end_collection_is_immediate_but_notifications_wait_for_local_fences'


def main(work, target, before_source=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_source_completion.py' in frozen
    source = package / 'src/nodes/buffer_source.rs'
    original = source.read_text()
    rows = [
        ('natural-eof-deferred', 'if (!self.loop_enabled && self.position >= buffer_frames as f64)', 'if (false && self.position >= buffer_frames as f64)', NATURAL, 'source natural completion missed its final block'),
        ('duration-deferred', '|| duration_frames.is_some_and(|limit| self.elapsed_frames >= limit)', '|| false', NATURAL, 'source natural completion missed its final block'),
        ('loop-stops-at-buffer-end', 'if (!self.loop_enabled && self.position >= buffer_frames as f64)', 'if (self.position >= buffer_frames as f64)', NATURAL, 'source natural completion missed its final block'),
        ('unbound-source-never-finishes', 'None => {\n                self.state = BufferSourceState::Finished;\n                return 0;', 'None => {\n                return 0;', COLLECTION, 'unbound source did not finish on its first active block'),
        ('duration-restarted-at-every-loop', 'self.position = restart + overshoot.rem_euclid(loop_len);', 'self.position = restart + overshoot.rem_euclid(loop_len);\n                self.elapsed_frames = 0.0;', NATURAL, 'source natural completion missed its final block'),
        ('last-source-sample-dropped', 'if self.position >= wrap_frame as f64 {', 'if self.position + rate >= wrap_frame as f64 {', NATURAL, 'natural completion PCM changed'),
    ]
    for name, before, after, _, _ in rows:
        assert before != after and original.count(before) == 1, (name, 'anchor drift')
    programs, controls, red, suites = [], [], [], {}

    def record(name, binary, pruned=False):
        digest = product.whole.support.codecs.sha(binary)
        programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=pruned))
        if pruned:
            binary.unlink()
        return digest

    positive = product.src.build(package, work, env, 'positive-debug', False)
    capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    record('positive-debug', positive)
    for index, test in enumerate([NATURAL, COLLECTION, FENCE]):
        capacity.run(positive, package, env, work, f'positive-targeted-{index}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        suites['debug'] = capacity.run(positive, package, env, work, 'full-debug')
        if before_source:
            old = before_source.read_bytes()
            (work / 'before-buffer.rs').write_bytes(old)
            try:
                source.write_bytes(old)
                binary = product.src.build(package, work, env, 'before-buffer', False)
                for label, test, assertion in [('before-natural', NATURAL, 'source natural completion missed its final block'), ('before-unbound', COLLECTION, 'unbound source did not finish on its first active block'), ('before-fence', FENCE, 'natural source collection missed its final SRC input block')]:
                    red.append(capacity.run(binary, package, env, work, label, test, 101, assertion))
                record('before-buffer', binary, True)
            finally:
                source.write_text(original)
        for name, before, after, test, assertion in rows:
            try:
                source.write_text(original.replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                item = capacity.run(binary, package, env, work, name, test, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                controls.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual source completion fault rejected', name, flush=True)
            finally:
                source.write_text(original)
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Source completion verified', len(controls), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
