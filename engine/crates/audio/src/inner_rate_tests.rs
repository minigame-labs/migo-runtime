//! Product cutover checks: independent whole-input native oracle, exact media
//! clock, command/reset boundaries, physical admission and original CPU Drop.
use super::*;
use crate::node_retirement::Host;
use std::{
    ffi::{c_int, c_long, c_void},
    sync::{Condvar, Mutex, atomic::AtomicUsize},
    time::{Duration, Instant},
};

#[repr(C)]
struct NativeData {
    input: *const f32,
    output: *mut f32,
    input_frames: c_long,
    output_frames: c_long,
    used: c_long,
    produced: c_long,
    eof: c_int,
    ratio: f64,
}
unsafe extern "C" {
    fn src_new(kind: c_int, channels: c_int, error: *mut c_int) -> *mut c_void;
    fn src_delete(owner: *mut c_void) -> *mut c_void;
    fn src_set_ratio(owner: *mut c_void, ratio: f64) -> c_int;
    fn src_process(owner: *mut c_void, data: *mut NativeData) -> c_int;
    fn migo_src_required_bytes(channels: c_int) -> usize;
}
#[cfg(migo_native_alloc_probe)]
unsafe extern "C" {
    fn probe_alloc_count() -> usize;
    fn probe_requested_bytes() -> usize;
    fn probe_watch_constructed(observer: extern "C" fn(*mut c_void, usize), context: *mut c_void);
}
#[cfg(migo_native_alloc_probe)]
struct NativeFrees {
    count: AtomicUsize,
    bytes: AtomicUsize,
    wrong: AtomicBool,
    worker: Arc<Mutex<Option<std::thread::ThreadId>>>,
    ledger: Arc<AudioAggregateLedger>,
    charged: usize,
}
#[cfg(migo_native_alloc_probe)]
extern "C" fn native_freed(context: *mut c_void, bytes: usize) {
    let seen = unsafe { &*context.cast::<NativeFrees>() };
    if Some(std::thread::current().id()) != *seen.worker.lock().unwrap()
        || seen.ledger.used_bytes() != seen.charged
    {
        seen.wrong.store(true, Ordering::Release);
    }
    seen.bytes.fetch_add(bytes, Ordering::Release);
    seen.count.fetch_add(1, Ordering::Release);
}
#[cfg(migo_native_alloc_probe)]
extern "C" fn web_native_freed(context: *mut c_void, bytes: usize) {
    let seen = unsafe { &*context.cast::<NativeFrees>() };
    if Some(std::thread::current().id()) != *seen.worker.lock().unwrap()
        || seen.ledger.used_bytes() < seen.charged
    {
        seen.wrong.store(true, Ordering::Release);
    }
    seen.bytes.fetch_add(bytes, Ordering::Release);
    seen.count.fetch_add(1, Ordering::Release);
}

