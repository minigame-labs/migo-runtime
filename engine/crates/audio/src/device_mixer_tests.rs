use super::*;
use crate::{context::DESTINATION_NODE_ID, decoder::DecodedAudio};
use std::{cell::RefCell, thread};

fn fixture(
    rate: u32,
    channels: u32,
) -> (
    migo_io::scheduler::IoScheduler,
    DeviceMixer,
    HashMap<u32, AudioContext>,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut mixer = DeviceMixer::new(scheduler.audio_cpu());
    mixer.controller.host.usage = Usage::new(8);
    mixer.controller.host.process = Usage::new(16);
    (
        scheduler,
        mixer,
        HashMap::from([(1, AudioContext::new(1, rate, channels))]),
    )
}
fn source(ctx: &mut AudioContext, id: u32, frames: usize) -> Vec<f32> {
    let samples: Vec<_> = (0..frames * ctx.channels() as usize)
        .map(|i| (i % 127) as f32 / 256.0 - 0.25)
        .collect();
    let b = ctx
        .add_buffer(DecodedAudio {
            sample_rate: ctx.sample_rate(),
            channels: ctx.channels(),
            samples: samples.clone(),
        })
        .unwrap();
    ctx.create_buffer_source(id);
    ctx.set_buffer(id, Some(b));
    ctx.watch_source_ended(id, true);
    ctx.connect(id, DESTINATION_NODE_ID);
    ctx.start_source(id, ctx.current_time(), 0.0, None);
    ctx.resume();
    samples
}
fn service(m: &mut DeviceMixer, contexts: &mut HashMap<u32, AudioContext>, target: u32) {
    m.service(
        contexts,
        target,
        false,
        &ThreadWakeup::new(),
        |_| {},
        |_| {},
    );
}
fn until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < end, "device mixer did not become ready");
        thread::yield_now();
    }
}
fn ready(m: &mut DeviceMixer, contexts: &mut HashMap<u32, AudioContext>, target: u32) {
    until(|| {
        service(m, contexts, target);
        matches!(
            m.routes.get(&1).map(|r| &r.state),
            Some(RouteState::Active(_))
        )
    });
}
fn session(m: &mut DeviceMixer, id: u32) -> &mut Session {
    let RouteState::Active(d) = &mut m.routes.get_mut(&id).unwrap().state else {
        panic!("managed route was not active")
    };
    d.session().unwrap()
}

fn held_request(
    m: &mut DeviceMixer,
    context: &AudioContext,
    target: u32,
) -> std::sync::mpsc::Sender<()> {
    use std::sync::{Mutex, atomic::AtomicBool, mpsc};
    let (entered, wait) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    let once = AtomicBool::new(false);
    m.controller
        .ensure(
            context,
            target,
            ThreadWakeup::new(),
            Some(Arc::new(move |_: &Ready| {
                if !once.swap(true, Ordering::AcqRel) {
                    let _ = entered.send(());
                    let _ = blocked.lock().unwrap().recv();
                }
            })),
        )
        .unwrap();
    m.routes.insert(
        context.id(),
        Route {
            epoch: context.device_epoch(),
            format: Format {
                source: context.sample_rate(),
                target,
                channels: context.channels(),
            },
            state: RouteState::Pending,
        },
    );
    wait.recv_timeout(Duration::from_secs(10)).unwrap();
    release
}

