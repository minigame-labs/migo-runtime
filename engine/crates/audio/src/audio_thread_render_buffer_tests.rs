//! Active, pooled, and retired graph render storage retains physical credit.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::{AudioNodeProcessor, ChannelMergerNode, ChannelSplitterNode, GainNode};
use crate::render_buffer::RenderBuffer;
use std::sync::atomic::Ordering;

const LIMIT: usize = 32 << 20;
struct Probe {
    id: u32,
    calls: u32,
    finished: bool,
    dropped: Arc<Dropped>,
}
impl Drop for Probe {
    fn drop(&mut self) {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            self.dropped.wrong.store(true, Ordering::Release);
        }
        self.dropped.count.fetch_add(1, Ordering::Release);
    }
}
impl AudioNodeProcessor for Probe {
    fn id(&self) -> u32 {
        self.id
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_finished(&self) -> bool {
        self.finished
    }
    fn process(
        &mut self,
        _input: &[f32],
        output: &mut [f32],
        _rate: u32,
        channels: u32,
        _time: f64,
    ) -> usize {
        if self.id != 99 || self.calls == 0 {
            output.fill(0.25);
        } else {
            output[0] = 0.25;
        }
        self.calls += 1;
        output.len() / channels.max(1) as usize
    }
}
fn probe(id: u32, dropped: &Arc<Dropped>) -> Box<dyn AudioNodeProcessor> {
    Box::new(Probe {
        id,
        calls: 0,
        finished: false,
        dropped: dropped.clone(),
    })
}
fn graph(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> AudioContext {
    context(
        cpu,
        ledger,
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        channels,
    )
}
fn node_fee() -> usize {
    std::mem::size_of::<Probe>() + 4096
}

#[test]
fn pooled_capacity_outlives_node_and_reuse_keeps_original_pointer() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 2);
    let baseline = ledger.used_bytes();
    let nodes = Arc::new(Dropped::default());
    assert!(ctx.add_node(probe(10, &nodes)));
    let identity = ctx.render_buffer_identity(10).unwrap();
    let bytes = identity.2 * 4;
    let active = ledger.used_bytes();
    assert_eq!(
        active - baseline,
        node_fee() + bytes,
        "node lease duplicated render capacity credit"
    );
    ctx.release_node(10);
    wait(|| nodes.count.load(Ordering::Acquire) == 1);
    wait(|| ledger.used_bytes() <= baseline + bytes);
    assert!(
        ledger.used_bytes() >= baseline + bytes,
        "pooled render capacity returned with node credit"
    );
    assert!(!nodes.wrong.load(Ordering::Acquire));
    assert_eq!(ctx.render_pool_identity(), vec![identity]);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes() - node_fee(), "pool reuse peer")
        .unwrap();
    assert!(
        ctx.add_node(probe(11, &nodes)),
        "pooled render reuse acquired duplicate admission"
    );
    let reused = ctx.render_buffer_identity(11).unwrap();
    assert_eq!(
        reused, identity,
        "pooled render buffer lost original allocation"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn bulk_pool_overflow_and_closed_teardown_use_original_cpu() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    let baseline = ledger.used_bytes();
    let nodes = Arc::new(Dropped::default());
    let data = Arc::new(Dropped::default());
    for id in 10..30 {
        assert!(ctx.add_node(probe(id, &nodes)));
        ctx.observe_render_buffer(id, witness(&data));
    }
    for id in 10..30 {
        ctx.with_node_typed::<Probe, _>(id, |node| node.finished = true);
    }
    let bytes = ctx.render_buffer_identity(10).unwrap().2 * 4;
    let active = ledger.used_bytes();
    let gate = block(&cpu);
    let start = migo_alloc_probe::thread_counts();
    ctx.collect_device_controls();
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - start.allocations,
            end.reallocations - start.reallocations
        ),
        (0, 0),
        "render collection allocated"
    );
    assert!(
        end.bytes_freed - start.bytes_freed < bytes as u64,
        "render collection freed bulk storage on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        active,
        "render collection returned credit before CPU destruction"
    );
    assert_eq!(ctx.render_pool_identity().len(), 16);
    gate.open();
    wait(|| nodes.count.load(Ordering::Acquire) == 20 && data.count.load(Ordering::Acquire) == 4);
    wait(|| ledger.used_bytes() <= active - 20 * node_fee() - 4 * (bytes + 4096));
    assert!(
        ledger.used_bytes() >= baseline + 16 * (bytes + 4096),
        "bulk pool lost live capacity credit"
    );
    let gate = block(&cpu);
    host.close();
    let held = ledger.used_bytes();
    let start = migo_alloc_probe::thread_counts();
    ctx.close();
    drop(ctx);
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - start.bytes_freed < bytes as u64,
        "closed context freed pooled bulk storage on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "pooled credit returned before original CPU drop"
    );
    gate.open();
    wait(|| data.count.load(Ordering::Acquire) == 20 && ledger.used_bytes() == 0);
    assert!(
        !nodes.wrong.load(Ordering::Acquire) && !data.wrong.load(Ordering::Acquire),
        "render storage escaped original CPU"
    );
}

