//! Last PCM/snapshot references and large curves used to die in setters.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::{AudioNodeProcessor, BufferSourceNode};
use std::sync::atomic::Ordering;

const BYTES: usize = 4 << 20;
const LIMIT: usize = 32 << 20;

fn snapshot(value: f32, frames: u32) -> Arc<AudioSnapshot> {
    snapshot_pcm(vec![value; frames as usize])
}

fn snapshot_pcm(samples: Vec<f32>) -> Arc<AudioSnapshot> {
    let frames = samples.len() as u32;
    use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
    let registry = AudioResourceRegistry::new();
    let lease = registry
        .adopt_frozen(
            1,
            AudioBufferFormat {
                channels: 1,
                frames,
                sample_rate: 48000,
            },
            samples,
        )
        .unwrap();
    let result = registry
        .prepare_snapshot(lease.key(), None)
        .unwrap()
        .commit();
    assert!(registry.release_buffer(lease.key()));
    result
}

fn check_pcm(ctx: &mut AudioContext, value: f32) {
    let mut pcm = [0.0; 128];
    ctx.process_native_device(&mut pcm, 48000, 1).unwrap();
    assert!(
        pcm.iter().all(|sample| sample.to_bits() == value.to_bits()),
        "replacement changed complete source PCM"
    );
}

pub(super) fn legacy_rebindings() {
    for mode in ["clear", "replace", "replace-playing", "deny"] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 16));
        let process = Arc::new(PcmUsage::new(16 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, 1);
        assert!(ctx.create_buffer_source(10));
        ctx.connect(10, crate::context::DESTINATION_NODE_ID);
        let base = ledger.used_bytes();
        let id = ctx
            .create_empty_buffer(1, (BYTES / 4) as u32, 48000)
            .unwrap();
        ctx.copy_to_channel(id, &[0.25; 128], 0, 0).unwrap();
        let old = Arc::downgrade(&ctx.get_buffer(id).unwrap());
        assert!(ctx.set_buffer(10, Some(id)));
        if mode == "replace-playing" {
            assert!(ctx.start_source(10, 0.0, 0.0, None));
            check_pcm(&mut ctx, 0.25);
        }
        assert!(ctx.remove_buffer(id));
        wait(|| ledger.used_bytes() == base + BYTES);
        let next = if mode.starts_with("replace") {
            let id = ctx.create_empty_buffer(1, 256, 48000).unwrap();
            ctx.copy_to_channel(id, &[0.75; 256], 0, 0).unwrap();
            if mode == "replace-playing" {
                ctx.copy_to_channel(id, &[0.5; 128], 0, 0).unwrap();
            }
            Some(id)
        } else {
            None
        };
        let mut contexts = HashMap::from([(1, ctx)]);
        let mut index = NodeContextIndex::new();
        index.register(10, 1);
        let filler = (mode == "deny").then(|| {
            ledger
                .try_reserve(LIMIT - ledger.used_bytes(), "test replacement quota")
                .unwrap()
        });
        let state = Arc::new(Dropped::default());
        let gate = block(&cpu);
        let before = migo_alloc_probe::thread_counts();
        assert!(!set_buffer_scoped(&mut contexts, &index, 2, 10, next));
        assert!(!set_buffer_scoped(&mut contexts, &index, 1, 99, next));
        assert!(!set_buffer_scoped(&mut contexts, &index, 1, 10, Some(999)));
        let accepted = crate::node_retirement::observe_replacement(witness(&state), || {
            set_buffer_scoped(&mut contexts, &index, 1, 10, next)
        });
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = old.upgrade().is_some();
        let held_bytes = usage.snapshot().bytes;
        let queued = state.count.load(Ordering::Acquire);
        gate.open();
        assert!(
            freed < (BYTES / 2) as u64 && held,
            "legacy binding escaped CPU retirement"
        );
        assert_eq!(
            accepted,
            mode != "deny",
            "binding admission changed old state"
        );
        assert_eq!(
            queued, 0,
            "binding destroyed while CPU retirement was blocked"
        );
        assert!(
            held_bytes >= BYTES,
            "binding PCM credit returned before actual drop"
        );
        let ctx = contexts.get_mut(&1).unwrap();
        assert!(ctx.start_source(10, 0.0, 0.0, None));
        check_pcm(
            ctx,
            if mode == "deny" {
                0.25
            } else if mode.starts_with("replace") {
                0.75
            } else {
                0.0
            },
        );
        if mode != "deny" {
            wait(|| old.upgrade().is_none() && state.count.load(Ordering::Acquire) == 1);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "legacy binding escaped CPU retirement"
            );
        } else {
            assert!(old.upgrade().is_some());
            assert_eq!(state.count.load(Ordering::Acquire), 0);
        }
        drop(filler);
        contexts.clear();
        wait(|| {
            ledger.used_bytes() == 0 && usage.snapshot().bytes == 0 && process.snapshot().bytes == 0
        });
    }
}

