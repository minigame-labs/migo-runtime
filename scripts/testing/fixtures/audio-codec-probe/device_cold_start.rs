//! First-use graphs used to be absent from the prewarmed device churn probe.
//! Drive the actual AudioThread contribution and retain every callback span so
//! setup latency cannot hide missing PCM, ended notifications or owner credit.
use super::*;
use crate::nodes::AudioNodeProcessor;
use migo_alloc_probe::thread_counts;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const FRAMES: usize = 2048;
const VOICES: usize = 32;
const CALLBACK: usize = 128;

struct Trace {
    spans: Vec<(u64, u64)>,
    output: [f32; CALLBACK * 2],
}

struct HostRun {
    scheduler: migo_io::scheduler::IoScheduler,
    mixer: crate::device_prepare::DeviceMixer,
    contexts: HashMap<u32, AudioContext>,
    index: NodeContextIndex,
    order: Vec<u32>,
    traces: Vec<Trace>,
    ended: Vec<u32>,
    source_ids: Vec<u32>,
    retire_started: Arc<AtomicUsize>,
    retired: Arc<AtomicUsize>,
    bad_thread: Arc<AtomicBool>,
    first_output_ns: Option<u128>,
}

fn waveform(frames: usize) -> Vec<f32> {
    (0..frames * 2)
        .map(|i| ((i % 127) as i32 - 63) as f32 / 4096.0)
        .collect()
}

struct SourceConfig<'a> {
    frames: usize,
    kind: &'a str,
    start: f64,
    stop: Option<f64>,
}

fn host(
    number: u32,
    count: usize,
    rate: u32,
    automation: bool,
    waveshaper: usize,
    source: SourceConfig<'_>,
) -> HostRun {
    let SourceConfig {
        frames,
        kind: source_kind,
        start,
        stop,
    } = source;
    let scheduler = migo_io::scheduler::IoScheduler::new(number as i32);
    let mut mixer = crate::device_prepare::DeviceMixer::new(scheduler.audio_cpu());
    let retire_started = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let bad_thread = Arc::new(AtomicBool::new(false));
    mixer.cold_probe_retirement(retire_started.clone(), retired.clone(), bad_thread.clone());
    let mut contexts = HashMap::new();
    let mut index = NodeContextIndex::new();
    let mut source_ids = Vec::new();
    for n in 0..count {
        let id = number * 100 + n as u32 + 1;
        let mut context = AudioContext::new(id, rate, 2);
        context
            .prepare_node_retirement(
                scheduler.audio_cpu(),
                crate::limits::AudioAggregateLedger::process_global(),
            )
            .unwrap();
        let buffer = (source_kind == "buffer").then(|| {
            context
                .add_buffer(DecodedAudio {
                    sample_rate: rate,
                    channels: 2,
                    samples: waveform(frames),
                })
                .unwrap()
        });
        let master = id * 1000 + 900;
        context.create_gain(master);
        context.set_gain(master, 0.5);
        let mut last = master;
        if automation {
            context.param_set_value_at_time(master, "gain", 0.5, 0.0);
            context.param_linear_ramp(master, "gain", 0.75, 1024.0 / rate as f64);
            let delay = master + 1;
            context.add_node(Box::new(crate::nodes::DelayNode::new(
                delay, 0.001, rate, 2,
            )));
            context.param_set_value_at_time(delay, "delayTime", 0.0, 0.0);
            context.connect(master, delay);
            last = delay;
        }
        if waveshaper != 0 {
            let shaper = master + 2;
            let mut node = crate::nodes::WaveShaperNode::new(shaper);
            node.set_curve(Some(vec![-1.0, -1.0, 0.0, 1.0, 1.0]));
            context.add_node(Box::new(node));
            assert!(context.set_wave_shaper_oversample(shaper, shaper_mode(waveshaper)));
            context.connect(last, shaper);
            last = shaper;
        }
        context.connect(last, crate::context::DESTINATION_NODE_ID);
        for voice in 0..VOICES as u32 {
            let source = id * 1000 + voice * 2 + 1;
            let gain = source + 1;
            if source_kind == "buffer" {
                context.create_buffer_source(source);
                context.set_buffer(source, buffer);
                context.start_source(source, start / rate as f64, 0.0, None);
                if let Some(stop) = stop {
                    context.stop_source(source, stop / rate as f64);
                }
            } else if source_kind == "constant" {
                let mut node = crate::nodes::ConstantSourceNode::new(source);
                node.start(start / rate as f64);
                node.stop(stop.unwrap_or(frames as f64) / rate as f64);
                context.add_node(Box::new(node));
                context.param_set_value_at_time(source, "offset", 0.25, 0.0);
                context.param_linear_ramp(source, "offset", 0.75, frames as f64 / rate as f64);
            } else {
                let mut node = crate::nodes::OscillatorNode::new(source, rate);
                node.get_param_mut("frequency").unwrap().set_value(333.0);
                node.start(start / rate as f64);
                node.stop(stop.unwrap_or(frames as f64) / rate as f64);
                context.add_node(Box::new(node));
                if source_kind != "oscillator-detune" {
                    context.param_set_value_at_time(source, "frequency", 333.0, 0.0);
                    context.param_linear_ramp(
                        source,
                        "frequency",
                        777.0,
                        frames as f64 / rate as f64,
                    );
                }
                if source_kind != "oscillator-frequency" {
                    context.param_set_value_at_time(source, "detune", 0.0, 0.0);
                    context.param_linear_ramp(
                        source,
                        "detune",
                        1200.0,
                        frames as f64 / rate as f64,
                    );
                }
            }
            context.watch_source_ended(source, true);
            context.create_gain(gain);
            context.set_gain(gain, 1.0 / VOICES as f32);
            context.connect(source, gain);
            context.connect(gain, master);
            index.register(source, id);
            source_ids.push(source);
        }
        context.resume();
        contexts.insert(id, context);
    }
    let order = contexts.keys().copied().collect();
    HostRun {
        scheduler,
        mixer,
        contexts,
        index,
        order,
        traces: Vec::new(),
        ended: Vec::with_capacity(count * VOICES),
        source_ids,
        retire_started,
        retired,
        bad_thread,
        first_output_ns: None,
    }
}