#[test]
fn native_rate_bypasses_filter_admission_and_preserves_additive_channel_mapping() {
    for (source_channels, target_channels) in [(1, 2), (2, 1), (3, 5), (5, 2)] {
        let (_s, mut m, mut contexts) = fixture(48000, source_channels);
        let samples = source(contexts.get_mut(&1).unwrap(), 10, 128);
        service(&mut m, &mut contexts, 48000);
        assert!(m.has_output_work(&contexts, 48000));
        let mut out = vec![0.25; 128 * target_channels];
        let mut expected = out.clone();
        AudioContext::mix_device_channels(
            &samples,
            &mut expected,
            source_channels as usize,
            target_channels,
        );
        m.mix(
            contexts.get_mut(&1).unwrap(),
            &mut out,
            48000,
            target_channels as u32,
            |_| {},
            |_| {},
        );
        assert_eq!(
            out, expected,
            "native bypass corrupted the shared mix or channel mapping"
        );
        assert!(
            m.routes.is_empty() && m.controller.slots.is_empty(),
            "native bypass allocated a managed route"
        );
        assert_eq!(
            m.controller.host.usage.owners.load(Ordering::Acquire),
            0,
            "native bypass acquired a filter owner"
        );
    }
}
#[test]
fn real_managed_mix_matches_complete_mature_pcm_with_channel_mapping_and_partial_callbacks() {
    for (rate, target, source_channels, target_channels, partition) in [
        (44100, 48000, 1, 2, 128),
        (48000, 44100, 2, 1, 3),
        (96000, 8000, 3, 5, 17),
    ] {
        let (_s, mut m, mut contexts) = fixture(rate, source_channels);
        let mut input = source(contexts.get_mut(&1).unwrap(), 10, 513);
        ready(&mut m, &mut contexts, target);
        let mut pcm = Vec::new();
        let mut ended = Vec::new();
        let mut collected = Vec::new();
        for _ in 0..100000 {
            let mut out = vec![0.0; partition * target_channels + target_channels - 1];
            out[partition * target_channels..].fill(0.625);
            m.mix(
                contexts.get_mut(&1).unwrap(),
                &mut out,
                target,
                target_channels as u32,
                |id| collected.push(id),
                |id| ended.push(id),
            );
            assert_eq!(
                out[partition * target_channels..],
                vec![0.625; target_channels - 1],
                "partial device frame was modified"
            );
            pcm.extend_from_slice(&out[..partition * target_channels]);
            if !m.has_output_work(&contexts, target) {
                break;
            }
        }
        assert!(
            !m.has_output_work(&contexts, target),
            "managed tail never became idle"
        );
        assert!(
            session(&mut m, 1).sleeping,
            "mixer hid buffered sinc tail after graph became inactive"
        );
        let p = session(&mut m, 1).pull.clocks();
        input.resize(
            (p.rendered_source as usize + 4096) * source_channels as usize,
            0.0,
        );
        let reference = crate::resample_job::tests::reference(
            &DecodedAudio {
                samples: input,
                sample_rate: rate,
                channels: source_channels,
            },
            target,
        );
        let mut mapped = vec![0.0; pcm.len()];
        AudioContext::mix_device_channels(
            &reference[..p.published_output as usize * source_channels as usize],
            &mut mapped,
            source_channels as usize,
            target_channels,
        );
        assert_eq!(
            pcm, mapped,
            "managed device mix differed from complete mature PCM"
        );
        assert_eq!(
            collected,
            [10],
            "production mix did not collect the graph node immediately"
        );
        assert_eq!(
            ended,
            [10],
            "production mix lost or repeated a fenced ended event"
        );
    }
}
#[test]
fn pending_preparation_does_not_render_or_mutate_the_shared_bus() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    // Route registration is management work; render itself never starts it.
    let mut out = [0.25; 256];
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut out,
        48000,
        2,
        |_| {},
        |_| {},
    );
    assert_eq!(out, [0.25; 256], "unprepared route mutated the shared mix");
    assert_eq!(
        contexts[&1].device_graph_frame(),
        0,
        "unprepared route advanced its graph clock"
    );
    assert!(m.routes.is_empty(), "render started route preparation");
    let release = held_request(&mut m, &contexts[&1], 48000);
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut out,
        48000,
        2,
        |_| {},
        |_| {},
    );
    assert_eq!(
        out, [0.25; 256],
        "pending preparation mutated the shared mix"
    );
    assert_eq!(
        contexts[&1].device_graph_frame(),
        0,
        "pending preparation rendered graph PCM"
    );
    drop(release);
    ready(&mut m, &mut contexts, 48000);
    assert_eq!(
        contexts[&1].device_graph_frame(),
        0,
        "preparation rendered graph PCM"
    );
}
#[test]
fn cold_unused_contexts_and_stopped_pending_sources_do_not_hold_filter_credit() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    contexts.get_mut(&1).unwrap().resume();
    service(&mut m, &mut contexts, 48000);
    assert!(
        m.routes.is_empty(),
        "idle context prewarmed an unused filter"
    );
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    let release = held_request(&mut m, &contexts[&1], 48000);
    contexts.get_mut(&1).unwrap().stop_source(10, 0.0);
    let mut collected = Vec::new();
    let mut ended = Vec::new();
    m.service(
        &mut contexts,
        48000,
        false,
        &ThreadWakeup::new(),
        |id| collected.push(id),
        |id| ended.push(id),
    );
    assert!(
        m.routes.is_empty() && m.controller.slots.is_empty(),
        "stopped pending source retained its preparation"
    );
    assert_eq!(collected, [10]);
    assert_eq!(ended, [10]);
    assert_eq!(
        m.controller.host.usage.owners.load(Ordering::Acquire),
        1,
        "cancelled CPU owner returned physical credit before destruction"
    );
    drop(release);
    until(|| m.controller.host.usage.owners.load(Ordering::Acquire) == 0);
}
#[test]
fn source_collection_and_ended_survive_preparation_admission_failure_without_audio() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    m.controller.host.usage = Usage::new(0);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    until(|| {
        service(&mut m, &mut contexts, 48000);
        matches!(
            m.routes[&1].state,
            RouteState::Failed {
                code: ErrorCode::InputSaturated,
                ..
            }
        )
    });
    assert!(
        !m.has_output_work(&contexts, 48000),
        "failed admission kept audible output active"
    );
    assert!(
        m.retry_wait(&contexts).is_some(),
        "admission failure had no retry wake deadline"
    );
    contexts.get_mut(&1).unwrap().stop_source(10, 0.0);
    let events = RefCell::new(Vec::new());
    m.service(
        &mut contexts,
        48000,
        true,
        &ThreadWakeup::new(),
        |id| events.borrow_mut().push((0, id)),
        |id| events.borrow_mut().push((1, id)),
    );
    assert_eq!(
        *events.borrow(),
        [(0, 10)],
        "background stop collection or event deferral failed"
    );
    m.service(
        &mut contexts,
        48000,
        false,
        &ThreadWakeup::new(),
        |id| events.borrow_mut().push((0, id)),
        |id| events.borrow_mut().push((1, id)),
    );
    assert_eq!(
        *events.borrow(),
        [(0, 10), (1, 10)],
        "filter admission hid an explicit stop event"
    );
    assert!(m.retry_wait(&contexts).is_none());
}
#[test]
fn failed_admission_retries_a_new_operation_on_the_same_host_authority() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    m.controller.host.usage = Usage::new(1);
    let held = m.controller.host.usage.reserve().unwrap();
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    until(|| {
        service(&mut m, &mut contexts, 48000);
        matches!(
            m.routes[&1].state,
            RouteState::Failed {
                code: ErrorCode::InputSaturated,
                ..
            }
        )
    });
    let operation = m.controller.slots[&1].operation.clone();
    drop(held);
    if let RouteState::Failed { retry, .. } = &mut m.routes.get_mut(&1).unwrap().state {
        *retry = Some(Instant::now());
    }
    service(&mut m, &mut contexts, 48000);
    assert!(
        !Arc::ptr_eq(&operation, &m.controller.slots[&1].operation),
        "admission retry reused an exhausted operation"
    );
    ready(&mut m, &mut contexts, 48000);
    assert!(
        Arc::ptr_eq(&m.controller.host.usage, &m.controller.slots[&1].host.usage),
        "admission retry duplicated host authority"
    );
}
#[test]
fn suspension_background_and_empty_callbacks_preserve_owner_clocks_and_pcm() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    ready(&mut m, &mut contexts, 48000);
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut [0.0; 256],
        48000,
        2,
        |_| {},
        |_| {},
    );
    let before = session(&mut m, 1).pull.clocks();
    let op = m.controller.slots[&1].operation.clone();
    contexts.get_mut(&1).unwrap().suspend();
    service(&mut m, &mut contexts, 48000);
    assert!(!m.has_output_work(&contexts, 48000));
    let mut out = [0.25; 256];
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut out,
        48000,
        2,
        |_| panic!("suspended collection"),
        |_| panic!("suspended event"),
    );
    assert_eq!(out, [0.25; 256]);
    assert_eq!(
        session(&mut m, 1).pull.clocks(),
        before,
        "suspended mix advanced publication"
    );
    contexts.get_mut(&1).unwrap().resume();
    m.service(
        &mut contexts,
        48000,
        true,
        &ThreadWakeup::new(),
        |_| {},
        |_| panic!("background event"),
    );
    assert_eq!(session(&mut m, 1).pull.clocks(), before);
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut [0.25],
        48000,
        2,
        |_| {},
        |_| {},
    );
    assert_eq!(
        session(&mut m, 1).pull.clocks(),
        before,
        "partial callback advanced publication"
    );
    assert!(
        Arc::ptr_eq(&op, &m.controller.slots[&1].operation),
        "suspension replaced the prepared owner"
    );
}
#[test]
fn close_id_reuse_and_same_format_route_reset_revoke_cached_pcm_and_release_slots() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    ready(&mut m, &mut contexts, 48000);
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut [0.0; 2],
        48000,
        2,
        |_| {},
        |_| {},
    );
    assert!(
        !m.has_output_work(&contexts, 96000),
        "foreign target route was reported as audible"
    );
    let old = m.routes[&1].epoch.clone();
    contexts.remove(&1);
    contexts.insert(1, AudioContext::new(1, 44100, 2));
    contexts.get_mut(&1).unwrap().resume();
    let mut out = [0.25; 256];
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut out,
        48000,
        2,
        |_| panic!("foreign collection"),
        |_| panic!("foreign ended"),
    );
    assert_eq!(out, [0.25; 256], "ID reuse published stale cached PCM");
    service(&mut m, &mut contexts, 48000);
    assert!(
        m.routes.is_empty() && m.controller.slots.is_empty(),
        "closed context retained a route slot"
    );
    assert!(!old.is_live());
    source(contexts.get_mut(&1).unwrap(), 11, 1024);
    ready(&mut m, &mut contexts, 48000);
    let old = m.routes[&1].epoch.clone();
    crate::audio_thread::renegotiate_context_routes(&mut contexts, (48000, 2), (48000, 2));
    m.restart();
    service(&mut m, &mut contexts, 48000);
    assert!(!old.is_live());
    assert!(
        !m.has_output_work(&contexts, 48000),
        "same-format backend kept stale active PCM"
    );
    ready(&mut m, &mut contexts, 48000);
    assert!(!m.routes[&1].epoch.same(&old));
}
#[test]
fn warmed_first_and_steady_mix_turns_including_end_and_idle_do_not_allocate() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    contexts.get_mut(&1).unwrap().process(&mut [0.0; 256]);
    ready(&mut m, &mut contexts, 48000);
    let mut ended = 0;
    for _ in 0..10000 {
        let before = migo_alloc_probe::thread_counts();
        m.mix(
            contexts.get_mut(&1).unwrap(),
            &mut [0.0; 256],
            48000,
            2,
            |_| {},
            |_| ended += 1,
        );
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            (after.allocations, after.reallocations),
            (before.allocations, before.reallocations),
            "managed mix or watched end allocated"
        );
        if !m.has_output_work(&contexts, 48000) {
            break;
        }
    }
    assert_eq!(ended, 1);
    assert!(!m.has_output_work(&contexts, 48000));
    profile_continuous_mix();
}
#[test]
fn extreme_ratios_fill_a_quantum_within_the_mix_turn_and_lookahead_bounds() {
    for (source_rate, target_rate, channels) in [(768000, 3000, 1), (3000, 768000, 32)] {
        let (_s, mut m, mut contexts) = fixture(source_rate, channels);
        source(contexts.get_mut(&1).unwrap(), 10, 256);
        ready(&mut m, &mut contexts, target_rate);
        let mut out = vec![0.0; 128 * channels as usize];
        m.mix(
            contexts.get_mut(&1).unwrap(),
            &mut out,
            target_rate,
            channels,
            |_| {},
            |_| {},
        );
        assert!(
            matches!(m.routes[&1].state, RouteState::Active(_)),
            "valid extreme rate exhausted the bounded mix driver"
        );
        assert_eq!(
            session(&mut m, 1).pull.clocks().published_output,
            128,
            "mix driver did not fill its quantum"
        );
    }
}
#[test]
fn bounded_mix_exhaustion_retires_the_owner_and_preserves_the_existing_bus() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    source(contexts.get_mut(&1).unwrap(), 10, 1024);
    ready(&mut m, &mut contexts, 48000);
    let mut out = [0.25; 256];
    m.mix_bounded(
        contexts.get_mut(&1).unwrap(),
        &mut out,
        (48000, 2),
        0,
        |_| panic!("zero-budget collection"),
        |_| panic!("zero-budget ended"),
    );
    assert!(
        matches!(
            m.routes[&1].state,
            RouteState::Failed {
                code: ErrorCode::InputSaturated,
                retry: None
            }
        ),
        "zero turn budget did not retire its route"
    );
    assert_eq!(out, [0.25; 256], "zero turn budget mutated the shared bus");
    assert_eq!(contexts[&1].device_graph_frame(), 0);
    until(|| m.controller.host.usage.owners.load(Ordering::Acquire) == 0);
}
#[test]
fn preparation_retry_shortens_idle_sleep_without_overriding_commands_or_earlier_deadlines() {
    use crate::{audio_thread::device_retry_wait, power_manager::AudioWaitMode};
    let delay = Duration::from_millis(100);
    assert_eq!(
        device_retry_wait(AudioWaitMode::Indefinite, Some(delay)),
        AudioWaitMode::Timed(delay),
        "idle sleep lost the admission retry deadline"
    );
    assert_eq!(
        device_retry_wait(AudioWaitMode::Timed(Duration::from_millis(5)), Some(delay)),
        AudioWaitMode::Timed(Duration::from_millis(5))
    );
    assert_eq!(
        device_retry_wait(AudioWaitMode::Continue, Some(delay)),
        AudioWaitMode::Continue
    );
    assert_eq!(
        device_retry_wait(AudioWaitMode::Indefinite, None),
        AudioWaitMode::Indefinite
    );
}