struct Reference {
    owner: *mut c_void,
    input: Vec<f32>,
    read: usize,
    channels: usize,
}
impl Reference {
    fn new(input: &[f32], channels: usize) -> Self {
        let mut error = 0;
        let owner = unsafe { src_new(0, channels as c_int, &mut error) };
        assert!(!owner.is_null() && error == 0);
        let mut padded = input.to_vec();
        padded.resize(input.len() + 8192 * channels, 0.);
        Self {
            owner,
            input: padded,
            read: 0,
            channels,
        }
    }
    // Whole remaining source and one output frame. No product staging, reset,
    // cursor, duration or output-block helper is reused here.
    fn one(&mut self, units: u32) -> Vec<f32> {
        self.one_ratio(1000. / f64::from(units))
    }
    fn one_ratio(&mut self, ratio: f64) -> Vec<f32> {
        let mut output = vec![0.; self.channels];
        assert_eq!(unsafe { src_set_ratio(self.owner, ratio) }, 0);
        let mut d = NativeData {
            input: self.input[self.read * self.channels..].as_ptr(),
            output: output.as_mut_ptr(),
            input_frames: (self.input.len() / self.channels - self.read) as c_long,
            output_frames: 1,
            used: 0,
            produced: 0,
            eof: 0,
            ratio,
        };
        assert_eq!(unsafe { src_process(self.owner, &mut d) }, 0);
        assert_eq!(d.produced, 1);
        self.read += d.used as usize;
        output
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        assert!(unsafe { src_delete(self.owner) }.is_null());
    }
}
fn signal(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| {
            if i == 0 {
                -0.0
            } else {
                ((i * 17 % 1009) as f32 - 504.) / 512.
            }
        })
        .collect()
}
fn setup(
    channels: usize,
    id: i32,
) -> (
    migo_io::scheduler::IoScheduler,
    Arc<AudioAggregateLedger>,
    Host,
    InnerAudioPlayer,
) {
    let scheduler = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(id, 1, u64::MAX);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(32 << 20));
    let host = Host::new(cpu.clone(), ledger.clone(), channels as u32).unwrap();
    let mut player = InnerAudioPlayer::new_with_aggregate(1, channels as u32, ledger.clone())
        .with_rate_host(host.clone());
    player.retirement_cpu = Some(cpu);
    (scheduler, ledger, host, player)
}
fn load(player: &mut InnerAudioPlayer, input: &[f32], channels: usize) {
    player
        .load_audio(DecodedAudio {
            samples: input.to_vec(),
            sample_rate: 48000,
            channels: channels as u32,
        })
        .unwrap();
    player.drain_events().for_each(drop);
}
fn save(name: &str, actual: &[f32], expected: &[f32]) {
    if let Some(directory) = std::env::var_os("MIGO_INNER_RATE_PCM_DIR") {
        let root = std::path::PathBuf::from(directory);
        for (label, pcm) in [("actual", actual), ("reference", expected)] {
            std::fs::write(
                root.join(format!("{name}-{label}.f32le")),
                pcm.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
        }
    }
}

fn web_context(
    channels: usize,
    id: i32,
) -> (
    migo_io::scheduler::IoScheduler,
    Arc<AudioAggregateLedger>,
    crate::context::AudioContext,
) {
    let io = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(id, 1, u64::MAX);
    let ledger = Arc::new(AudioAggregateLedger::new(32 << 20));
    let budget = crate::limits::PcmBudget::with_aggregate(
        Arc::new(crate::limits::PcmUsage::new(32 << 20, 32)),
        Arc::new(crate::limits::PcmUsage::new(32 << 20, 32)),
        ledger.clone(),
    );
    let mut ctx =
        crate::context::AudioContext::new_with_pcm_budget(1, 48000, channels as u32, budget);
    ctx.prepare_node_retirement(io.audio_cpu(), ledger.clone())
        .unwrap();
    (io, ledger, ctx)
}

fn web_process(ctx: &mut crate::context::AudioContext, output: &mut [f32], channels: usize) {
    for block in output.chunks_mut(128 * channels) {
        ctx.process(block);
    }
}

fn web_reference_at(input: &[f32], channels: usize, position: f64) -> Reference {
    let frame = position.floor() as usize;
    let phase = position - frame as f64;
    let mut oracle = if frame == 0 && phase != 0.0 {
        // The fractional phase step starts one frame before the source. Give
        // the independent oracle that explicit zero frame rather than wrapping.
        let mut padded = vec![0.0; channels];
        padded.extend_from_slice(input);
        Reference::new(&padded, channels)
    } else {
        Reference::new(input, channels)
    };
    for _ in 0..frame.saturating_sub(usize::from(phase != 0.0)) {
        oracle.one_ratio(1.0);
    }
    if phase != 0.0 {
        oracle.one_ratio(1.0 / (1.0 + phase));
    }
    oracle
}

#[test]
fn webaudio_nonloop_native_keeps_f64_compound_rate_offset_and_complete_pcm() {
    for channels in [1, 2, 32] {
        for (rate, cents, source_rate) in
            [(1.33337f32, 53.25f32, 44100u32), (0.7312345, -67.1, 48000)]
        {
            let (_io, ledger, mut ctx) = web_context(channels, 1340 + channels as i32);
            let input = signal(4099, channels);
            let buffer = ctx
                .add_buffer(DecodedAudio {
                    samples: input.clone(),
                    sample_rate: source_rate,
                    channels: channels as u32,
                })
                .unwrap();
            assert!(ctx.create_buffer_source(41));
            ctx.connect(41, crate::context::DESTINATION_NODE_ID);
            assert!(ctx.set_buffer(41, Some(buffer)));
            assert!(ctx.set_node_param(41, "playbackRate", rate));
            assert!(ctx.set_node_param(41, "detune", cents));
            assert!(ctx.watch_source_ended(41, true));
            let credit = ledger.used_bytes();
            #[cfg(migo_native_alloc_probe)]
            let allocations = unsafe { probe_alloc_count() };
            assert!(ctx.start_source(
                41,
                0.0,
                17.375 / source_rate as f64,
                Some(2050.25 / source_rate as f64)
            ));
            assert!(
                ledger.used_bytes()
                    >= credit
                        + unsafe { migo_src_required_bytes(channels as c_int) }
                        + crate::node_retirement::HANDOFF_BYTES,
                "WebAudio source did not own native capacity before start"
            );
            #[cfg(migo_native_alloc_probe)]
            assert_eq!(
                unsafe { probe_alloc_count() } - allocations,
                3,
                "WebAudio start did not prepare native state"
            );
            let effective =
                rate as f64 * 2.0f64.powf(cents as f64 / 1200.0) * (source_rate as f64 / 48000.0);
            let mut oracle = Reference::new(&input, channels);
            for _ in 0..16 {
                oracle.one_ratio(1.0);
            }
            oracle.one_ratio(1.0 / 1.375);
            let (mut position, mut elapsed) = (17.375, 0.0);
            let (mut actual, mut expected) = (Vec::new(), Vec::new());
            let mut ended = 0;
            let mut turn = 0;
            while elapsed < 2050.25 {
                let request = [1, 17, 127, 128, 129, 257][turn % 6];
                turn += 1;
                let mut output = vec![0.; request * channels];
                let before = migo_alloc_probe::thread_counts();
                #[cfg(migo_native_alloc_probe)]
                let native_before = unsafe { probe_alloc_count() };
                web_process(&mut ctx, &mut output, channels);
                let after = migo_alloc_probe::thread_counts();
                assert_eq!(
                    (before.allocations, before.reallocations),
                    (after.allocations, after.reallocations),
                    "WebAudio native source allocated in render"
                );
                #[cfg(migo_native_alloc_probe)]
                assert_eq!(unsafe { probe_alloc_count() }, native_before);
                let mut count = 0;
                while count < request && position < 4099.0 && elapsed < 2050.25 {
                    expected.extend(
                        oracle
                            .one_ratio(1.0 / effective)
                            .into_iter()
                            .map(|v| 0.0 + v),
                    );
                    position += effective;
                    elapsed += effective;
                    count += 1;
                }
                assert!(
                    output[count * channels..].iter().all(|v| *v == 0.0),
                    "WebAudio native duration overwrote the complete tail"
                );
                actual.extend_from_slice(&output[..count * channels]);
                ended += ctx.drain_ended().filter(|id| *id == 41).count();
            }
            assert_eq!(
                actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "WebAudio native PCM lost f64 rate, offset or detune"
            );
            assert_eq!(
                ended, 1,
                "WebAudio native source reported ended at the wrong boundary"
            );
            save(
                &format!("web-{channels}-{source_rate}-{rate}"),
                &actual,
                &expected,
            );
        }
    }
}

#[test]
fn webaudio_native_admission_and_late_controls_preserve_unity_and_original_cpu() {
    let (io, ledger, mut ctx) = web_context(2, 1378);
    let input = signal(4099, 2);
    let buffer = ctx
        .add_buffer(DecodedAudio {
            samples: input.clone(),
            sample_rate: 48000,
            channels: 2,
        })
        .unwrap();
    assert!(ctx.create_buffer_source(42));
    ctx.connect(42, crate::context::DESTINATION_NODE_ID);
    assert!(ctx.set_buffer(42, Some(buffer)));
    #[cfg(migo_native_alloc_probe)]
    let allocations = unsafe { probe_alloc_count() };
    assert!(ctx.start_source(42, 0.0, 0.0, None));
    ctx.process(&mut [0.; 256]);
    assert!(ctx.set_node_param(42, "playbackRate", 1.0));
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        allocations,
        "default WebAudio unity constructed native state"
    );
    assert!(ctx.create_buffer_source(43));
    assert!(ctx.set_buffer(43, Some(buffer)));
    assert!(ctx.set_node_param(43, "playbackRate", 0.5));
    assert!(ctx.create_buffer_source(44));
    assert!(ctx.set_node_param(44, "playbackRate", 0.731));
    assert!(ctx.start_source(44, 0.0, 0.0, None));
    let registry =
        shared::audio_resources::AudioResourceRegistry::with_physical_ledger(ledger.clone());
    assert!(ctx.create_buffer_source(45));
    assert!(ctx.set_node_param(45, "playbackRate", 0.731));
    let lease = registry
        .adopt_frozen(
            1,
            shared::audio_resources::AudioBufferFormat {
                channels: 2,
                frames: 17,
                sample_rate: 48000,
            },
            signal(17, 2),
        )
        .unwrap();
    let snapshot = registry
        .prepare_snapshot(lease.key(), None)
        .unwrap()
        .commit();
    let pointer = Arc::as_ptr(&snapshot);
    let sample_pointer = snapshot.samples().as_ptr();
    let mut incoming = Some(snapshot);
    let deny = ledger
        .try_reserve((32 << 20) - ledger.used_bytes(), "WebAudio source peer")
        .unwrap();
    assert!(
        !ctx.start_source(43, 0.0, 0.0, None),
        "WebAudio start bypassed native admission"
    );
    assert!(
        !ctx.try_set_started_buffer(44, &mut incoming),
        "WebAudio binding bypassed native admission"
    );
    assert_eq!(
        Arc::as_ptr(incoming.as_ref().unwrap()),
        pointer,
        "refused WebAudio binding took incoming snapshot"
    );
    assert_eq!(
        incoming.as_ref().unwrap().samples().as_ptr(),
        sample_pointer
    );
    assert!(
        !ctx.try_start_buffer(45, &mut incoming, 0.0, 0.0, None),
        "atomic WebAudio start took input before native admission"
    );
    assert_eq!(Arc::as_ptr(incoming.as_ref().unwrap()), pointer);
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        allocations,
        "WebAudio constructor ran before admission"
    );
    assert!(
        !ctx.set_node_param(42, "detune", 600.0),
        "WebAudio detune bypassed native admission"
    );
    let mut next = [0.; 2];
    ctx.process(&mut next);
    assert_eq!(
        next,
        input[256..258],
        "refused WebAudio detune changed the source clock or parameter"
    );
    drop(deny);
    drop(incoming);
    assert!(registry.release_buffer(lease.key()));
    #[cfg(migo_native_alloc_probe)]
    let count = unsafe { probe_alloc_count() };
    assert!(ctx.start_source(43, 0.0, 0.0, None));
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() } - count,
        3,
        "refused WebAudio start changed Pending state"
    );
    assert!(ctx.stop_source(43, 0.0));
    ctx.remove_finished_node(43);
    assert!(ctx.set_node_param(42, "detune", 600.0));
    let charged = ledger.used_bytes();
    #[cfg(migo_native_alloc_probe)]
    let allocations = unsafe { probe_alloc_count() };
    let deny = ledger
        .try_reserve((32 << 20) - charged, "WebAudio retained source peer")
        .unwrap();
    assert!(ctx.set_node_param(42, "detune", 0.0));
    assert!(ctx.set_node_param(42, "playbackRate", 1.33337));
    assert!(
        !ctx.param_set_value_at_time(42, "playbackRate", 0.75, 10.0),
        "WebAudio timeline growth escaped a saturated ledger"
    );
    let mut oracle = Reference::new(&input, 2);
    for _ in 0..129 {
        oracle.one_ratio(1.0);
    }
    let mut output = [0.; 514];
    web_process(&mut ctx, &mut output, 2);
    let expected: Vec<_> = (0..257)
        .flat_map(|_| oracle.one_ratio(1.0 / 1.33337f32 as f64))
        .map(|v| 0.0 + v)
        .collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "WebAudio late controls lost native history or changed refused automation"
    );
    save("web-late-controls", &output, &expected);
    let mut position = 129.0;
    for _ in 0..257 {
        position += 1.33337f32 as f64;
    }
    assert!(ctx.set_node_param(42, "playbackRate", 0.0));
    let frame = position.floor() as usize;
    let phase = (position - frame as f64) as f32;
    let held: Vec<_> = (0..2)
        .map(|ch| {
            let first = input[frame * 2 + ch];
            0.0 + first + (input[(frame + 1) * 2 + ch] - first) * phase
        })
        .collect();
    let mut paused_rate = [0.; 34];
    ctx.process(&mut paused_rate);
    assert!(
        paused_rate.chunks_exact(2).all(|sample| sample == held),
        "WebAudio zero rate advanced or changed the held sample"
    );
    assert!(ctx.set_node_param(42, "playbackRate", 0.7312345));
    let mut oracle = web_reference_at(&input, 2, position);
    let mut output = [0.; 258];
    web_process(&mut ctx, &mut output, 2);
    let expected: Vec<_> = (0..129)
        .flat_map(|_| oracle.one_ratio(1.0 / 0.7312345f32 as f64))
        .map(|v| 0.0 + v)
        .collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "WebAudio zero-rate reentry lost exact phase"
    );
    save("web-zero-reentry", &output, &expected);
    for _ in 0..129 {
        position += 0.7312345f32 as f64;
    }
    assert!(ctx.set_loop(42, true, 0.0, 1024.0 / 48000.0));
    let mut loop_output = [0.; 34];
    ctx.process(&mut loop_output);
    for _ in 0..17 {
        position += 0.7312345f32 as f64;
    }
    assert!(ctx.set_loop(42, false, 0.0, 1024.0 / 48000.0));
    let mut oracle = web_reference_at(&input, 2, position);
    let mut output = [0.; 258];
    web_process(&mut ctx, &mut output, 2);
    let expected: Vec<_> = (0..129)
        .flat_map(|_| oracle.one_ratio(1.0 / 0.7312345f32 as f64))
        .map(|v| 0.0 + v)
        .collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "WebAudio loop reentry kept stale native lookahead"
    );
    save("web-loop-reentry", &output, &expected);

    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        allocations,
        "WebAudio retained source reconstructed native state"
    );
    drop(deny);
    #[cfg(migo_native_alloc_probe)]
    {
        let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
        let worker = Arc::new(Mutex::new(None));
        let value = Block {
            gate: gate.0.clone(),
            worker: worker.clone(),
        };
        let retired = io.audio_cpu().retirement();
        crate::streaming::get_stream_runtime().spawn(async move { retired.retire(value).await });
        wait(|| worker.lock().unwrap().is_some());
        let watched = NativeFrees {
            count: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            wrong: AtomicBool::new(false),
            worker,
            ledger: ledger.clone(),
            charged: unsafe { migo_src_required_bytes(2) },
        };
        unsafe {
            probe_watch_constructed(
                web_native_freed,
                (&watched as *const NativeFrees).cast_mut().cast(),
            )
        };
        assert!(ctx.set_buffer(42, None));
        assert_eq!(
            watched.count.load(Ordering::Acquire),
            0,
            "WebAudio native destructor escaped original CPU"
        );
        drop(gate);
        let start = Instant::now();
        while watched.count.load(Ordering::Acquire) < 3 {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !watched.wrong.load(Ordering::Acquire),
            "WebAudio native credit returned before actual original-CPU free"
        );
        assert_eq!(watched.bytes.load(Ordering::Acquire), unsafe {
            migo_src_required_bytes(2)
        });
    }
}
#[test]
fn webaudio_integer_loops_use_native_pcm_and_keep_the_authoritative_duration() {
    for channels in [1, 2, 32] {
        for (case, (start, end, offset, rate, cents, sample_rate)) in [
            (0, 67, 0.375, 0.7312345f32, -67.1f32, 48000u32),
            (3, 17, 1.375, 1.33337, 53.25, 44100),
            (2, 3, 9.0, 0.625, 0.0, 48000),
        ]
        .into_iter()
        .enumerate()
        {
            let (_io, ledger, mut ctx) =
                web_context(channels, 1600 + case as i32 * 64 + channels as i32);
            let mut input = signal(67, channels);
            input[end * channels..].fill(1000.0);
            let buffer = ctx
                .add_buffer(DecodedAudio {
                    samples: input.clone(),
                    sample_rate,
                    channels: channels as u32,
                })
                .unwrap();
            assert!(ctx.create_buffer_source(46));
            ctx.connect(46, crate::context::DESTINATION_NODE_ID);
            assert!(ctx.set_buffer(46, Some(buffer)));
            assert!(ctx.set_loop(
                46,
                true,
                start as f64 / sample_rate as f64,
                if case == 0 {
                    0.0
                } else {
                    end as f64 / sample_rate as f64
                },
            ));
            assert!(ctx.set_node_param(46, "playbackRate", rate));
            assert!(ctx.set_node_param(46, "detune", cents));
            assert!(ctx.watch_source_ended(46, true));
            let before = ledger.used_bytes();
            #[cfg(migo_native_alloc_probe)]
            let allocated = unsafe { probe_alloc_count() };
            assert!(ctx.start_source(
                46,
                0.0,
                offset / sample_rate as f64,
                Some(257.25 / sample_rate as f64),
            ));
            assert!(
                ledger.used_bytes()
                    >= before + unsafe { migo_src_required_bytes(channels as c_int) },
                "integer WebAudio loop did not admit its native converter"
            );
            #[cfg(migo_native_alloc_probe)]
            assert_eq!(unsafe { probe_alloc_count() } - allocated, 3);

            // Explicitly unroll the intro and the selected PCM period before
            // handing the entire independent signal to the raw-C oracle.
            let mut unrolled = input[..end * channels].to_vec();
            while unrolled.len() / channels < 12288 {
                unrolled.extend_from_slice(&input[start * channels..end * channels]);
            }
            let initial = if offset >= end as f64 {
                start as f64
            } else {
                offset
            };
            let mut reference = web_reference_at(&unrolled, channels, initial);
            let effective =
                rate as f64 * 2.0f64.powf(cents as f64 / 1200.0) * (sample_rate as f64 / 48000.0);
            let (mut actual, mut expected) = (Vec::new(), Vec::new());
            let (mut elapsed, mut ended, mut turn) = (0.0, 0, 0);
            while elapsed < 257.25 {
                let request = [1, 17, 127, 128, 129, 257][turn % 6];
                turn += 1;
                let mut output = vec![0.0; request * channels];
                let before = migo_alloc_probe::thread_counts();
                #[cfg(migo_native_alloc_probe)]
                let allocated = unsafe { probe_alloc_count() };
                web_process(&mut ctx, &mut output, channels);
                let after = migo_alloc_probe::thread_counts();
                assert_eq!(
                    (before.allocations, before.reallocations),
                    (after.allocations, after.reallocations)
                );
                #[cfg(migo_native_alloc_probe)]
                assert_eq!(unsafe { probe_alloc_count() }, allocated);
                let mut count = 0;
                while count < request && elapsed < 257.25 {
                    expected.extend(
                        reference
                            .one_ratio(1.0 / effective)
                            .into_iter()
                            .map(|v| 0.0 + v),
                    );
                    elapsed += effective;
                    count += 1;
                }
                assert!(output[count * channels..].iter().all(|v| *v == 0.0));
                actual.extend_from_slice(&output[..count * channels]);
                ended += ctx.drain_ended().filter(|id| *id == 46).count();
            }
            assert_eq!(
                ended, 1,
                "native WebAudio loop lost its duration or ended boundary"
            );
            assert!(
                actual
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "native WebAudio loop PCM disagrees with the unrolled signal"
            );
            save(
                &format!("web-native-loop-{channels}-{case}"),
                &actual,
                &expected,
            );
        }
    }
}

