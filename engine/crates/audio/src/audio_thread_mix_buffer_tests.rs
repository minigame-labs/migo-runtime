//! Actual scratch allocation, transactional output admission and original CPU drop.
use super::*;
use crate::audio_thread::context_retirement_tests::{Dropped, block, wait, witness};
use crate::audio_thread::{NodeContextIndex, mix_contexts, renegotiate_context_routes, soft_limit};
use crate::context::{AudioContext, DESTINATION_NODE_ID};
use crate::decoder::DecodedAudio;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use shared::channel::ThreadWakeup;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const LIMIT: usize = 64 << 20;
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    Host,
    Arc<AudioAggregateLedger>,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = Host::new(scheduler.audio_cpu(), ledger.clone(), 2).unwrap();
    (scheduler, host, ledger)
}
fn bytes(samples: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let value = Vec::<f32>::with_capacity(samples);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert_eq!(allocated, value.capacity() as u64 * 4);
    value.capacity() * 4
}
fn fee(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    let before = ledger.used_bytes();
    let quote = host
        .reserve_value_with_bytes::<Option<Vec<f32>>>(bytes)
        .unwrap();
    let result = ledger.used_bytes() - before;
    drop(quote);
    result
}
fn identity(buffer: &MixBuffer) -> (usize, usize, usize, Vec<u32>) {
    (
        buffer.len(),
        buffer.capacity(),
        buffer.as_ptr() as usize,
        buffer.iter().map(|v| v.to_bits()).collect(),
    )
}
#[test]
fn first_capacity_is_charged_from_real_allocation_and_zero_initialized() {
    let (_s, host, ledger) = setup();
    for samples in [256, 4096] {
        let expected = fee(&host, &ledger, bytes(samples));
        let buffer = MixBuffer::new(host.clone(), samples).unwrap();
        assert_eq!(buffer.capacity(), samples);
        assert_eq!(buffer.len(), samples);
        assert!(buffer.iter().all(|v| v.to_bits() == 0));
        assert_eq!(
            ledger.used_bytes(),
            HANDOFF_BYTES + expected,
            "mix buffer initial capacity omitted physical credit"
        );
        drop(buffer);
        wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn first_refusal_does_not_allocate_pcm_or_leak_tickets() {
    let (_s, host, ledger) = setup();
    let samples = 4096;
    let size = bytes(samples);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "mix initial peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = MixBuffer::new(host.clone(), samples);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "mix buffer initial admission was bypassed");
    assert!(
        allocated < size as u64,
        "mix buffer initial PCM allocated before admission"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop((result, filler, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn growth_refusal_keeps_original_capacity_pointer_length_and_pcm() {
    let (_s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 4096).unwrap();
    buffer.fill(0.75);
    let original = identity(&buffer);
    let expected = HANDOFF_BYTES + fee(&host, &ledger, bytes(4096));
    // The actual old Vec reaches this same business assertion without credit.
    wait(|| ledger.used_bytes() <= expected);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "mix growth peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = buffer.resize(8192);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "mix buffer growth bypassed admission");
    assert_eq!(
        identity(&buffer),
        original,
        "mix buffer refusal changed old allocation or PCM"
    );
    assert!(
        allocated < 32768,
        "mix buffer growth allocated PCM before admission"
    );
    drop((filler, buffer, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn admitted_growth_keeps_prefix_and_zero_tail_until_render_clears_it() {
    let (_s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 256).unwrap();
    for (i, value) in buffer.iter_mut().enumerate() {
        *value = f32::from_bits(0x3f000000 + i as u32);
    }
    let prefix: Vec<_> = buffer.iter().map(|v| v.to_bits()).collect();
    buffer.resize(512).unwrap();
    assert_eq!(
        buffer.len(),
        512,
        "mix buffer growth did not publish complete new length"
    );
    assert_eq!(
        buffer[..256]
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        prefix,
        "mix buffer growth changed original PCM prefix"
    );
    assert!(
        buffer[256..].iter().all(|v| v.to_bits() == 0),
        "mix buffer growth did not zero new tail"
    );
    buffer.fill(0.0);
    assert!(buffer.iter().all(|v| v.to_bits() == 0));
    drop((buffer, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn shrink_and_reuse_under_full_budget_do_not_allocate_or_return_capacity_credit() {
    let (_s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 4096).unwrap();
    buffer.fill(0.25);
    let pointer = buffer.as_ptr();
    let capacity = buffer.capacity();
    let used = ledger.used_bytes();
    let filler = ledger.try_reserve(LIMIT - used, "mix reuse peer").unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = buffer
        .resize(128)
        .and_then(|()| buffer.resize(4096))
        .and_then(|()| buffer.resize(4096));
    let end = migo_alloc_probe::thread_counts();
    assert!(
        result.is_ok(),
        "mix buffer within capacity attempted fresh admission"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "mix buffer shrink or reuse allocated"
    );
    assert_eq!(buffer.as_ptr(), pointer);
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(
        ledger.used_bytes(),
        LIMIT,
        "mix buffer shrink returned retained capacity credit"
    );
    assert!(buffer[..128].iter().all(|v| *v == 0.25));
    assert!(buffer[128..].iter().all(|v| v.to_bits() == 0));
    drop(filler);
    assert_eq!(ledger.used_bytes(), used);
    drop((buffer, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn bulk_growth_keeps_both_credits_and_destroys_original_vec_on_original_cpu() {
    let (s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 4096).unwrap();
    buffer.fill(0.75);
    let old = fee(&host, &ledger, bytes(4096));
    let next = fee(&host, &ledger, bytes(8192));
    let state = Arc::new(Dropped::default());
    buffer.observe(witness(&state));
    let gate = block(&s.audio_cpu());
    let before = migo_alloc_probe::thread_counts();
    buffer.resize(8192).unwrap();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    assert!(freed < 16384, "mix buffer growth freed old PCM on caller");
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + old + next,
        "mix buffer returned overlapping PCM credit early"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    assert_eq!(buffer.len(), 8192);
    assert!(buffer[..4096].iter().all(|v| *v == 0.75));
    assert!(buffer[4096..].iter().all(|v| v.to_bits() == 0));
    gate.open();
    wait(|| {
        state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES + next
    });
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((buffer, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_cpu_drop_keeps_credit_and_pcm_until_real_drop_without_late_allocation() {
    let (s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 4096).unwrap();
    let expected = ledger.used_bytes();
    let state = Arc::new(Dropped::default());
    buffer.observe(witness(&state));
    let gate = block(&s.audio_cpu());
    s.close();
    let before = migo_alloc_probe::thread_counts();
    drop(buffer);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "mix buffer shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < 16384,
        "mix buffer shutdown freed large PCM on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "mix buffer shutdown returned credit before real CPU drop"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
struct Candidate(Arc<AtomicUsize>);
impl Drop for Candidate {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}
#[test]
fn refused_output_candidate_is_disposed_before_old_buffer_or_route_can_be_replaced() {
    let (_s, host, ledger) = setup();
    let mut buffer = MixBuffer::new(host.clone(), 256).unwrap();
    buffer.fill(0.75);
    let original = identity(&buffer);
    let old = Arc::new(AtomicUsize::new(0));
    let old_output = Candidate(old.clone());
    let proposed = Arc::new(AtomicUsize::new(0));
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "mix output peer")
        .unwrap();
    let result = buffer.admit(Candidate(proposed.clone()), 8192);
    assert!(
        result.is_err(),
        "mix output candidate bypassed buffer admission"
    );
    assert_eq!(proposed.load(Ordering::Acquire), 1);
    assert_eq!(old.load(Ordering::Acquire), 0);
    assert_eq!(identity(&buffer), original);
    drop(filler);
    let output = buffer.admit(Candidate(proposed.clone()), 8192).unwrap();
    assert_eq!(buffer.len(), 8192);
    assert_eq!(proposed.load(Ordering::Acquire), 1);
    drop(output);
    assert_eq!(proposed.load(Ordering::Acquire), 2);
    drop((old_output, buffer, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn complete_actor_mix_limiter_and_output_layout_pcm_trace() {
    let (scheduler, host, ledger) = setup();
    for rate in [44100, 48000] {
        for source_channels in [1, 2, 32] {
            let mut buffer = MixBuffer::new(host.clone(), 256).unwrap();
            let mut context = AudioContext::new(7, rate, source_channels);
            let samples = (0..16384 * source_channels as usize)
                .map(|i| (i % 127) as f32 / 96.0 - 0.625)
                .collect();
            let audio = context
                .add_buffer(DecodedAudio {
                    sample_rate: rate,
                    channels: source_channels,
                    samples,
                })
                .unwrap();
            assert!(context.create_buffer_source(10));
            assert!(context.set_buffer(10, Some(audio)));
            context.connect(10, DESTINATION_NODE_ID);
            context.start_source(10, 0.0, 0.0, None);
            context.resume();
            let mut contexts = HashMap::from([(7, context)]);
            let mut index = NodeContextIndex::new();
            let mut mixer =
                crate::device_prepare::DeviceMixer::managed(scheduler.audio_cpu(), host.clone());
            let wakeup = ThreadWakeup::new();
            let mut previous_channels = 2;
            for channels in [1, 2, 5, 32, 64] {
                let (target, target_channels) = buffer
                    .admit((48000, channels), 128 * channels as usize)
                    .unwrap();
                if renegotiate_context_routes(
                    &mut contexts,
                    (48000, previous_channels),
                    (target, target_channels),
                ) {
                    mixer.restart();
                }
                previous_channels = channels;
                wait(|| {
                    mixer.service(
                        &mut contexts,
                        target,
                        false,
                        &wakeup,
                        |id| index.unregister(id),
                        |_| {},
                    );
                    mixer.has_output_work(&contexts, target)
                });
                for turn in 0..3 {
                    buffer.fill(0.375);
                    let mut ended = Vec::new();
                    mix_contexts(
                        &mut contexts,
                        &mut index,
                        &mut mixer,
                        &mut buffer,
                        target,
                        target_channels,
                        |id| ended.push(id),
                    );
                    soft_limit(&mut buffer);
                    assert!(buffer.iter().all(|v| v.is_finite()));
                    assert!(
                        buffer.iter().any(|v| v.to_bits() != 0.375f32.to_bits()),
                        "mix buffer trace omitted real graph PCM"
                    );
                    let bits: Vec<_> = buffer.iter().map(|v| v.to_bits()).collect();
                    ended.sort_unstable();
                    println!(
                        "MIGO_MIX_BUFFER_PCM {rate}:{source_channels}:{channels}:{turn} {:?} {:?} {:?}",
                        bits,
                        ended,
                        [contexts[&7].current_time().to_bits()]
                    );
                }
            }
            drop((contexts, index, mixer, buffer));
            wait(|| ledger.used_bytes() == HANDOFF_BYTES);
        }
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