pub(super) fn snapshot_rebindings() {
    for mode in [
        "clear",
        "replace",
        "replace-playing",
        "replace-start",
        "legacy",
        "deny-start",
    ] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 16));
        let process = Arc::new(PcmUsage::new(16 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, 1);
        assert!(ctx.create_buffer_source(10));
        ctx.connect(10, crate::context::DESTINATION_NODE_ID);
        let buffer = snapshot(0.25, (BYTES / 4) as u32);
        let old = Arc::downgrade(&buffer);
        assert!(ctx.set_started_buffer(10, Some(buffer)));
        if mode == "replace-playing" {
            assert!(ctx.start_source(10, 0.0, 0.0, None));
            check_pcm(&mut ctx, 0.25);
        }
        let legacy = if mode == "legacy" {
            let id = ctx.create_empty_buffer(1, 256, 48000).unwrap();
            ctx.copy_to_channel(id, &[0.75; 256], 0, 0).unwrap();
            Some(id)
        } else {
            None
        };
        let next = (mode.starts_with("replace") || mode == "deny-start").then(|| {
            let mut samples = vec![0.75; 256];
            if mode == "replace-playing" {
                samples[..128].fill(0.5);
            }
            snapshot_pcm(samples)
        });
        let mut contexts = HashMap::from([(1, ctx)]);
        let mut index = NodeContextIndex::new();
        index.register(10, 1);
        let filler = (mode == "deny-start").then(|| {
            ledger
                .try_reserve(LIMIT - ledger.used_bytes(), "test snapshot quota")
                .unwrap()
        });
        let state = Arc::new(Dropped::default());
        let gate = block(&cpu);
        let before = migo_alloc_probe::thread_counts();
        let accepted = crate::node_retirement::observe_replacement(witness(&state), || {
            if mode == "legacy" {
                set_buffer_scoped(&mut contexts, &index, 1, 10, legacy)
            } else if mode == "deny-start" || mode == "replace-start" {
                start_buffer_scoped(&mut contexts, &index, 1, 10, next, 0.0, 0.0, None)
            } else {
                set_started_buffer_scoped(&mut contexts, &index, 1, 10, next)
            }
        });
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = old.upgrade().is_some();
        let queued = state.count.load(Ordering::Acquire);
        gate.open();
        assert!(
            freed < (BYTES / 2) as u64 && held,
            "snapshot binding escaped CPU retirement"
        );
        assert_eq!(
            accepted,
            mode != "deny-start",
            "snapshot admission changed old state"
        );
        assert_eq!(
            queued, 0,
            "snapshot destroyed while CPU retirement was blocked"
        );
        let ctx = contexts.get_mut(&1).unwrap();
        assert_eq!(
            ctx.with_node_typed::<BufferSourceNode, _>(10, |source| {
                source.state() == BufferSourceNode::new(99, 48000).state()
            }),
            Some(mode != "replace-start" && mode != "replace-playing"),
            "replacement advanced or reset source state"
        );
        assert!(ctx.start_source(10, 0.0, 0.0, None));
        check_pcm(
            ctx,
            if mode == "deny-start" {
                0.25
            } else if mode == "clear" {
                0.0
            } else {
                0.75
            },
        );
        if mode != "deny-start" {
            wait(|| old.upgrade().is_none() && state.count.load(Ordering::Acquire) == 1);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "snapshot binding escaped CPU retirement"
            );
        } else {
            assert!(old.upgrade().is_some());
            assert_eq!(state.count.load(Ordering::Acquire), 0);
        }
        drop(filler);
        contexts.clear();
        wait(|| ledger.used_bytes() == 0 && old.upgrade().is_none());
    }
}

pub(super) fn curve_replacements() {
    for mode in ["clear", "replace", "deny"] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 16));
        let process = Arc::new(PcmUsage::new(16 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, 1);
        assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
        let mut curve = Vec::with_capacity(BYTES / 4);
        curve.resize(2, 0.25);
        let retained_bytes = curve.capacity() * 4;
        assert_eq!(retained_bytes, BYTES);
        assert!(ctx.set_wave_shaper_curve(10, Some(curve)));
        let filler = (mode == "deny").then(|| {
            ledger
                .try_reserve(
                    LIMIT - ledger.used_bytes() - BYTES + 1,
                    "test curve capacity quota",
                )
                .unwrap()
        });
        let next = (mode == "replace").then(|| vec![0.75; 2]);
        let state = Arc::new(Dropped::default());
        let gate = block(&cpu);
        let credit = ledger.used_bytes();
        let before = migo_alloc_probe::thread_counts();
        let accepted = crate::node_retirement::observe_replacement(witness(&state), || {
            ctx.set_wave_shaper_curve(10, next)
        });
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let retained_credit = ledger.used_bytes();
        let queued = state.count.load(Ordering::Acquire);
        gate.open();
        assert!(freed < (BYTES / 2) as u64, "curve escaped CPU retirement");
        assert_eq!(
            accepted,
            mode != "deny",
            "curve capacity admission bypassed physical ledger"
        );
        assert_eq!(
            queued, 0,
            "curve destroyed while CPU retirement was blocked"
        );
        if accepted {
            assert!(
                retained_credit >= credit + retained_bytes,
                "curve capacity admission bypassed physical ledger"
            );
            wait(|| state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == credit);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "curve escaped CPU retirement"
            );
        }
        ctx.with_node_typed::<WaveShaperNode, _>(10, |node| {
            let input = [0.0; 128];
            let mut output = [0.0; 128];
            node.process(&input, &mut output, 48000, 1, 0.0);
            let expected: f32 = if mode == "deny" {
                0.25
            } else if mode == "replace" {
                0.75
            } else {
                0.0
            };
            assert!(
                output.iter().all(|v| v.to_bits() == expected.to_bits()),
                "curve replacement changed complete PCM"
            );
        })
        .unwrap();
        drop(filler);
        ctx.close();
        wait(|| ledger.used_bytes() == 0);
    }
}