#[test]
fn webaudio_loop_activation_admission_and_fallbacks_keep_the_existing_owner() {
    let (_io, ledger, mut ctx) = web_context(2, 1810);
    let input = signal(67, 2);
    let buffer = ctx
        .add_buffer(DecodedAudio {
            samples: input.clone(),
            sample_rate: 48000,
            channels: 2,
        })
        .unwrap();
    assert!(ctx.create_buffer_source(47));
    ctx.connect(47, crate::context::DESTINATION_NODE_ID);
    assert!(ctx.set_buffer(47, Some(buffer)));
    assert!(ctx.set_loop(47, true, 0.0, 0.0));
    #[cfg(migo_native_alloc_probe)]
    let allocated = unsafe { probe_alloc_count() };
    assert!(ctx.start_source(47, 0.0, 0.0, None));
    // Traverse far more than the native history window before activation.
    let unity_frames = 8193;
    web_process(&mut ctx, &mut vec![0.0; unity_frames * 2], 2);
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        allocated,
        "unity loop constructed native state"
    );
    let deny = ledger
        .try_reserve((32 << 20) - ledger.used_bytes(), "loop activation peer")
        .unwrap();
    assert!(
        !ctx.set_playback_rate(47, 0.7312345),
        "loop rate bypassed native admission"
    );
    let mut held = [0.0; 34];
    ctx.process(&mut held);
    let expected: Vec<_> = (unity_frames..unity_frames + 17)
        .flat_map(|n| {
            input[(n % 67) * 2..(n % 67 + 1) * 2]
                .iter()
                .map(|v| 0.0 + v)
        })
        .collect();
    assert_eq!(
        held.as_slice(),
        expected,
        "refused loop rate changed its PCM or clock"
    );
    drop(deny);
    assert!(ctx.set_playback_rate(47, 0.7312345));
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(unsafe { probe_alloc_count() } - allocated, 3);
    let unrolled = input.repeat(256);
    let mut reference = web_reference_at(&unrolled, 2, (unity_frames + 17) as f64);
    for (part, frames) in [(0, 513), (1, 129)] {
        if part == 1 {
            assert!(ctx.set_loop(47, true, 0.0, 0.0));
        }
        let mut output = vec![0.0; frames * 2];
        let expected: Vec<_> = (0..frames)
            .flat_map(|_| reference.one_ratio(1.0 / 0.7312345f32 as f64))
            .map(|v| 0.0 + v)
            .collect();
        web_process(&mut ctx, &mut output, 2);
        assert!(
            output
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "loop activation or repeated setting lost real past or filter phase"
        );
        save(&format!("web-loop-activation-{part}"), &output, &expected);
    }
    let now = ctx.current_time();
    #[cfg(migo_native_alloc_probe)]
    let automation_allocated = unsafe { probe_alloc_count() };
    assert!(ctx.param_set_value_at_time(47, "playbackRate", 1.33337, now + 31.0 / 48000.0));
    assert!(ctx.param_set_value_at_time(47, "detune", -600.0, now + 129.0 / 48000.0));
    // Events inside a quantum take effect at the next k-rate block boundary.
    for (block, rate) in [
        0.7312345f32 as f64,
        1.33337f32 as f64,
        1.33337f32 as f64 * 2.0f64.powf(-600.0 / 1200.0),
    ]
    .into_iter()
    .enumerate()
    {
        let expected: Vec<_> = (0..128)
            .flat_map(|_| reference.one_ratio(1.0 / rate))
            .map(|v| 0.0 + v)
            .collect();
        let mut output = [0.0; 256];
        let before = migo_alloc_probe::thread_counts();
        ctx.process(&mut output);
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            (before.allocations, before.reallocations),
            (after.allocations, after.reallocations)
        );
        assert!(
            output
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "native loop automation lost k-rate timing or continuous history"
        );
        save(&format!("web-loop-automation-{block}"), &output, &expected);
    }
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(unsafe { probe_alloc_count() }, automation_allocated);
    #[cfg(migo_native_alloc_probe)]
    let allocated = unsafe { probe_alloc_count() };
    let deny = ledger
        .try_reserve((32 << 20) - ledger.used_bytes(), "retained loop peer")
        .unwrap();
    assert!(ctx.set_loop(47, true, 3.25 / 48000.0, 17.75 / 48000.0));
    assert!(ctx.set_playback_rate(47, 0.25));
    ctx.process(&mut [0.0; 34]);
    assert!(ctx.set_loop(47, true, 3.0 / 48000.0, 17.0 / 48000.0));
    assert!(ctx.set_playback_rate(47, 1.33337));
    ctx.process(&mut [0.0; 34]);
    assert!(ctx.set_loop(47, false, 0.0, 0.0));
    ctx.process(&mut [0.0; 34]);
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        allocated,
        "loop controls reconstructed an existing native owner"
    );
    drop(deny);

    // A fractional loop can run without a native owner under a full ledger.
    // Changing it to an exact integer period must admit before changing bounds.
    assert!(ctx.create_buffer_source(48));
    ctx.connect(48, crate::context::DESTINATION_NODE_ID);
    assert!(ctx.set_buffer(48, Some(buffer)));
    assert!(ctx.set_loop(48, true, 3.25 / 48000.0, 17.75 / 48000.0));
    assert!(ctx.set_playback_rate(48, 0.7312345));
    assert!(ctx.start_source(48, 0.0, 0.0, None));
    let deny = ledger
        .try_reserve((32 << 20) - ledger.used_bytes(), "loop geometry peer")
        .unwrap();
    assert!(
        !ctx.set_loop(48, true, 3.0 / 48000.0, 17.0 / 48000.0),
        "loop geometry bypassed native admission"
    );
    drop(deny);
    assert!(ctx.set_loop(48, true, 3.0 / 48000.0, 17.0 / 48000.0));
}