fn shaper_mode(factor: usize) -> crate::nodes::OversampleType {
    match factor {
        2 => crate::nodes::OversampleType::TwoX,
        4 => crate::nodes::OversampleType::FourX,
        _ => panic!("invalid waveshaper probe mode"),
    }
}

#[test]
fn first_use_large_graph_preserves_complete_pcm_events_and_closed_host_credit() {
    let hosts: usize = std::env::var("MIGO_COLD_HOSTS").unwrap().parse().unwrap();
    let contexts: usize = std::env::var("MIGO_COLD_CONTEXTS")
        .unwrap()
        .parse()
        .unwrap();
    let rate: u32 = std::env::var("MIGO_COLD_RATE").unwrap().parse().unwrap();
    let source_kind = std::env::var("MIGO_COLD_SOURCE").unwrap_or_else(|_| "buffer".to_owned());
    assert!(matches!(
        source_kind.as_str(),
        "buffer" | "constant" | "oscillator-frequency" | "oscillator-detune" | "oscillator-both"
    ));
    let source_frames: usize = std::env::var("MIGO_COLD_FRAMES")
        .unwrap_or_else(|_| FRAMES.to_string())
        .parse()
        .unwrap();
    assert!((1..=16384).contains(&source_frames));
    let start_frame: f64 = std::env::var("MIGO_COLD_START")
        .unwrap_or_else(|_| "0".to_owned())
        .parse()
        .unwrap();
    let stop_frame: Option<f64> = std::env::var("MIGO_COLD_STOP")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap());
    assert!(start_frame.is_finite() && (0.0..=16384.0).contains(&start_frame));
    assert!(stop_frame.is_none_or(|s| s.is_finite() && (0.0..=32768.0).contains(&s)));
    let stop_frame =
        stop_frame.or_else(|| (source_kind != "buffer").then_some(source_frames as f64));
    let silent = stop_frame.is_some_and(|s| s <= start_frame.ceil());
    let automation = std::env::var("MIGO_COLD_AUTOMATION").is_ok_and(|v| v == "1");
    let waveshaper: usize = std::env::var("MIGO_COLD_WAVESHAPER")
        .unwrap_or_else(|_| "0".to_owned())
        .parse()
        .unwrap();
    assert!(matches!(waveshaper, 0 | 2 | 4));
    assert!(matches!((hosts, contexts), (1, 1) | (4, 8)));
    assert!(matches!(rate, 44_100 | 48_000));
    for cycle in 0..2 {
        // First cycle includes first process executor/runtime use. The second
        // reuses those services, but builds new graphs and filter owners.
        let origin = Instant::now();
        let mut runs: Vec<_> = (0..hosts)
            .map(|n| {
                host(
                    n as u32,
                    contexts,
                    rate,
                    automation,
                    waveshaper,
                    SourceConfig {
                        frames: source_frames,
                        kind: &source_kind,
                        start: start_frame,
                        stop: stop_frame,
                    },
                )
            })
            .collect();
        let graph_setup_ns = origin.elapsed().as_nanos();
        let mut callback_ns = Vec::new();
        let mut mix_allocations = 0;
        let mut mix_reallocations = 0;
        let mut peak_owners = 0;
        let mut peak_aggregate = 0;
        let mut all_ready_ns = None;
        let wakeup = ThreadWakeup::new();
        let mut next = Instant::now();
        let mut resyncs = 0;
        loop {
            assert!(
                origin.elapsed() < Duration::from_secs(20),
                "cold graph did not finish"
            );
            for run in &mut runs {
                run.mixer.service(
                    &mut run.contexts,
                    48_000,
                    false,
                    &wakeup,
                    |id| run.index.unregister(id),
                    |id| run.ended.push(id),
                );
            }
            let owners = runs[0].mixer.cold_probe_usage();
            peak_owners = peak_owners.max(owners.1);
            peak_aggregate = peak_aggregate.max(owners.2);
            if all_ready_ns.is_none()
                && runs
                    .iter()
                    .all(|run| rate == 48_000 || run.mixer.cold_probe_active() == contexts)
            {
                all_ready_ns = Some(origin.elapsed().as_nanos());
            }
            for run in &mut runs {
                let before: Vec<_> = run
                    .order
                    .iter()
                    .map(|id| run.mixer.cold_probe_position(&run.contexts[id]))
                    .collect();
                let mut output = [0.125; CALLBACK * 2];
                let allocations = thread_counts();
                let start = Instant::now();
                mix_contexts(
                    &mut run.contexts,
                    &mut run.index,
                    &mut run.mixer,
                    &mut output,
                    48_000,
                    2,
                    |id| run.ended.push(id),
                );
                let after = thread_counts();
                // The metrics vector is harness work, outside the measured
                // production allocation window.
                callback_ns.push(start.elapsed().as_nanos());
                mix_allocations += after.allocations - allocations.allocations;
                mix_reallocations += after.reallocations - allocations.reallocations;
                let spans = run
                    .order
                    .iter()
                    .zip(before)
                    .map(|(id, from)| {
                        let to = run.mixer.cold_probe_position(&run.contexts[id]);
                        assert!(
                            to >= from && to - from <= CALLBACK as u64,
                            "cold publication exceeded callback"
                        );
                        (from, to)
                    })
                    .collect();
                if run.first_output_ns.is_none() && output.iter().any(|value| *value != 0.125) {
                    run.first_output_ns = Some(origin.elapsed().as_nanos());
                }
                run.traces.push(Trace { spans, output });
            }
            if runs.iter().all(|run| {
                !run.contexts.values().any(AudioContext::has_active_sources)
                    && !run.mixer.has_output_work(&run.contexts, 48_000)
            }) {
                break;
            }
            next += Duration::from_nanos(1_000_000_000 * CALLBACK as u64 / 48_000);
            let now = Instant::now();
            if now > next + Duration::from_millis(20) {
                resyncs += 1;
                next = now;
            }
            std::thread::sleep(next.saturating_duration_since(now));
        }
        let playback_ns = origin.elapsed().as_nanos();
        assert_eq!(
            mix_allocations + mix_reallocations,
            0,
            "cold production context mix allocated"
        );
        assert!(
            all_ready_ns.is_some(),
            "cold graph never admitted every route"
        );
        let expected_owners = if rate == 48_000 { 0 } else { hosts * contexts };
        assert_eq!(
            peak_owners, expected_owners,
            "cold graph owner admission changed"
        );
        // Compute the reference only after the timed first-use interval. Input
        // gains and voice sums are exact dyadic arithmetic; no graph runs here.
        let material = waveform(source_frames);
        let reference_frames = (source_frames + start_frame.ceil() as usize + 128)
            .max(stop_frame.unwrap_or(0.0).ceil() as usize + 128);
        let mut input = Vec::with_capacity(reference_frames * 2);
        let mut cursor = None;
        let mut phase = None;
        for frame in 0..reference_frames {
            let time = frame as f64 / rate as f64;
            let active = time >= start_frame / rate as f64
                && stop_frame.is_none_or(|s| time < s / rate as f64);
            let mut samples = [0.0; 2];
            if active && source_kind == "buffer" {
                let position =
                    cursor.get_or_insert((time - start_frame / rate as f64).max(0.0) * rate as f64);
                if *position < source_frames as f64 {
                    let index = *position as usize;
                    let next = (index + 1).min(source_frames - 1);
                    let frac = (*position - index as f64) as f32;
                    for ch in 0..2 {
                        samples[ch] = material[index * 2 + ch]
                            + (material[next * 2 + ch] - material[index * 2 + ch]) * frac;
                    }
                }
                *position += 1.0;
            } else if active {
                let progress = (time / (source_frames as f64 / rate as f64)).clamp(0.0, 1.0) as f32;
                let value = if source_kind == "constant" {
                    0.25 + 0.5 * progress
                } else {
                    let frequency = if source_kind == "oscillator-detune" {
                        333.0
                    } else {
                        333.0 + 444.0 * progress
                    };
                    let detune = if source_kind == "oscillator-frequency" {
                        0.0
                    } else {
                        1200.0 * progress
                    };
                    let multiplier = 2.0_f64.powf(detune as f64 / 1200.0);
                    let phase = phase.get_or_insert(
                        ((time - start_frame / rate as f64).max(0.0)
                            * frequency as f64
                            * multiplier)
                            .fract(),
                    );
                    let value = (*phase * std::f64::consts::TAU).sin() as f32;
                    *phase = (*phase + frequency as f64 * multiplier / rate as f64).fract();
                    value
                };
                samples.fill(value);
            }
            for sample in &mut samples {
                let mut mixed = 0.0_f32;
                for _ in 0..VOICES {
                    mixed += *sample / VOICES as f32;
                }
                *sample = mixed;
            }
            input.extend(samples);
        }
        for (index, sample) in input.iter_mut().enumerate() {
            let gain = if automation {
                0.5 + 0.25 * ((index / 2).min(1024) as f32 / 1024.0)
            } else {
                0.5
            };
            *sample *= gain;
        }
        if waveshaper != 0 {
            // Include the finite shaper tail past the final source quantum,
            // also when the source ends in the middle of that quantum.
            input.resize(
                ((input.len() / 2).div_ceil(CALLBACK) + 1) * CALLBACK * 2,
                0.0,
            );
            // Compare preparation with the unchanged, unprepared production
            // kernel. Reference allocations are outside measurement.
            let mut reference = crate::nodes::WaveShaperNode::new(1);
            reference.set_curve(Some(vec![-1.0, -1.0, 0.0, 1.0, 1.0]));
            reference.set_oversample(shaper_mode(waveshaper));
            let mut shaped = vec![0.0; input.len()];
            for (from, to) in input
                .chunks(CALLBACK * 2)
                .zip(shaped.chunks_mut(CALLBACK * 2))
            {
                assert_eq!(reference.process(from, to, rate, 2, 0.0), CALLBACK);
            }
            input = shaped;
        }
        input.resize((reference_frames + 8192) * 2, 0.0);
        let reference = if rate == 48_000 {
            input
        } else {
            crate::resample_job::tests::reference(
                &DecodedAudio {
                    samples: input,
                    sample_rate: rate,
                    channels: 2,
                },
                48_000,
            )
        };
        for run in &mut runs {
            for Trace { spans, output } in &run.traces {
                let mut expected = [0.125; CALLBACK * 2];
                for &(from, to) in spans {
                    for (target, source) in expected
                        .iter_mut()
                        .zip(&reference[from as usize * 2..to as usize * 2])
                    {
                        *target += *source;
                    }
                }
                if let Some(index) = output
                    .iter()
                    .zip(expected)
                    .position(|(actual, expected)| actual.to_bits() != expected.to_bits())
                {
                    panic!(
                        "cold graph complete PCM changed at sample {index}, spans {spans:?}: actual {} expected {}",
                        output[index], expected[index]
                    );
                }
            }
            assert!(
                run.first_output_ns.is_some() != silent,
                "cold graph audible/silent schedule changed"
            );
            run.ended.sort_unstable();
            run.source_ids.sort_unstable();
            assert_eq!(run.ended, run.source_ids, "cold graph ended set changed");
            assert!(
                run.source_ids
                    .iter()
                    .all(|id| run.index.get_context(*id).is_none()),
                "cold graph retained source index"
            );
        }
        let first_output: Vec<_> = runs
            .iter()
            .map(|run| run.first_output_ns.unwrap_or(0))
            .collect();
        let retire_start = Instant::now();
        for run in &mut runs {
            run.scheduler.close();
            run.contexts.clear();
            run.mixer.restart();
        }
        while runs[0].mixer.cold_probe_usage().1 != 0
            || runs[0].mixer.cold_probe_usage().2 != 0
            // Retries can retire Empty/Ready leases before the final sessions.
            // Every actual destructor must complete, including those retries.
            || runs.iter().any(|run| {
                run.retire_started.load(Ordering::Acquire) != run.retired.load(Ordering::Acquire)
            })
            || runs
                .iter()
                .map(|run| run.retired.load(Ordering::Acquire))
                .sum::<usize>()
                < expected_owners
        {
            assert!(
                retire_start.elapsed() < Duration::from_secs(15),
                "cold graph retirement leaked credit"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            runs.iter().all(|run| run.mixer.cold_probe_usage().0 == 0),
            "cold graph leaked host credit"
        );
        assert!(
            !runs
                .iter()
                .any(|run| run.bad_thread.load(Ordering::Acquire)),
            "cold graph destroyed an owner outside CPU workers"
        );
        let retired_instances: usize = runs
            .iter()
            .map(|run| run.retired.load(Ordering::Acquire))
            .sum();
        assert_eq!(
            retired_instances, expected_owners,
            "cold preparation discarded admitted owners"
        );
        let nodes_per_context = 66 + usize::from(automation) + usize::from(waveshaper != 0);
        callback_ns.sort_unstable();
        let p95 = callback_ns[(callback_ns.len() - 1) * 95 / 100];
        let stop_json = stop_frame.map_or_else(|| "null".to_owned(), |s| s.to_string());
        println!(
            "MIGO_COLD {{\"cycle\":{cycle},\"automation\":{automation},\"waveshaper_factor\":{waveshaper},\"nodes_per_context\":{nodes_per_context},\"hosts\":{hosts},\"contexts_per_host\":{contexts},\"voices_per_context\":{VOICES},\"source_rate\":{rate},\"source_frames\":{source_frames},\"source_kind\":\"{source_kind}\",\"start_frame\":{start_frame},\"stop_frame\":{stop_json},\"silent\":{silent},\"graph_setup_ns\":{graph_setup_ns},\"all_ready_ns\":{},\"first_output_ns\":{first_output:?},\"playback_ns\":{playback_ns},\"mix_calls\":{},\"mix_p95_ns\":{p95},\"mix_max_ns\":{},\"mix_allocations\":{mix_allocations},\"mix_reallocations\":{mix_reallocations},\"peak_owners\":{peak_owners},\"retired_instances_including_retries\":{retired_instances},\"peak_aggregate_bytes\":{peak_aggregate},\"retirement_ns\":{},\"host_clock_resyncs\":{resyncs},\"complete_mixed_pcm_bits_checked\":true}}",
            all_ready_ns.unwrap(),
            callback_ns.len(),
            callback_ns.last().unwrap(),
            retire_start.elapsed().as_nanos()
        );
    }
}