#[test]
fn native_and_managed_contexts_add_into_one_bus_without_cross_route_authority() {
    let (_s, mut m, mut contexts) = fixture(44100, 2);
    let mut input = source(contexts.get_mut(&1).unwrap(), 10, 1024);
    contexts.insert(2, AudioContext::new(2, 48000, 1));
    let native = source(contexts.get_mut(&2).unwrap(), 20, 128);
    ready(&mut m, &mut contexts, 48000);
    let mut output = [0.125; 256];
    m.mix(
        contexts.get_mut(&1).unwrap(),
        &mut output,
        48000,
        2,
        |_| {},
        |_| {},
    );
    m.mix(
        contexts.get_mut(&2).unwrap(),
        &mut output,
        48000,
        2,
        |_| {},
        |_| {},
    );
    input.resize(4096, 0.0);
    let expected_src = crate::resample_job::tests::reference(
        &DecodedAudio {
            sample_rate: 44100,
            channels: 2,
            samples: input,
        },
        48000,
    );
    let mut expected = [0.125; 256];
    AudioContext::mix_device_channels(&expected_src[..256], &mut expected, 2, 2);
    AudioContext::mix_device_channels(&native, &mut expected, 1, 2);
    assert_eq!(
        output, expected,
        "native and sinc contributions did not share one additive mix"
    );
    assert_eq!(
        m.controller.slots.len(),
        1,
        "native context created a managed filter slot"
    );
    assert!(Arc::ptr_eq(
        &m.controller.host.usage,
        &m.controller.slots[&1].host.usage
    ));
}