fn stream_pcm(
    tx: &tokio::sync::mpsc::Sender<StreamMsg>,
    pool: &mut crate::streaming::PcmPool,
    revision: StreamRevision,
    samples: &[f32],
) {
    let mut pcm = pool.take();
    pcm.buffer_mut().extend_from_slice(samples);
    assert!(tx.try_send(StreamMsg::Samples { revision, pcm }).is_ok());
}

#[test]
fn streaming_native_rate_keeps_complete_pcm_through_stalls_eof_and_prefix_reclaim() {
    for (channels, frames) in [(1, 5003), (2, 5003), (32, 5003), (1, 450007)] {
        let (_scheduler, _ledger, _host, mut player) = setup(channels, 1280 + channels as i32);
        let input = signal(frames, channels);
        let (tx, rx) = crate::streaming::stream_channel();
        player.shared.set_playback_rate(1.333);
        player
            .start_streaming("rate".into(), rx, StreamingState::new())
            .unwrap();
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: channels as u32,
        })
        .unwrap();
        let mut pool = crate::streaming::PcmPool::new();
        let mut sent = 4000;
        stream_pcm(
            &tx,
            &mut pool,
            StreamRevision::INITIAL,
            &input[..sent * channels],
        );
        player.poll_stream();
        assert!(
            player.rate_converter.is_some(),
            "stream Ready omitted native rate owner"
        );
        player.play();
        player.drain_events().for_each(drop);
        let mut reference = Reference::new(&input, channels);
        let (mut cursor, mut rendered, mut stalls) = (0u64, 0usize, 0usize);
        let (mut actual, mut expected) = (Vec::new(), Vec::new());
        loop {
            let units = if frames > 10000 {
                1333
            } else {
                [503, 2000, 1333, 500][rendered / 257 % 4]
            };
            player.set_playback_rate(units as f32 / 1000.).unwrap();
            let request = [1, 17, 127, 128, 129, 257][rendered % 6];
            let mut bus = vec![0.125; request * channels + channels - 1];
            let before = migo_alloc_probe::thread_counts();
            #[cfg(migo_native_alloc_probe)]
            let native_before = unsafe { probe_alloc_count() };
            let playing = player.process(&mut bus);
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                (before.allocations, before.reallocations),
                (after.allocations, after.reallocations),
                "stream rate allocated during mix"
            );
            #[cfg(migo_native_alloc_probe)]
            assert_eq!(native_before, unsafe { probe_alloc_count() });
            let count = if playing {
                let next = (player.stream_base_frames + (player.position / channels) as u64) * 1000
                    + u64::from(player.position_phase);
                assert!(
                    next >= cursor && (next - cursor).is_multiple_of(u64::from(units)),
                    "stream rate cursor skipped or lost phase"
                );
                ((next - cursor) / u64::from(units)) as usize
            } else {
                ((frames as u64 * 1000 - cursor).div_ceil(u64::from(units))) as usize
            };
            assert!(count <= request);
            assert!(
                bus[count * channels..]
                    .iter()
                    .all(|v| v.to_bits() == 0.125f32.to_bits()),
                "stream stall overwrote the shared mix or incomplete tail"
            );
            actual.extend_from_slice(&bus[..count * channels]);
            for _ in 0..count {
                expected.extend(reference.one(units).into_iter().map(|v| 0.125 + v));
            }
            cursor += count as u64 * u64::from(units);
            rendered += count;
            let events: Vec<_> = player.drain_events().collect();
            assert!(
                !events
                    .iter()
                    .any(|e| e.event_type == InnerAudioEventType::Error),
                "live SRC rejected ordinary input starvation"
            );
            if !player.stream_complete {
                assert!(
                    cursor < sent as u64 * 1000,
                    "live SRC consumed unavailable future"
                );
            }
            if !playing {
                assert!(player.stream_complete && cursor >= frames as u64 * 1000);
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.event_type == InnerAudioEventType::Ended)
                        .count(),
                    1
                );
                break;
            }
            assert!(
                !events
                    .iter()
                    .any(|e| e.event_type == InnerAudioEventType::Ended)
            );
            if count == 0 {
                stalls += 1;
                assert!(
                    !player.stream_complete,
                    "native live stream stopped making progress at EOF"
                );
                let position = (player.position, player.position_phase);
                let mut untouched = vec![0.125; request * channels];
                assert!(player.process(&mut untouched));
                assert_eq!((player.position, player.position_phase), position);
                assert!(
                    untouched.iter().all(|v| v.to_bits() == 0.125f32.to_bits()),
                    "native stream invented zeros before EOF"
                );
                assert!(
                    player.drain_events().next().is_none(),
                    "repeated stall repeated Waiting"
                );
                if sent == frames {
                    tx.try_send(StreamMsg::Done {
                        revision: StreamRevision::INITIAL,
                    })
                    .unwrap();
                } else {
                    let end = (sent
                        + if frames > 10000 {
                            4097
                        } else {
                            [1, 17, 83, 257][stalls % 4]
                        })
                    .min(frames);
                    stream_pcm(
                        &tx,
                        &mut pool,
                        StreamRevision::INITIAL,
                        &input[sent * channels..end * channels],
                    );
                    sent = end;
                }
            }
            player.poll_stream();
        }
        assert!(stalls > 2);
        assert_eq!(
            actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "stream complete native PCM changed across stalls or reclaimed prefix"
        );
        if frames > 10000 {
            assert!(
                player.stream_windowed && player.stream_base_frames > 0,
                "long stream did not exercise prefix reclaim"
            );
        }
        save(&format!("stream-{channels}-{frames}"), &actual, &expected);
    }
}

