//! Follow the real Actor response through Services and the Shared registry.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use shared::audio_resources::{AudioBufferFormat, AudioBufferKey, AudioResourceRegistry};
use std::sync::atomic::Ordering;

const BYTES: usize = 4 << 20;
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn counters(tx: &DecodeQueueSender<DecodeResult>) -> (usize, usize) {
    (
        tx.usage.items.load(Ordering::Acquire),
        tx.usage.bytes.load(Ordering::Acquire),
    )
}
pub(super) fn ownership() {
    for closed in [false, true] {
        for mode in [
            "unpolled",
            "release",
            "generation",
            "materialize",
            "registry-drop",
            "retired",
            "runtime-full",
            "spare",
        ] {
            transfer(mode, closed);
        }
    }
}
fn transfer(mode: &str, closed: bool) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(941, 1, u64::MAX);
    let cpu = host.audio_cpu();
    let (mut tx, rx) = result_custody::channel(1, BYTES, cpu.clone());
    let seen = Arc::new(Dropped::default());
    tx.retirement.as_mut().unwrap().hook = Some(witness(&seen));
    let resources = AudioResourceRegistry::new();
    let (commands, command_rx) = shared::audio_channel::channel();
    let sender = shared::op_state::AudioSender::hosted(
        commands,
        shared::channel::ThreadWakeup::new(),
        shared::op_state::AudioHostStartSignal::new(),
        resources.clone(),
    );
    let scope = migo_services::audio::BufferScope {
        tx: &sender,
        runtime_generation: 77,
    };
    let permit = migo_services::audio::reserve_encoded(&sender, 3).unwrap();
    let decoding =
        migo_services::audio::decode_audio_data(scope, 1, vec![1, 2, 3], permit).unwrap();
    let AudioCmd::DecodeAudioData { resp, .. } = command_rx.try_recv().unwrap() else {
        panic!("decode command missing")
    };
    let count = if mode == "spare" { 128 } else { BYTES / 4 };
    let mut samples = Vec::with_capacity(BYTES / 4);
    samples.extend((0..count).map(sample));
    let pointer = samples.as_ptr();
    publish_decode_result(
        &tx,
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(DecodedAudio {
                samples,
                sample_rate: 48000,
                channels: 2,
            }),
            resp,
        },
    );
    let contexts = HashMap::from([(1, AudioContext::new(1, 48000, 2))]);
    if closed {
        host.close();
    }
    result_custody::integrate(rx.try_recv_owned().unwrap(), &contexts, &mut HashMap::new());
    assert_eq!(
        counters(&tx),
        (1, BYTES),
        "response lost queue credit before registry admission"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    if mode == "retired" {
        resources.finish_runtime_drop(77);
    }
    if mode == "runtime-full" {
        let format = AudioBufferFormat {
            sample_rate: 48000,
            channels: 1,
            frames: 1,
        };
        for _ in 0..512 {
            resources.reserve_backing(77, format).unwrap();
        }
    }
    let gate = block(&cpu);
    if mode == "unpolled" {
        let before = migo_alloc_probe::thread_counts();
        drop(decoding);
        assert_eq!(
            counters(&tx),
            (1, BYTES),
            "unpolled response returned credit before CPU disposal"
        );
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (BYTES / 2) as u64,
            "response final payload destructor escaped CPU workers"
        );
    } else {
        let answer = crate::streaming::get_stream_runtime().block_on(decoding.answer());
        assert_eq!(
            counters(&tx),
            (0, 0),
            "registry adoption retained transport credit"
        );
        if matches!(mode, "retired" | "runtime-full") {
            let error = answer.unwrap_err();
            let expected = if mode == "retired" {
                "[InvalidOperation]"
            } else {
                "[InputSaturated]"
            };
            assert!(
                error.message.contains(expected),
                "response registry changed typed refusal"
            );
        } else {
            let info = answer.unwrap();
            assert_eq!(
                (info.sample_rate, info.channels, info.length),
                (48000, 2, (count / 2) as u32)
            );
            assert_eq!(info.duration, (count / 2) as f64 / 48000.0);
            let key = AudioBufferKey {
                runtime_generation: 77,
                serial: info.id,
            };
            let node = resources.prepare_snapshot(key, None).unwrap().snapshot();
            assert!(
                node.samples()
                    .iter()
                    .enumerate()
                    .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
                "response registry changed complete PCM"
            );
            if mode != "spare" {
                assert_eq!(
                    node.samples().as_ptr(),
                    pointer,
                    "response registry copied original PCM"
                );
            }
            match mode {
                "generation" => resources.finish_runtime_drop(77),
                "materialize" => {
                    let planar = resources.prepare_materialize(key).unwrap().commit();
                    assert_eq!(planar.len(), count);
                    resources.release_buffer(key);
                }
                "registry-drop" => {}
                _ => {
                    resources.release_buffer(key);
                }
            }
            drop(sender);
            drop(resources);
            assert_eq!(
                seen.count.load(Ordering::Acquire),
                0,
                "response snapshot lost its final Arc custody"
            );
            host.close();
            let before = migo_alloc_probe::thread_counts();
            drop(node);
            assert!(
                migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed
                    < (BYTES / 2) as u64,
                "response final payload destructor escaped CPU workers"
            );
        }
    }
    assert_eq!(
        seen.count.load(Ordering::Acquire),
        0,
        "response final payload destructor escaped CPU workers"
    );
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1 && counters(&tx) == (0, 0));
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "response final payload destructor escaped CPU workers"
    );
}
