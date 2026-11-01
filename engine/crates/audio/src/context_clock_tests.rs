use super::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn clock(rate: u32, wall: f64) -> AudioContext {
    let mut context = AudioContext::new(1, rate, 1);
    context.test_wall_enabled = true;
    context.wall_banked = wall;
    freeze_elapsed(&mut context);
    context
}
fn freeze_elapsed(context: &mut AudioContext) {
    // Exercise the production wall policy with exactly zero elapsed time.
    context.wall_origin = Instant::now() + Duration::from_secs(60);
}
struct Times(Arc<Mutex<Vec<f64>>>);
impl AudioNodeProcessor for Times {
    fn id(&self) -> u32 {
        10
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_producing(&self) -> bool {
        true
    }
    fn process(&mut self, _: &[f32], out: &mut [f32], _: u32, _: u32, time: f64) -> usize {
        self.0.lock().unwrap().push(time);
        out.fill(0.25);
        out.len()
    }
}

#[test]
fn consecutive_blocks_after_idle_advance_the_graph_sample_clock() {
    let mut context = clock(48000, 1.0);
    let times = Arc::new(Mutex::new(Vec::with_capacity(4)));
    context.add_node(Box::new(Times(times.clone())));
    context.connect(10, DESTINATION_NODE_ID);
    for _ in 0..4 {
        context.process(&mut [0.0; 128]);
    }
    let times = times.lock().unwrap();
    for (i, &time) in times.iter().enumerate() {
        assert_eq!(
            time,
            (48000 + i * 128) as f64 / 48000.0,
            "post-idle graph time did not advance by its rendered quantum"
        );
    }
    assert_eq!(context.frames_processed, 48000 + 512);
}

#[test]
fn suspension_banks_rendered_time_ahead_of_the_wall() {
    let mut context = clock(48000, 0.0);
    context.process(&mut [0.0; 128]);
    let rendered = context.current_time();
    context.suspend();
    assert!(
        context.wall_banked >= rendered,
        "suspension discarded the rendered clock lead"
    );
    context.wall_origin = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        context.current_time(),
        rendered,
        "suspended wall time advanced"
    );
    let resumed_after = Instant::now();
    context.resume();
    assert!(
        context.wall_origin >= resumed_after,
        "resume retained the paused wall epoch"
    );
    freeze_elapsed(&mut context);
    assert_eq!(context.current_time(), rendered);
    context.process(&mut [0.0; 128]);
    assert_eq!(context.frames_processed, 256);
    let mut idle = clock(48000, 1.0);
    idle.suspend();
    assert_eq!(
        idle.current_time(),
        1.0,
        "suspension discarded the idle wall epoch"
    );
}

#[test]
fn scheduled_source_after_idle_starts_in_the_correct_rendered_block() {
    let mut context = clock(48000, 1.0);
    let buffer = context
        .add_buffer(DecodedAudio {
            samples: vec![0.5; 480],
            sample_rate: 48000,
            channels: 1,
        })
        .unwrap();
    context.add_node(Box::new(BufferSourceNode::new(10, 48000)));
    context.set_buffer(10, Some(buffer));
    context.connect(10, DESTINATION_NODE_ID);
    assert!(context.start_source(10, 1.0 + 192.0 / 48000.0, 0.0, None));
    let mut out = [0.0; 128];
    context.process(&mut out);
    assert!(out.iter().all(|&v| v == 0.0));
    out.fill(0.0);
    context.process(&mut out);
    assert!(out[..64].iter().all(|&v| v == 0.0));
    assert!(
        out[64..].iter().all(|&v| v == 0.5),
        "scheduled source missed its exact post-idle sample"
    );
    out.fill(0.0);
    context.process(&mut out);
    assert!(
        out.iter().all(|&v| v == 0.5),
        "scheduled audio stayed silent after idle"
    );
}

#[test]
fn wall_epoch_is_aligned_once_and_never_rounded_through_the_rendered_clock() {
    for rate in [3000u32, 44100, 48000, 768000] {
        let mut context = clock(rate, 0.125);
        let start = u64::from(rate.div_ceil(8));
        let mut frames = 0u64;
        let mut out = [0.0; 128];
        for n in [1usize, 3, 127, 128].into_iter().cycle().take(512) {
            context.process(&mut out[..n]);
            frames += n as u64;
            assert_eq!(
                context.frames_processed,
                start + frames,
                "idle clock rebase accumulated per-block rounding"
            );
            assert_eq!(
                context.current_time(),
                (start + frames) as f64 / f64::from(rate),
                "public currentTime lost the graph render lead"
            );
        }
    }
}

#[test]
fn background_pause_banks_the_render_lead_and_resumes_without_a_clock_gap() {
    let mut context = clock(44100, 0.125);
    context.process(&mut [0.0; 128]);
    let now = context.current_time();
    context.pause_clock_for_background();
    assert_eq!(
        context.wall_banked, now,
        "background pause did not bank the render lead"
    );
    context.wall_origin = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        context.current_time(),
        now,
        "background pause lost the rendered clock lead"
    );
    assert_eq!(context.state(), AudioContextState::Running);
    let resumed_after = Instant::now();
    context.resume_clock_after_background();
    assert!(
        context.wall_origin >= resumed_after,
        "background resume retained the paused wall epoch"
    );
    freeze_elapsed(&mut context);
    assert_eq!(context.current_time(), now);
    let frame = context.frames_processed;
    context.process(&mut [0.0; 128]);
    assert_eq!(
        context.frames_processed,
        frame + 128,
        "background resume inserted a clock gap"
    );
    context.wall_origin = Instant::now() - Duration::from_secs(1);
    assert!(
        context.current_time() >= context.wall_banked + 1.0,
        "background resume left the wall clock paused"
    );
}

#[test]
fn a_new_idle_epoch_rebases_forward_without_rewinding_existing_render_time() {
    let mut context = clock(48000, 0.125);
    context.process(&mut [0.0; 128]);
    context.wall_banked = 2.0;
    freeze_elapsed(&mut context);
    assert_eq!(
        context.current_time(),
        2.0,
        "public currentTime did not advance during idle"
    );
    context.process(&mut [0.0; 128]);
    assert_eq!(
        context.frames_processed,
        96000 + 128,
        "new idle epoch did not rebase the render clock"
    );
    context.wall_banked = 1.0;
    freeze_elapsed(&mut context);
    context.process(&mut [0.0; 128]);
    assert_eq!(
        context.frames_processed,
        96000 + 256,
        "render time rewound to an older wall epoch"
    );
}

#[test]
fn render_frame_counter_saturates_instead_of_wrapping() {
    let mut context = AudioContext::new(1, 48000, 1);
    context.frames_processed = u64::MAX - 64;
    context.process(&mut [0.0; 128]);
    assert_eq!(
        context.frames_processed,
        u64::MAX,
        "render clock wrapped at its frame limit"
    );
}