#[test]
fn streaming_rate_activation_revision_and_refusal_preserve_native_state_and_inputs() {
    let (_scheduler, ledger, _host, mut player) = setup(2, 1317);
    let input = signal(5003, 2);
    let (tx, rx) = crate::streaming::stream_channel();
    let state = StreamingState::new_reusable();
    player
        .start_streaming("controls".into(), rx, state.clone())
        .unwrap();
    tx.try_send(StreamMsg::Ready {
        revision: StreamRevision::INITIAL,
        sample_rate: 8000,
        channels: 2,
    })
    .unwrap();
    let mut pool = crate::streaming::PcmPool::new();
    stream_pcm(&tx, &mut pool, StreamRevision::INITIAL, &input);
    player.poll_stream();
    player.play();
    player.drain_events().for_each(drop);
    assert!(player.process(&mut [0.; 1006]));
    let old = (
        player.position,
        player.position_phase,
        player.source.samples().as_ptr(),
        ledger.used_bytes(),
    );
    let deny = ledger
        .try_reserve((32 << 20) - old.3, "stream saturation")
        .unwrap();
    #[cfg(migo_native_alloc_probe)]
    let allocations = unsafe { probe_alloc_count() };
    assert!(
        player.set_playback_rate(1.333).is_err(),
        "stream rate bypassed native preadmission"
    );
    assert_eq!(player.shared.playback_rate_units(), 1000);
    assert_eq!(
        (
            player.position,
            player.position_phase,
            player.source.samples().as_ptr()
        ),
        (old.0, old.1, old.2)
    );
    assert_eq!(player.source.samples(), input);
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        allocations,
        unsafe { probe_alloc_count() },
        "stream constructed native before admission"
    );
    drop(deny);
    player.set_playback_rate(1.333).unwrap();
    let owner = (&**player.rate_converter.as_ref().unwrap()) as *const _;
    let mut oracle = Reference::new(&input, 2);
    for _ in 0..503 {
        oracle.one(1000);
    }
    let mut output = [0.; 514];
    assert!(player.process(&mut output));
    let expected: Vec<_> = (0..257).flat_map(|_| oracle.one(1333)).collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "stream activation lost actual past or phase"
    );
    save("stream-activation", &output, &expected);
    player.drain_events().for_each(drop);
    let _deny = ledger
        .try_reserve(
            (32 << 20) - ledger.used_bytes(),
            "stream revision saturation",
        )
        .unwrap();
    #[cfg(migo_native_alloc_probe)]
    let allocations = unsafe { probe_alloc_count() };
    let revision = StreamRevision::INITIAL.checked_next().unwrap();
    player
        .accept_stream_revision(&state, revision, 131)
        .unwrap();
    stream_pcm(&tx, &mut pool, revision, &input[262..296]);
    player.poll_stream();
    let mut empty = [0.125; 514];
    assert!(player.process(&mut empty));
    assert!(
        empty.iter().all(|v| v.to_bits() == 0.125f32.to_bits()),
        "stream revision retained pre-seek native lookahead"
    );
    assert_eq!((player.position, player.position_phase), (0, 0));
    stream_pcm(&tx, &mut pool, revision, &input[296..]);
    tx.try_send(StreamMsg::Done { revision }).unwrap();
    player.poll_stream();
    let mut oracle = Reference::new(&input[262..], 2);
    let mut output = [0.; 514];
    assert!(player.process(&mut output));
    let expected: Vec<_> = (0..257).flat_map(|_| oracle.one(1333)).collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "stream revision reused stale native PCM or history"
    );
    assert_eq!(
        (&**player.rate_converter.as_ref().unwrap()) as *const _,
        owner
    );
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        allocations,
        unsafe { probe_alloc_count() },
        "stream revision rebuilt native under full budget"
    );
    save("stream-revision", &output, &expected);
    let frame = player.position / 2;
    let phase = player.position_phase;
    player.set_loop_enabled(true);
    let mut oracle = Reference::new(&input[262..].repeat(4), 2);
    for _ in 0..frame - usize::from(phase != 0) {
        oracle.one(1000);
    }
    if phase != 0 {
        oracle.one(1000 + u32::from(phase));
    }
    let mut output = [0.; 514];
    assert!(player.process(&mut output));
    let expected: Vec<_> = (0..257).flat_map(|_| oracle.one(1333)).collect();
    assert_eq!(
        output.map(f32::to_bits).as_slice(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "completed stream loop reset kept deferred native warmup"
    );
    save("stream-completed-loop", &output, &expected);
}
#[test]
fn buffered_native_rate_matches_complete_pcm_and_exact_clock_without_hot_allocations() {
    for (channels, output_channels) in [(1, 1), (2, 2), (32, 32), (1, 2), (2, 1), (32, 2)] {
        let (_scheduler, _ledger, _host, mut player) = setup(channels, 1200 + channels as i32);
        player.output_channels = output_channels;
        let input = signal(4099, channels);
        for rates in [
            &[503][..],
            &[1333][..],
            &[2000][..],
            &[503, 2000, 1333, 500][..],
        ] {
            player.shared.set_playback_rate(rates[0] as f32 / 1000.);
            load(&mut player, &input, channels);
            player.play();
            player.drain_events().for_each(drop);
            let mut reference = Reference::new(&input, channels);
            let (mut cursor, mut written, mut ended) = (0u64, 0usize, 0usize);
            let (mut actual, mut expected) = (Vec::new(), Vec::new());
            while cursor < 4099 * 1000 {
                let units = rates[(written / 128) % rates.len()];
                player.set_playback_rate(units as f32 / 1000.).unwrap();
                let n = [1, 17, 127, 128, 129, 257][written % 6]
                    .min(128 - written % 128)
                    .min(((4099 * 1000 - cursor).div_ceil(u64::from(units))) as usize);
                let tail = output_channels as usize - 1;
                let mut bus = vec![0.125; n * output_channels as usize + tail];
                let before = migo_alloc_probe::thread_counts();
                #[cfg(migo_native_alloc_probe)]
                let c_before = unsafe { probe_alloc_count() };
                let playing = player.process(&mut bus);
                let after = migo_alloc_probe::thread_counts();
                assert_eq!(
                    (after.allocations, after.reallocations),
                    (before.allocations, before.reallocations),
                    "native InnerAudio allocated in mix"
                );
                #[cfg(migo_native_alloc_probe)]
                assert_eq!(
                    unsafe { probe_alloc_count() },
                    c_before,
                    "native InnerAudio C allocated in mix"
                );
                assert!(
                    bus[n * output_channels as usize..]
                        .iter()
                        .all(|v| v.to_bits() == 0.125f32.to_bits()),
                    "native InnerAudio overwrote incomplete output tail"
                );
                bus.truncate(n * output_channels as usize);
                actual.extend(bus);
                for _ in 0..n {
                    let frame = reference.one(units);
                    if channels == 1 {
                        expected.extend(std::iter::repeat_n(
                            0.125 + frame[0],
                            output_channels as usize,
                        ));
                    } else if output_channels == 1 {
                        expected.push(0.125 + frame.iter().copied().sum::<f32>() / channels as f32);
                    } else {
                        expected.extend(
                            (0..output_channels as usize)
                                .map(|c| 0.125 + frame.get(c).copied().unwrap_or(0.)),
                        );
                    }
                    cursor += u64::from(units);
                }
                written += n;
                assert_eq!(
                    playing,
                    cursor < 4099 * 1000,
                    "native media endpoint changed"
                );
                if playing {
                    assert_eq!(player.position / channels, (cursor / 1000) as usize);
                    assert_eq!(u64::from(player.position_phase), cursor % 1000);
                }
                for event in player.drain_events() {
                    if matches!(event.event_type, InnerAudioEventType::Ended) {
                        ended += 1;
                    }
                }
            }
            assert_eq!(ended, 1);
            assert_eq!(player.shared.state(), PlaybackState::Stopped);
            assert_eq!(player.shared.position_frames(), 0);
            assert!(
                actual
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "production native complete PCM changed"
            );
            save(
                &format!(
                    "{channels}-{output_channels}-{}",
                    rates
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("_")
                ),
                &actual,
                &expected,
            );
        }
    }
}