#[test]
fn buffer_refusal_precedes_allocation_and_preserves_existing_node() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    let original = Arc::new(Dropped::default());
    let rejected = Arc::new(Dropped::default());
    assert!(ctx.add_node(probe(10, &original)));
    let identity = ctx.render_buffer_identity(10).unwrap();
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - node_fee(),
            "render buffer peer",
        )
        .unwrap();
    let start = migo_alloc_probe::thread_counts();
    let accepted = ctx.add_node(probe(10, &rejected));
    let bytes = migo_alloc_probe::thread_counts().bytes_allocated - start.bytes_allocated;
    assert!(!accepted, "render capacity admission bypassed");
    assert!(
        bytes < (identity.2 * 4) as u64,
        "render storage allocated before admission"
    );
    assert!(
        ctx.node_is::<Probe>(10),
        "render refusal removed original node"
    );
    assert_eq!(
        ctx.render_buffer_identity(10).unwrap(),
        identity,
        "render refusal replaced original buffer"
    );
    assert_eq!(
        original.count.load(Ordering::Acquire),
        0,
        "render refusal replaced original node"
    );
    wait(|| rejected.count.load(Ordering::Acquire) == 1);
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
    assert!(
        !rejected.wrong.load(Ordering::Acquire),
        "rejected prepared node escaped CPU retirement"
    );
}

#[test]
fn migration_adopts_active_and_pooled_actual_capacity_without_copying() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut empty = graph(&cpu, &ledger, 2);
    let baseline = ledger.used_bytes();
    let mut native = AudioContext::new(2, 48000, 2);
    let nodes = Arc::new(Dropped::default());
    assert!(native.add_node(probe(10, &nodes)));
    native.reserve_native_render_buffer(10, 262144);
    let pooled = native.render_buffer_identity(10).unwrap();
    native.release_node(10);
    native.reserve_native_render_buffer(crate::context::DESTINATION_NODE_ID, 8192);
    let destination = native
        .render_buffer_identity(crate::context::DESTINATION_NODE_ID)
        .unwrap();
    native
        .prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let delta = ledger.used_bytes() - baseline;
    assert!(
        delta >= baseline + (pooled.2 + destination.2) * 4 + 2 * 4096 - 1024,
        "pooled render capacity omitted during migration"
    );
    assert_eq!(
        native.render_pool_identity(),
        vec![pooled],
        "pooled adoption copied original allocation"
    );
    assert_eq!(
        native
            .render_buffer_identity(crate::context::DESTINATION_NODE_ID)
            .unwrap(),
        destination,
        "active adoption copied original allocation"
    );
    let credit = ledger.used_bytes();
    let owner = native.response_retirement_host().unwrap().clone();
    let mut data = RenderBuffer::new(Some(&owner), 128).unwrap();
    let extra = ledger.used_bytes();
    data.prepare(&owner).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        extra,
        "render adoption acquired duplicate credit"
    );
    assert!(extra > credit);
    drop(data);
    drop(owner);
    native.close();
    empty.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn small_bulk_boundary_and_closed_reuse_keep_physical_credit() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 2);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let base = ledger.used_bytes();
    let mut small = RenderBuffer::new(Some(&owner), 1023).unwrap();
    assert_eq!(
        ledger.used_bytes() - base,
        4092,
        "small render buffer acquired artificial handoff"
    );
    let mut bulk = RenderBuffer::new(Some(&owner), 1024).unwrap();
    assert!(
        ledger.used_bytes() >= base + 4092 + 4096 + 4096,
        "bulk render buffer omitted CPU handoff credit"
    );
    let pointer = bulk.as_slice().as_ptr() as usize;
    let held = ledger.used_bytes();
    host.close();
    let start = migo_alloc_probe::thread_counts();
    small.clear();
    small.resize(1023, 0.5);
    bulk.clear();
    bulk.resize(1024, 0.75);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - start.allocations,
            end.reallocations - start.reallocations
        ),
        (0, 0),
        "prepared render reuse allocated"
    );
    assert_eq!(bulk.as_slice().as_ptr() as usize, pointer);
    assert_eq!(ledger.used_bytes(), held);
    assert!(
        RenderBuffer::new(Some(&owner), 1024).is_err(),
        "closed host admitted fresh render capacity"
    );
    drop(small);
    drop(bulk);
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn foreign_adoption_refusal_preserves_original_cpu_and_capacity() {
    let first = migo_io::scheduler::IoScheduler::new(1);
    let second = migo_io::scheduler::IoScheduler::new(1);
    let cpu = first.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let other_ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let owner = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let foreign =
        crate::node_retirement::Host::new(second.audio_cpu(), other_ledger.clone(), 2).unwrap();
    let same = crate::node_retirement::Host::new(second.audio_cpu(), ledger.clone(), 2).unwrap();
    for samples in [1023, 4096] {
        let mut data = RenderBuffer::new(None, samples).unwrap();
        let pointer = data.as_slice().as_ptr() as usize;
        data.prepare(&owner).unwrap();
        let held = ledger.used_bytes();
        assert!(
            data.prepare(&foreign).is_err(),
            "render buffer accepted foreign physical ledger"
        );
        assert_eq!(data.as_slice().as_ptr() as usize, pointer);
        assert_eq!(ledger.used_bytes(), held);
        data.prepare(&same).unwrap();
        assert_eq!(
            ledger.used_bytes(),
            held,
            "same-ledger render adoption charged twice"
        );
        if samples == 4096 {
            let state = Arc::new(Dropped::default());
            data.observe(witness(&state));
            let gate = block(&cpu);
            first.close();
            drop(data);
            assert_eq!(
                state.count.load(Ordering::Acquire),
                0,
                "render storage destroyed away from blocked original CPU"
            );
            assert_eq!(
                ledger.used_bytes(),
                held,
                "render credit returned before original CPU destructor"
            );
            gate.open();
            wait(|| state.count.load(Ordering::Acquire) == 1);
            assert!(!state.wrong.load(Ordering::Acquire));
        }
    }
    drop((owner, foreign, same));
    wait(|| ledger.used_bytes() == 0 && other_ledger.used_bytes() == 0);
}