#[test]
fn connected_dc_drains_and_restarts_with_same_device_owner() {
    let rate: u32 = std::env::var("MIGO_COLD_RATE").unwrap().parse().unwrap();
    let factor: usize = std::env::var("MIGO_COLD_WAVESHAPER")
        .unwrap()
        .parse()
        .unwrap();
    assert!(matches!(rate, 44_100 | 48_000));
    let mode = if factor == 0 {
        crate::nodes::OversampleType::None
    } else {
        shaper_mode(factor)
    };
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let mut mixer = crate::device_prepare::DeviceMixer::new(scheduler.audio_cpu());
    let started = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let bad = Arc::new(AtomicBool::new(false));
    mixer.cold_probe_retirement(started.clone(), retired.clone(), bad.clone());
    let mut context = AudioContext::new(1, rate, 2);
    context
        .prepare_node_retirement(
            scheduler.audio_cpu(),
            crate::limits::AudioAggregateLedger::process_global(),
        )
        .unwrap();
    let mut node = crate::nodes::WaveShaperNode::new(11);
    node.set_curve(Some(vec![-0.5, 0.25, 1.0]));
    context.add_node(Box::new(node));
    context.set_wave_shaper_oversample(11, mode);
    let mut contexts = HashMap::from([(1, context)]);
    let mut index = NodeContextIndex::new();
    index.register(11, 1);
    let wakeup = ThreadWakeup::new();
    let mut traces = Vec::new();
    let mut segments = Vec::new();
    let origin = Instant::now();
    let mut allocations = 0;
    for active in [true, false, true, false] {
        let context = contexts.get_mut(&1).unwrap();
        if active {
            context.connect(11, crate::context::DESTINATION_NODE_ID);
        } else {
            context.disconnect(11, None);
        }
        assert_eq!(
            context.has_active_sources(),
            active,
            "DC route activity changed"
        );
        let from = context.device_graph_frame();
        loop {
            assert!(
                origin.elapsed() < Duration::from_secs(20),
                "DC route failed to drain or restart"
            );
            mixer.service(
                &mut contexts,
                48_000,
                false,
                &wakeup,
                |id| index.unregister(id),
                |_| panic!("DC emitted a source ended event"),
            );
            let before_position = mixer.cold_probe_position(&contexts[&1]);
            let mut output = [0.125; CALLBACK * 2];
            let before = thread_counts();
            mix_contexts(
                &mut contexts,
                &mut index,
                &mut mixer,
                &mut output,
                48_000,
                2,
                |_| panic!("DC emitted a source ended event"),
            );
            let after = thread_counts();
            allocations +=
                after.allocations - before.allocations + after.reallocations - before.reallocations;
            let to = mixer.cold_probe_position(&contexts[&1]);
            traces.push((before_position, to, output));
            if (active && contexts[&1].device_graph_frame() >= from + 2048)
                || (!active && !mixer.has_output_work(&contexts, 48_000))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        segments.push((active, from, contexts[&1].device_graph_frame()));
        assert_eq!(
            mixer.cold_probe_usage().1,
            usize::from(rate != 48_000),
            "DC restart replaced or lost its owner"
        );
    }
    assert_eq!(allocations, 0, "DC production mix allocated");
    let mut input = vec![0.0; (contexts[&1].device_graph_frame() as usize + 8192) * 2];
    let mut reference = crate::nodes::WaveShaperNode::new(1);
    reference.set_curve(Some(vec![-0.5, 0.25, 1.0]));
    reference.set_oversample(mode);
    for &(active, from, to) in &segments {
        if active {
            for output in input[from as usize * 2..to as usize * 2].chunks_mut(CALLBACK * 2) {
                let zero = [0.0; CALLBACK * 2];
                reference.process(&zero[..output.len()], output, rate, 2, 0.0);
            }
        }
    }
    let reference = if rate == 48_000 {
        input
    } else {
        crate::resample_job::tests::reference(
            &DecodedAudio {
                sample_rate: rate,
                channels: 2,
                samples: input,
            },
            48_000,
        )
    };
    for (from, to, output) in &traces {
        assert!(to >= from && to - from <= CALLBACK as u64);
        let mut expected = [0.125; CALLBACK * 2];
        for (actual, source) in expected
            .iter_mut()
            .zip(&reference[*from as usize * 2..*to as usize * 2])
        {
            *actual += *source;
        }
        assert_eq!(
            output.map(f32::to_bits),
            expected.map(f32::to_bits),
            "DC production PCM or restart history changed"
        );
    }
    scheduler.close();
    contexts.clear();
    mixer.restart();
    let closing = Instant::now();
    let owners = usize::from(rate != 48_000);
    while mixer.cold_probe_usage().1 != 0
        || mixer.cold_probe_usage().2 != 0
        || retired.load(Ordering::Acquire) != owners
    {
        assert!(
            closing.elapsed() < Duration::from_secs(15),
            "DC close leaked physical credit"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(started.load(Ordering::Acquire), owners);
    assert!(
        !bad.load(Ordering::Acquire),
        "DC owner destroyed outside CPU workers"
    );
    let segments = segments
        .iter()
        .map(|(active, from, to)| format!("[{active},{from},{to}]"))
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "MIGO_DC {{\"source_rate\":{rate},\"factor\":{factor},\"mix_allocations\":{allocations},\"complete_pcm_bits_checked\":true,\"segments\":[{segments}],\"retired_owners\":{owners}}}"
    );
}