#[test]
fn activation_fractional_phase_seek_stop_and_replay_use_the_real_player() {
    let (_scheduler, _ledger, _host, mut player) = setup(2, 1250);
    let input = signal(4099, 2);
    load(&mut player, &input, 2);
    assert!(
        player.rate_converter.is_none(),
        "default unity allocated native state"
    );
    player.play();
    let mut unity = [0.; 34];
    player.process(&mut unity);
    assert_eq!(unity.as_slice(), &input[..34]);
    player.set_playback_rate(0.503).unwrap();
    let mut output = [0.; 22];
    player.process(&mut output);
    player.set_playback_rate(1.).unwrap();
    assert!(player.rate_converter.is_none());
    player.process(&mut [0.; 6]);
    let frame = player.position / 2;
    let phase = player.position_phase;
    assert_ne!(phase, 0);
    player.set_playback_rate(1.333).unwrap();
    let mut reference = Reference::new(&input, 2);
    for _ in 0..frame - 1 {
        reference.one(1000);
    }
    reference.one(1000 + u32::from(phase));
    let expected = (0..128)
        .flat_map(|_| reference.one(1333))
        .collect::<Vec<_>>();
    let mut actual = vec![0.; 256];
    player.process(&mut actual);
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "native activation lost existing fractional phase"
    );
    save("activation-phase", &actual, &expected);
    for target in [0, 1024, 17] {
        player.seek(target as f64 / 48000.);
        let mut reference = Reference::new(&input, 2);
        for _ in 0..target {
            reference.one(1000);
        }
        let expected = (0..128)
            .flat_map(|_| reference.one(1333))
            .collect::<Vec<_>>();
        actual.fill(0.);
        player.process(&mut actual);
        assert!(
            actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "native seek retained wrong history"
        );
        save(&format!("seek-{target}"), &actual, &expected);
    }
    player.stop();
    player.play();
    let mut reference = Reference::new(&input, 2);
    let expected = (0..128)
        .flat_map(|_| reference.one(1333))
        .collect::<Vec<_>>();
    actual.fill(0.);
    player.process(&mut actual);
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "native stop/replay retained wrong state"
    );
    save("replay", &actual, &expected);
    player.stop();
    player.set_playback_rate(0.503).unwrap();
    player.play();
    player.process(&mut [0.; 2]);
    assert_eq!((player.position, player.position_phase), (0, 503));
    player.set_playback_rate(1.).unwrap();
    player.set_playback_rate(1.333).unwrap();
    let mut padded = vec![0.; 2];
    padded.extend_from_slice(&input);
    let mut reference = Reference::new(&padded, 2);
    reference.one(1503);
    let expected = (0..128)
        .flat_map(|_| reference.one(1333))
        .collect::<Vec<_>>();
    actual.fill(0.);
    player.process(&mut actual);
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "native activation lost frame-zero phase"
    );
    save("activation-frame-zero-phase", &actual, &expected);
    player.set_loop_enabled(true);
    assert!(
        player.rate_converter.is_some(),
        "buffered loop discarded admitted native state"
    );
}

