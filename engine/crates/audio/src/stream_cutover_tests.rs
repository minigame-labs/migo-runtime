//! Output samples choose loop boundaries; rounded per-track lengths do not.
use super::*;

fn input_at(output_frame: u64, input: u32, output: u32) -> u128 {
    u128::from(output_frame / u64::from(output)) * u128::from(input)
        + u128::from(output_frame % u64::from(output)) * u128::from(input) / u128::from(output)
}
#[test]
fn disabling_finishes_the_consumed_cycle_on_the_global_grid() {
    for (input, output) in [
        (48000, 44100),
        (44100, 48000),
        (48000, 16000),
        (48000, 48000),
        (3001, 768000),
        (768000, 3001),
    ] {
        let clock = StreamTimeline::new(17, input, output, 2).unwrap();
        for at in [0, 1, 7, clock.output_frames(), 12345, 9_007_199_254_740_993] {
            let plan = StreamCutover::new(clock, at, false, 258).unwrap();
            let cycles = input_at(at, input, output) / 17 + 1;
            let scaled = cycles * 17 * u128::from(output);
            let end =
                scaled / u128::from(input) + u128::from(!scaled.is_multiple_of(u128::from(input)));
            assert_eq!(u128::from(plan.cycles().unwrap()), cycles);
            assert_eq!(u128::from(plan.presentation().output_end().unwrap()), end);
            assert!(end > u128::from(at));
            assert_eq!(plan.presentation().timeline(), clock);
            assert_eq!(plan.output_start(), at);
        }
    }
}
#[test]
fn cyclic_preroll_crosses_short_tracks_without_rebasing_phase_to_each_track() {
    let clock = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    let plan = StreamCutover::new(clock, 10001, true, 258).unwrap();
    let target = input_at(10001, 48000, 44100) as u64;
    assert!(plan.input_origin() + 258 <= target);
    assert!(target - plan.input_origin() < 258 + 160);
    assert_eq!(plan.input_origin() * 44100 % 48000, 0);
    let (local, completed) = plan.source_origin();
    assert_eq!(completed * 17 + local, plan.input_origin());
    assert!(completed > 32);
    assert!(local < 17);
    assert!(plan.presentation().output_end().is_none());
    assert!(plan.cycles().is_none());
    assert_eq!(plan.input_frames(), 17);
}
#[test]
fn arbitrary_output_samples_are_not_rounded_to_an_input_frame_boundary() {
    let clock = StreamTimeline::new(17, 48000, 768000, 1).unwrap();
    let plan = StreamCutover::new(clock, 1, true, 258).unwrap();
    assert_eq!(plan.output_start(), 1);
    assert_eq!(plan.input_origin(), 0);
    assert_ne!(clock.position(0).unwrap().frame, plan.output_start());
    let p = StreamCutover::new(clock, 9_007_199_254_740_993, true, 258).unwrap();
    assert_eq!(p.output_start(), 9_007_199_254_740_993);
    let target = input_at(p.output_start(), 48000, 768000);
    assert_eq!(u128::from(p.input_origin()), target.saturating_sub(258));
    let clock = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    for delta in 0..512 {
        let at = 9_007_199_254_740_993 + delta;
        let p = StreamCutover::new(clock, at, false, 258).unwrap();
        let target = input_at(at, 48000, 44100);
        assert_eq!(u128::from(p.cycles().unwrap()), target / 17 + 1);
        assert_eq!(
            u128::from(p.input_origin()),
            target.saturating_sub(258) / 160 * 160
        );
    }
}
#[test]
fn cutover_overflow_is_rejected_without_saturating_or_wrapping() {
    let clock = StreamTimeline::new(17, 48000, 16000, 1).unwrap();
    assert!(StreamCutover::new(clock, u64::MAX, true, 258).is_err());
    let clock = StreamTimeline::new(1, 48000, 48000, 1).unwrap();
    assert!(StreamCutover::new(clock, u64::MAX, false, 258).is_err());
    assert!(StreamCutover::new(clock, u64::MAX, true, 258).is_ok());
    let clock = StreamTimeline::new(u64::MAX, 48000, 48000, 1).unwrap();
    assert!(StreamCutover::new(clock, u64::MAX, false, 258).is_err());
}
#[test]
fn same_rate_cutover_uses_the_exact_sample_without_filter_preroll() {
    let clock = StreamTimeline::new(17, 48000, 48000, 1).unwrap();
    for at in [0, 17, 257, 9_007_199_254_740_993, u64::MAX - 17] {
        let p = StreamCutover::new(clock, at, true, 258).unwrap();
        assert_eq!(p.input_origin(), at);
        assert_eq!(p.source_origin(), (at % 17, at / 17));
    }
}
#[test]
fn cutover_planning_has_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let clock = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    assert_no_steady_state_allocation(
        Burst {
            path: "loop cutover output clock",
            warmup: 64,
            measured: 256,
        },
        |i| {
            std::hint::black_box(
                StreamCutover::new(clock, 10001 + i as u64, i.is_multiple_of(2), 258).unwrap(),
            );
        },
    );
}