// The functional suite checks continuous publication and allocations here.
// Timing lines from parallel full suites are not performance evidence; the
// accepted receipt runs this same test executable alone with fixed affinity.
fn profile_continuous_mix() {
    for (rate, target, channels) in [
        (44100, 48000, 2),
        (48000, 44100, 2),
        (96000, 8000, 2),
        (768000, 3000, 1),
        (3000, 768000, 32),
    ] {
        let (_scheduler, mut mixer, mut contexts) = fixture(rate, channels);
        source(contexts.get_mut(&1).unwrap(), 10, 1024);
        assert!(contexts.get_mut(&1).unwrap().set_loop(10, true, 0.0, 0.0));
        ready(&mut mixer, &mut contexts, target);
        let mut output = vec![0.0; 128 * channels as usize];
        for _ in 0..16 {
            output.fill(0.0);
            mixer.mix(
                contexts.get_mut(&1).unwrap(),
                &mut output,
                target,
                channels,
                |_| {},
                |_| panic!("loop ended"),
            );
        }
        let start_frame = session(&mut mixer, 1).pull.clocks().published_output;
        let rounds = if cfg!(debug_assertions) { 16 } else { 1000 };
        let mut times = Vec::with_capacity(rounds);
        let before = migo_alloc_probe::thread_counts();
        for _ in 0..rounds {
            output.fill(0.0);
            let start = Instant::now();
            mixer.mix(
                contexts.get_mut(&1).unwrap(),
                &mut output,
                target,
                channels,
                |_| {},
                |_| panic!("loop ended"),
            );
            times.push(start.elapsed().as_nanos() as u64);
        }
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            (after.allocations, after.reallocations),
            (before.allocations, before.reallocations),
            "continuous device mix allocated"
        );
        assert_eq!(
            session(&mut mixer, 1).pull.clocks().published_output - start_frame,
            (rounds * 128) as u64,
            "continuous mix lost or repeated device frames"
        );
        assert!(output.iter().all(|sample| sample.is_finite()));
        times.sort_unstable();
        println!(
            "MIGO_DEVICE_MIX {{\"source\":{rate},\"target\":{target},\"channels\":{channels},\"rounds\":{rounds},\"p50_ns\":{},\"p99_ns\":{},\"max_ns\":{}}}",
            times[rounds / 2],
            times[rounds * 99 / 100],
            times[rounds - 1]
        );
    }
}