#[test]
fn buffered_native_loops_keep_complete_periodic_pcm_phase_and_hot_bounds() {
    for (frames, channels) in [(1, 1), (3, 2), (17, 32), (129, 2)] {
        let (_scheduler, _ledger, _host, mut player) = setup(channels, 1300 + channels as i32);
        let mut input = signal(frames, channels);
        input[0] = 0.375;
        for rates in [
            &[503][..],
            &[1333][..],
            &[2000][..],
            &[503, 2000, 1333, 500][..],
        ] {
            player.shared.set_playback_rate(rates[0] as f32 / 1000.);
            player.set_loop_enabled(true);
            load(&mut player, &input, channels);
            player.play();
            player.drain_events().for_each(drop);
            assert!(
                player.rate_converter.is_some(),
                "buffered loop did not prepare native rate"
            );
            // Expand a complete independent periodic source, then use the raw
            // whole-input/one-output oracle. No production wrap code is reused.
            let repeated = input.repeat((2049usize * 2 + 8192).div_ceil(frames));
            let mut reference = Reference::new(&repeated, channels);
            let (mut written, mut clock) = (0, 0u64);
            let (mut actual, mut expected) = (Vec::new(), Vec::new());
            while written < 2049 {
                let units = rates[(written / 128) % rates.len()];
                player.set_playback_rate(units as f32 / 1000.).unwrap();
                let n = (2049 - written)
                    .min(128 - written % 128)
                    .min([1, 17, 127, 129, 257][written % 5]);
                let mut output = vec![0.125; n * channels];
                let before = migo_alloc_probe::thread_counts();
                #[cfg(migo_native_alloc_probe)]
                let c_before = unsafe { probe_alloc_count() };
                assert!(
                    player.process(&mut output),
                    "native loop ended at source seam"
                );
                let after = migo_alloc_probe::thread_counts();
                assert_eq!(
                    (before.allocations, before.reallocations),
                    (after.allocations, after.reallocations),
                    "native loop allocated in mix"
                );
                #[cfg(migo_native_alloc_probe)]
                assert_eq!(
                    unsafe { probe_alloc_count() },
                    c_before,
                    "native loop C allocated in mix"
                );
                actual.extend(output);
                for _ in 0..n {
                    expected.extend(reference.one(units).into_iter().map(|v| 0.125 + v));
                    clock += u64::from(units);
                }
                written += n;
                assert_eq!(
                    player.position / channels,
                    ((clock / 1000) % (frames as u64)) as usize
                );
                assert_eq!(u64::from(player.position_phase), clock % 1000);
                assert_eq!(player.buffered_loops, (clock / 1000) as usize / frames);
                assert!(
                    !player
                        .drain_events()
                        .any(|event| matches!(event.event_type, InnerAudioEventType::Ended)),
                    "native loop emitted ended"
                );
            }
            assert!(
                actual
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "native loop complete periodic PCM changed"
            );
            save(
                &format!(
                    "loop-{frames}-{channels}-{}",
                    rates
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("_")
                ),
                &actual,
                &expected,
            );
        }
    }
    // Cross native buffer compaction while the retained application source is
    // only 17 frames. Every output sample is checked, including the full tail.
    let (_scheduler, _ledger, _host, mut player) = setup(2, 1340);
    let input = signal(17, 2);
    let rates = [503, 2000, 1333, 500];
    player.shared.set_playback_rate(0.503);
    player.set_loop_enabled(true);
    load(&mut player, &input, 2);
    player.play();
    player.drain_events().for_each(drop);
    let repeated = input.repeat((131137usize * 2 + 8192).div_ceil(17));
    let mut reference = Reference::new(&repeated, 2);
    let (mut actual, mut expected) = (Vec::new(), Vec::new());
    let mut written = 0;
    while written < 131137 {
        let n = (131137 - written).min(128);
        let units = rates[(written / 128) % rates.len()];
        player.set_playback_rate(units as f32 / 1000.).unwrap();
        let mut block = vec![0.; n * 2];
        assert!(player.process(&mut block));
        actual.extend(block);
        for _ in 0..n {
            expected.extend(reference.one(units));
        }
        written += n;
        player.drain_events().for_each(drop);
    }
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "native long loop lost retained history"
    );
    save("loop-long-compaction", &actual, &expected);
}

#[test]
fn buffered_loop_controls_rebuild_only_real_past_without_reallocating() {
    let (_scheduler, ledger, _host, mut player) = setup(2, 1350);
    let mut input = signal(3, 2);
    input[0] = 0.375;
    player.set_loop_enabled(true);
    load(&mut player, &input, 2);
    player.play();
    player.process(&mut [0.; 14]);
    assert_eq!((player.buffered_loops, player.position / 2), (2, 1));
    player.set_playback_rate(0.503).unwrap();
    let mut reference = Reference::new(&input.repeat(4096), 2);
    for _ in 0..7 {
        reference.one(1000);
    }
    let mut actual = vec![0.; 256];
    let expected = (0..128)
        .flat_map(|_| reference.one(503))
        .collect::<Vec<_>>();
    player.process(&mut actual);
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "loop activation fabricated unplayed past"
    );
    save("loop-activation-after-unity", &actual, &expected);
    // Repeating the property must keep the live filter, including its phase.
    player.set_loop_enabled(true);
    actual.fill(0.);
    let expected = (0..128)
        .flat_map(|_| reference.one(503))
        .collect::<Vec<_>>();
    player.process(&mut actual);
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "same loop setting reset native history"
    );
    let absolute = player.buffered_loops * 3 + player.position / 2;
    let phase = player.position_phase;
    let extent = (player.buffered_loops + 1) * 3;
    let mut reference = Reference::new(&input.repeat(player.buffered_loops + 1), 2);
    for _ in 0..absolute - usize::from(phase != 0) {
        reference.one(1000);
    }
    if phase != 0 {
        reference.one(1000 + u32::from(phase));
    }
    let frames = ((extent - absolute) * 1000 - usize::from(phase)).div_ceil(503);
    let mut expected = vec![0.125; 34];
    for chunk in expected[..frames * 2].chunks_mut(2) {
        for (v, s) in chunk.iter_mut().zip(reference.one(503)) {
            *v += s;
        }
    }
    let pointer = std::ptr::from_ref(&**player.rate_converter.as_ref().unwrap());
    let used = ledger.used_bytes();
    let _deny = ledger
        .try_reserve((32 << 20) - used, "loop control saturation")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    #[cfg(migo_native_alloc_probe)]
    let c_before = unsafe { probe_alloc_count() };
    player.set_loop_enabled(false);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (before.allocations, before.reallocations),
        (after.allocations, after.reallocations),
        "loop control allocated replacement state"
    );
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        c_before,
        "loop control C allocated replacement state"
    );
    assert_eq!(
        pointer,
        std::ptr::from_ref(&**player.rate_converter.as_ref().unwrap())
    );
    assert_eq!(ledger.used_bytes(), 32 << 20);
    let mut actual = vec![0.125; 34];
    assert!(!player.process(&mut actual));
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "loop disable kept wrapped future or lost actual past"
    );
    save("loop-disable-tail", &actual, &expected);
    drop(_deny);
    let input = signal(17, 2);
    load(&mut player, &input, 2);
    player.set_playback_rate(2.).unwrap();
    player.play();
    player.process(&mut [0.; 14]);
    assert_eq!(player.position / 2, 14);
    player.set_loop_enabled(true);
    let mut reference = Reference::new(&input.repeat(1024), 2);
    for _ in 0..14 {
        reference.one(1000);
    }
    let expected = (0..4097)
        .flat_map(|_| reference.one(2000))
        .collect::<Vec<_>>();
    let mut actual = vec![0.; 4097 * 2];
    assert!(player.process(&mut actual));
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "loop enable retained zero-padded future"
    );
    save("loop-enable-long-block", &actual, &expected);
    player.seek(1. / 48000.);
    assert_eq!(player.buffered_loops, 0);
    let mut reference = Reference::new(&input.repeat(1024), 2);
    reference.one(1000);
    let expected = (0..128)
        .flat_map(|_| reference.one(2000))
        .collect::<Vec<_>>();
    let mut actual = vec![0.; 256];
    assert!(player.process(&mut actual));
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "loop seek retained old traversal history"
    );
    save("loop-seek", &actual, &expected);
    player.stop();
    player.play();
    let mut reference = Reference::new(&input.repeat(1024), 2);
    let expected = (0..128)
        .flat_map(|_| reference.one(2000))
        .collect::<Vec<_>>();
    actual.fill(0.);
    assert!(player.process(&mut actual));
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "loop stop/replay retained old traversal history"
    );
    save("loop-replay", &actual, &expected);
}

