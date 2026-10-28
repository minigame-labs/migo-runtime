#!/usr/bin/env python3
"""Guard InnerAudio's decimal-rate clock with executed, rebuilt fault controls.

The old 16.16 step lost time at 1.1x despite preserving callback partitions.
Use independent media-time, loop-waveform and Ended assertions, and count real
heap activity. Mutate only an isolated copy of the actual audio crate. Neither
compile failures nor timeouts count as evidence of a rejected runtime fault.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import tomllib

import check_audio_cpu_admission as common

ROOT = common.ROOT


def build(package: Path, output: Path, env: dict, name: str) -> Path:
    assert common.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-audio'],
                      output / f'{name}-clean.log', package, env) == 0
    log = output / f'{name}-build.jsonl'
    assert common.run(['cargo', 'test', '--lib', '--offline', '--locked', '--no-run',
                       '--message-format=json'], log, package, env) == 0, ('compile failed', log)
    binaries = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if (row.get('reason') == 'compiler-artifact' and row.get('executable')
                and row.get('target', {}).get('name') == 'audio'):
            assert row.get('fresh') is False, 'audio test artifact was not rebuilt'
            binaries.append(Path(row['executable']))
    assert len(binaries) == 1, binaries
    binary = output / f'{name}-tests'
    shutil.copy2(binaries[0], binary)
    return binary


def controls() -> list[tuple[str, str, str, str]]:
    return [
        ('rate-rounded-down', '(clamped * PLAYBACK_RATE_SCALE as f32).round() as u32',
         '(clamped * PLAYBACK_RATE_SCALE as f32) as u32',
         'playback_rate_preserves_every_supported_thousandth'),
        ('nan-becomes-zero-step', '        if rate.is_nan() {\n            return;\n        }\n', '',
         'nan_rate_retains_the_previous_valid_playback_speed'),
        ('fractional-rate-discarded',
         'let step_phase = (rate_units % PLAYBACK_RATE_SCALE) as u16;',
         'let step_phase = 0_u16;', 'decimal_rate_tracks_exact_time_over_many_callbacks'),
        ('phase-carry-does-not-advance-time',
         'phase -= PLAYBACK_RATE_SCALE as u16;\n                source_frame += 1;',
         'phase -= PLAYBACK_RATE_SCALE as u16;',
         'decimal_rate_tracks_exact_time_over_many_callbacks'),
        ('callback-loses-phase', 'let mut phase = self.position_phase;', 'let mut phase = 0_u16;',
         'rate_changes_and_pause_preserve_the_fractional_media_clock'),
        ('loop-drops-overshoot',
         'source_frame %= total_frames;\n                    src_frame = source_frame;',
         'source_frame = 0;\n                    src_frame = source_frame;',
         'short_decimal_loops_match_an_absolute_clock_across_partitions'),
        ('loop-discards-fraction',
         'source_frame %= total_frames;\n                    src_frame = source_frame;',
         'source_frame %= total_frames;\n                    phase = 0;\n                    src_frame = source_frame;',
         'short_decimal_loops_match_an_absolute_clock_across_partitions'),
        ('ended-delayed-one-output-frame',
         'if !waiting_for_input && !loop_enabled && source_frame >= total_frames {',
         'if !waiting_for_input && !loop_enabled && source_frame > total_frames {',
         'decimal_rate_ends_on_the_exact_final_output_frame'),
        ('underrun-loses-phase',
         '                    self.position_phase = phase;',
         '                    self.position_phase = 0;',
         'decimal_stream_stalls_preserve_phase_and_the_additive_mix'),
        ('underrun-clears-other-players',
         '                    self.accumulate_time_update(frame_idx);',
         '                    output[frame_idx * output_channels..].fill(0.0);\n'
         '                    self.accumulate_time_update(frame_idx);',
         'decimal_stream_stalls_preserve_phase_and_the_additive_mix'),
        ('seek-retains-old-phase',
         '                .min(self.source.len());\n            self.position_phase = 0;',
         '                .min(self.source.len());',
         'buffered_seek_and_stop_reset_the_fractional_media_clock'),
        ('stop-retains-old-phase',
         '        self.position_phase = 0;\n        self.shared.set_position_frames(0);\n'
         '        self.frames_since_time_update = 0;\n    }\n\n    /// Drain pending events',
         '        self.shared.set_position_frames(0);\n'
         '        self.frames_since_time_update = 0;\n    }\n\n    /// Drain pending events',
         'buffered_seek_and_stop_reset_the_fractional_media_clock'),
        ('mix-allocates-on-every-callback',
         '    pub fn process(&mut self, output: &mut [f32]) -> bool {',
         '    pub fn process(&mut self, output: &mut [f32]) -> bool {\n'
         '        std::hint::black_box(vec![0_u8; 64]);',
         'decimal_rate_mixing_has_no_steady_state_heap_activity'),
    ]


def prepare(output: Path, target: Path, extra_inputs=()):
    assert not output.exists(), 'use a new evidence directory'
    output.mkdir(parents=True)
    audio = ROOT / 'engine/crates/audio'
    package = output / 'probe'
    package.mkdir()
    shutil.copytree(audio / 'src', package / 'src')
    shutil.copy2(audio / 'build.rs', package / 'build.rs')
    workspace = (ROOT / 'engine/Cargo.toml').read_text()
    manifest = common.rebase((audio / 'Cargo.toml').read_text(), audio)
    manifest += '\n[workspace]\n[workspace.package]\n' + common.section(workspace, 'workspace.package')
    manifest += '\n[workspace.dependencies]\n' + common.rebase(
        common.section(workspace, 'workspace.dependencies'), ROOT / 'engine')
    manifest += '\n[patch.crates-io]\n' + common.rebase(
        common.section(workspace, 'patch.crates-io'), ROOT / 'engine')
    (package / 'Cargo.toml').write_text(manifest)
    (package / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    inputs = {ROOT / 'engine/Cargo.toml', ROOT / 'engine/Cargo.lock', Path(__file__).resolve(),
              Path(common.__file__).resolve(), *extra_inputs}
    for name in ['audio', 'io', 'services', 'shared']:
        crate = ROOT / 'engine/crates' / name
        inputs.update(crate.rglob('*.rs'))
        inputs.add(crate / 'Cargo.toml')
    for name in ['alloc-probe', 'executor-probe']:
        crate = ROOT / 'engine/testing' / name
        inputs.update(crate.rglob('*.rs'))
        inputs.add(crate / 'Cargo.toml')
    frozen = {str(p.relative_to(ROOT)): common.sha(p) for p in sorted(inputs)}
    for path in inputs:
        destination = output / 'source-inputs' / path.relative_to(ROOT)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    (output / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
               CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    for key in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        env.pop(key, None)
    assert common.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
                      output / 'metadata.jsonl', package, env) == 0
    locked = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    selected = tomllib.loads((package / 'Cargo.lock').read_text())['package']
    key = lambda row: (row['name'], row['version'], row.get('source'), row.get('checksum'))
    assert set(map(key, selected)) <= set(map(key, locked)), 'existing dependency version/source drift'
    assert common.run(['cargo', 'check', '--lib', '--offline', '--locked'],
                      output / 'normal-library-check.log', package, env) == 0
    return package, env, frozen


def main(output: Path, target: Path) -> None:
    package, env, frozen = prepare(output, target)
    audio = ROOT / 'engine/crates/audio'
    source = package / 'src/inner_audio.rs'
    original = source.read_text()
    for name, before, _, _ in controls():
        assert original.count(before) == 1, (name, 'mutation anchor drift')
    positive = build(package, output, env, 'positive')
    assert common.run([str(positive)], output / 'positive-tests.log', package, env) == 0
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',
                      (output / 'positive-tests.log').read_text())
    assert match, 'missing complete positive test suite'
    receipts = []
    for name, before, after, test in controls():
        try:
            source.write_text(original.replace(before, after))
            binary = build(package, output, env, name)
            log = output / f'{name}-test.log'
            status = common.run([str(binary), '--exact', f'inner_audio::tests::{test}', '--nocapture'],
                                log, package, env, timeout=20)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, status, 'must reject an executed assertion')
            receipts.append({'name': name, 'test': test, 'exit': status,
                             'binary_sha256': common.sha(binary)})
        finally:
            source.write_text(original)
        print(f'compiled clock control {name}: runtime assertion rejected', flush=True)
    assert all(common.sha(ROOT / path) == value for path, value in frozen.items()), 'Root input changed'
    assert all(path.read_bytes() == (package / 'src' / path.relative_to(audio / 'src')).read_bytes()
               for path in (audio / 'src').rglob('*') if path.is_file()), 'copy was not restored'
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]),
               'compiled_controls': len(receipts), 'controls': receipts,
               'positive_binary_sha256': common.sha(positive), 'root_inputs_unchanged': True,
               'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
               'root_lock_sha256': common.sha(ROOT / 'engine/Cargo.lock'),
               'scope': 'InnerAudio exact decimal media clock and allocation contracts; '
                        'not band-limited rate DSP, outside-window seek, device or peak-performance acceptance'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
