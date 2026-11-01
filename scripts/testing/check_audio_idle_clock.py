#!/usr/bin/env python3
"""An idle wall getter hid a stalled render clock and lost pause/resume time.

Exercise the same production wall policy with a controlled origin and bank.
Rendering must rebase once, advance on the sample grid, and preserve the later
wall/render epoch across suspension. Only fresh exact assertions reject faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'context::clock_tests::'


def declarations(original):
    rows = []
    def change(name, before, after, test, assertion, count=1):
        assert original.count(before) == count, (name, 'mutation anchor drift')
        rows.append((name, before, after, PREFIX + test, assertion))
    blocks = 'consecutive_blocks_after_idle_advance_the_graph_sample_clock'
    aligned = 'wall_epoch_is_aligned_once_and_never_rounded_through_the_rendered_clock'
    suspend = 'suspension_banks_rendered_time_ahead_of_the_wall'
    background = 'background_pause_banks_the_render_lead_and_resumes_without_a_clock_gap'
    sync = original[original.index('    fn synchronize_render_clock('):original.index('    fn bank_wall_clock(')]
    pause = original[original.index('    pub fn pause_clock_for_background('):original.index('    pub fn resume_clock_after_background(')]
    background_resume = original[original.index('    pub fn resume_clock_after_background('):original.index('    pub fn suspend(')]
    suspension = original[original.index('    pub fn suspend('):original.index('    pub fn resume(')]
    resume = original[original.index('    pub fn resume('):original.index('    fn advance_rendered_frames_for_test(')]
    change('idle-render-rebase-skipped', '        self.synchronize_render_clock();', '', blocks,
           'post-idle graph time did not advance by its rendered quantum')
    change('idle-wall-rounded-to-earlier-sample', '.ceil() as u64;', '.floor() as u64;', aligned,
           'idle clock rebase accumulated per-block rounding')
    change('idle-wall-reanchored-every-block', sync,
           sync.replace('&& wall > self.rendered_time()', '').replace('self.frames_processed.max(frame)', 'frame'), aligned,
           'idle clock rebase accumulated per-block rounding')
    bank = 'self.wall_banked = wall.max(self.rendered_time());'
    change('idle-pause-discards-render-lead', bank, 'self.wall_banked = wall;', suspend,
           'suspension discarded the rendered clock lead')
    change('idle-pause-discards-wall-lead', bank, 'self.wall_banked = self.rendered_time();', suspend,
           'suspension discarded the idle wall epoch')
    change('idle-suspend-skips-bank', suspension, suspension.replace('        self.bank_wall_clock();\n', ''), suspend,
           'suspension discarded the rendered clock lead')
    change('idle-background-skips-bank', pause, pause.replace('        self.bank_wall_clock();\n', ''), background,
           'background pause did not bank the render lead')
    change('idle-suspended-wall-keeps-ticking', 'self.state == AudioContextState::Running && !self.clock_paused', '!self.clock_paused', suspend,
           'suspended wall time advanced')
    change('idle-background-does-not-freeze', pause, pause.replace('        self.clock_paused = true;\n', ''), background,
           'background pause lost the rendered clock lead')
    change('idle-resume-keeps-old-epoch', resume, resume.replace('            self.wall_origin = std::time::Instant::now();\n', ''), suspend,
           'resume retained the paused wall epoch')
    change('idle-background-resume-keeps-old-epoch', background_resume,
           background_resume.replace('            self.wall_origin = std::time::Instant::now();\n', ''), background,
           'background resume retained the paused wall epoch')
    change('idle-background-resume-stays-paused', background_resume,
           background_resume.replace('            self.clock_paused = false;\n', ''), background,
           'background resume left the wall clock paused')
    change('idle-render-frame-counter-wraps', '.saturating_add(frames as u64)', '.wrapping_add(frames as u64)',
           'render_frame_counter_saturates_instead_of_wrapping', 'render clock wrapped at its frame limit')
    change('idle-public-time-loses-render-lead', '|wall| rendered.max(wall)', '|wall| wall', aligned,
           'public currentTime lost the graph render lead')
    change('idle-public-time-loses-wall-lead', '|wall| rendered.max(wall)', '|_wall| rendered',
           'a_new_idle_epoch_rebases_forward_without_rewinding_existing_render_time', 'public currentTime did not advance during idle')
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    path = package / 'src/context.rs'
    original = path.read_text()
    rows = declarations(original)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 7 if preflight else 567
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        profiles[profile] = dict(passed=count, ignored=0 if preflight else 2)
        programs.append(dict(name=label, path=str(binary), sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    if not preflight:
        for name, before, after, test, assertion in rows:
            try:
                path.write_text(original.replace(before, after))
                binary = product.src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(t in text for t in ['running 1 test', test, 'panicked at', '1 failed', assertion]), (name, status, log)
                digest = whole.support.codecs.sha(binary)
                controls.append(dict(name=name, test=test, expected_assertion=assertion, exit=status, binary_sha256=digest))
                programs.append(dict(name=name, path=str(binary), sha256=digest, fresh_rust=True, pruned=True))
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('actual idle-clock fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   production_render_clock_rebased=True, pause_banks_later_clock=True, production_wall_policy_exercised=True,
                   production_device_sinc_migrated=False, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Production idle clock verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