#[test]
fn refused_native_adoption_keeps_pointer_and_original_samples() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let owner = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let mut data = RenderBuffer::new(None, 4096).unwrap();
    data.as_mut_slice().fill(0.25);
    let pointer = data.as_slice().as_ptr() as usize;
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "native render adoption peer")
        .unwrap();
    assert!(
        data.prepare(&owner).is_err(),
        "native render adoption bypassed physical admission"
    );
    assert_eq!(
        data.as_slice().as_ptr() as usize,
        pointer,
        "refused render adoption consumed allocation"
    );
    assert!(
        data.as_slice()
            .iter()
            .all(|v| v.to_bits() == 0.25f32.to_bits()),
        "refused render adoption changed PCM"
    );
    drop(filler);
    data.prepare(&owner).unwrap();
    drop(data);
    drop(owner);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn managed_resize_cannot_escape_admitted_capacity() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let owner = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let mut data = RenderBuffer::new(Some(&owner), 1024).unwrap();
    let pointer = data.as_slice().as_ptr() as usize;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data.resize(4097, 1.0)));
    assert!(
        result.is_err(),
        "managed render buffer resized past admitted capacity"
    );
    assert_eq!(data.capacity(), 1024);
    assert_eq!(data.len(), 1024);
    assert_eq!(data.as_slice().as_ptr() as usize, pointer);
    let mut native = RenderBuffer::new(None, 1).unwrap();
    native.as_mut_slice()[0] = 0.75;
    native.resize(4097, 0.25);
    assert_eq!(native.as_slice()[0], 0.75);
    assert_eq!(native.len(), 4097);
    drop(data);
    drop(owner);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn complete_graph_pcm_and_collection_receipt() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut audible = false;
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for topology in 0..2 {
                for mode in 0..3 {
                    let mut ctx = if mode == 1 {
                        AudioContext::new_managed(1, rate, channels, cpu.clone(), ledger.clone())
                            .unwrap()
                    } else {
                        AudioContext::new(1, rate, channels)
                    };
                    ctx.set_test_dense_order();
                    assert!(ctx.add_node(Box::new(OscillatorNode::new(10, rate))));
                    assert!(ctx.create_gain(20));
                    assert!(ctx.set_node_param(20, "gain", 0.25));
                    assert!(ctx.set_node_param(10, "frequency", 317.0));
                    assert!(ctx.watch_source_ended(10, true));
                    assert!(ctx.param_linear_ramp(20, "gain", 0.75, 0.01));
                    if topology == 0 {
                        ctx.connect(10, 20);
                        ctx.connect(20, crate::context::DESTINATION_NODE_ID);
                    } else {
                        assert!(ctx.add_node(Box::new(ChannelSplitterNode::new(11, 2))));
                        assert!(ctx.add_node(Box::new(ChannelMergerNode::new(12, 2))));
                        assert!(ctx.add_node(Box::new(GainNode::new(21))));
                        assert!(ctx.set_node_param(21, "gain", 0.125));
                        assert!(ctx.add_node(Box::new(DelayNode::new(40, 0.002, rate, channels))));
                        assert!(ctx.set_node_param(40, "delayTime", 0.0005));
                        ctx.connect(10, 11);
                        ctx.connect_ports(11, 0, 20, 0);
                        ctx.connect_ports(11, 1, 21, 0);
                        ctx.connect_ports(20, 0, 12, 0);
                        ctx.connect_ports(21, 0, 12, 1);
                        ctx.connect(12, 40);
                        ctx.connect(40, 20);
                        ctx.connect(40, crate::context::DESTINATION_NODE_ID);
                    }
                    if mode == 2 {
                        ctx.process(&mut vec![0.0; 4098 * channels as usize]);
                    }
                    if mode == 2 {
                        ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
                            .unwrap();
                    }
                    let start = ctx.current_time();
                    ctx.with_node_typed::<OscillatorNode, _>(10, |n| {
                        n.start(start);
                        n.stop(start + 0.012);
                    });
                    for frames in [1, 127, 128, 129, 257, 4097] {
                        let mut output = vec![
                            0.375;
                            frames * channels as usize
                                + channels.saturating_sub(1) as usize
                        ];
                        let begin = migo_alloc_probe::thread_counts();
                        let collected = if mode == 1 {
                            ctx.process_native_device(&mut output, rate, channels)
                                .unwrap()
                        } else {
                            ctx.process(&mut output)
                        };
                        let end = migo_alloc_probe::thread_counts();
                        if mode != 0 {
                            assert_eq!(
                                (
                                    end.allocations - begin.allocations,
                                    end.reallocations - begin.reallocations
                                ),
                                (0, 0),
                                "prepared graph render reached allocator"
                            );
                            // A completed oneshot can free its small metadata on
                            // this caller. The actual pooled PCM stays alive.
                            assert!(
                                end.bytes_freed - begin.bytes_freed < (128 * channels as u64 * 4),
                                "prepared graph render freed PCM on caller"
                            );
                        }
                        let mut collected = collected.to_vec();
                        collected.sort_unstable();
                        let mut ended: Vec<_> = ctx.drain_ended().collect();
                        ended.sort_unstable();
                        let bits: Vec<_> = output.iter().map(|v| v.to_bits()).collect();
                        audible |= output.iter().any(|v| (*v - 0.375).abs() > 1e-6);
                        println!(
                            "MIGO_RENDER_PCM {rate}:{channels}:{topology}:{mode}:{frames} {:?} {:?} {:?} {}",
                            bits,
                            collected,
                            ended,
                            ctx.current_time().to_bits()
                        );
                    }
                    ctx.close();
                }
            }
        }
    }
    wait(|| ledger.used_bytes() == 0);
    assert!(audible, "render receipt contained no nonzero graph PCM");
}

#[test]
fn additive_bus_and_partial_writes_preserve_original_zeroing() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let nodes = Arc::new(Dropped::default());
    let mut ctx = graph(&cpu, &ledger, 2);
    assert!(ctx.add_node(probe(99, &nodes)));
    ctx.connect(99, crate::context::DESTINATION_NODE_ID);
    let mut output = [0.375; 256];
    ctx.process(&mut output);
    assert!(
        output.iter().all(|v| *v == 0.625),
        "context rendering stopped adding to shared bus"
    );
    output.fill(0.375);
    ctx.process(&mut output);
    assert!(
        output[0] == 0.625 && output[1..].iter().all(|v| *v == 0.375),
        "partial render buffer retained stale samples"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