fn wait(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "native retirement did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Gate {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
struct Block {
    gate: Arc<(Mutex<bool>, Condvar)>,
    worker: Arc<Mutex<Option<std::thread::ThreadId>>>,
}
impl Drop for Block {
    fn drop(&mut self) {
        *self.worker.lock().unwrap() = Some(std::thread::current().id());
        let mut open = self.gate.0.lock().unwrap();
        while !*open {
            open = self.gate.1.wait(open).unwrap();
        }
    }
}
#[test]
fn native_admission_preserves_refused_inputs_and_original_cpu_keeps_credit_through_drop() {
    let (scheduler, ledger, host, mut player) = setup(2, 1260);
    let input = signal(17, 2);
    load(&mut player, &input, 2);
    let baseline = ledger.used_bytes();
    let deny = ledger
        .try_reserve((32 << 20) - baseline, "test saturation")
        .unwrap();
    let position = player.position;
    #[cfg(migo_native_alloc_probe)]
    let before_refusal = unsafe { probe_alloc_count() };
    assert!(
        player.set_playback_rate(1.333).is_err(),
        "native playbackRate bypassed physical admission"
    );
    #[cfg(migo_native_alloc_probe)]
    assert_eq!(
        unsafe { probe_alloc_count() },
        before_refusal,
        "native constructor ran before admission"
    );
    assert_eq!(player.shared.playback_rate(), 1.);
    assert_eq!(player.position, position);
    assert!(player.rate_converter.is_none());
    drop(deny);
    #[cfg(migo_native_alloc_probe)]
    let (allocations, requested) = unsafe { (probe_alloc_count(), probe_requested_bytes()) };
    player.set_playback_rate(1.333).unwrap();
    let charged = ledger.used_bytes();
    let empty = host
        .reserve_value::<crate::variable_rate::Converter>()
        .unwrap();
    let overhead = ledger.used_bytes() - charged;
    drop(empty);
    let bytes = unsafe { migo_src_required_bytes(2) };
    #[cfg(migo_native_alloc_probe)]
    unsafe {
        assert_eq!(
            probe_alloc_count() - allocations,
            3,
            "native constructor observer was not active"
        );
        assert_eq!(
            probe_requested_bytes() - requested,
            bytes,
            "native capacity query disagreed with actual calloc requests"
        );
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(
        bytes, 880228,
        "native capacity differs from pinned observed calloc requests"
    );
    assert_eq!(
        charged - baseline,
        bytes + overhead,
        "native retained capacity omitted from ledger"
    );
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let worker = Arc::new(Mutex::new(None));
    let value = Block {
        gate: gate.0.clone(),
        worker: worker.clone(),
    };
    let retired = scheduler.audio_cpu().retirement();
    crate::streaming::get_stream_runtime().spawn(async move { retired.retire(value).await });
    wait(|| worker.lock().unwrap().is_some());
    #[cfg(migo_native_alloc_probe)]
    let native_frees = Arc::new(NativeFrees {
        count: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        wrong: AtomicBool::new(false),
        worker: worker.clone(),
        ledger: ledger.clone(),
        charged,
    });
    #[cfg(migo_native_alloc_probe)]
    unsafe {
        probe_watch_constructed(native_freed, Arc::as_ptr(&native_frees).cast_mut().cast());
    }
    #[cfg(migo_native_alloc_probe)]
    let actual_frees = native_frees.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();
    let account = ledger.clone();
    player
        .rate_converter
        .as_mut()
        .unwrap()
        .observe(Arc::new(move || {
            assert_eq!(
                Some(std::thread::current().id()),
                *worker.lock().unwrap(),
                "native destructor escaped original CPU"
            );
            assert_eq!(
                account.used_bytes(),
                charged,
                "native credit returned before destructor"
            );
            #[cfg(migo_native_alloc_probe)]
            {
                assert_eq!(
                    actual_frees.count.load(Ordering::Acquire),
                    3,
                    "native original pointers were not actually freed"
                );
                assert_eq!(actual_frees.bytes.load(Ordering::Acquire), bytes);
                assert!(
                    !actual_frees.wrong.load(Ordering::Acquire),
                    "native free escaped original CPU or returned credit early"
                );
            }
            observed.fetch_add(1, Ordering::Release);
        }));
    let before = migo_alloc_probe::thread_counts();
    player.set_playback_rate(1.).unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        before.allocations, after.allocations,
        "native retirement allocated on caller"
    );
    assert_eq!(
        seen.load(Ordering::Acquire),
        0,
        "native state destroyed on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        charged,
        "native credit returned before CPU destruction"
    );
    scheduler.close();
    drop(gate);
    wait(|| seen.load(Ordering::Acquire) == 1 && ledger.used_bytes() == baseline);
    // A closed original host refuses the next state without changing the rate.
    assert!(player.set_playback_rate(1.333).is_err());
    assert_eq!(player.shared.playback_rate(), 1.);
    player.shared.set_playback_rate(1.333);
    let token = player.begin_load().unwrap();
    let mut incoming = DecodedAudio {
        samples: Vec::with_capacity(4096),
        sample_rate: 48000,
        channels: 2,
    };
    incoming.samples.extend(input);
    let pointer = incoming.samples.as_ptr();
    let bits = incoming
        .samples
        .iter()
        .map(|v| v.to_bits())
        .collect::<Vec<_>>();
    let mut physical = Some(ledger.try_reserve(4096 * 4, "incoming").unwrap());
    let retained = ledger.used_bytes();
    assert!(
        player
            .try_complete_load_charged(&token, &mut incoming, &mut physical)
            .is_err()
    );
    assert_eq!(incoming.samples.as_ptr(), pointer);
    assert_eq!(incoming.samples.capacity(), 4096);
    assert_eq!(
        incoming
            .samples
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert!(physical.is_some());
    assert_eq!(ledger.used_bytes(), retained);
    assert!(!player.shared.is_loaded());
}
