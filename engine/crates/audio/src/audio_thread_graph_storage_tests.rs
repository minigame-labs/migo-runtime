//! Graph vector ownership, transactional growth and real CPU destruction.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::graph_vec::{GraphVec, Plan};
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::GainNode;
use std::sync::atomic::Ordering;

const LIMIT: usize = 32 << 20;
const FIELDS: [&str; 18] = [
    "connections",
    "processing_order",
    "dense_ids",
    "in_degree",
    "input_start",
    "input_edges",
    "output_start",
    "output_edges",
    "ready_queue",
    "edge_src",
    "edge_src_port",
    "edge_dst_port",
    "collected",
    "ended",
    "mix_buffer",
    "native_input",
    "native_collected",
    "buffer_pool",
];
fn graph(cpu: &migo_io::audio_cpu::AudioCpu, ledger: &Arc<AudioAggregateLedger>) -> AudioContext {
    context(
        cpu,
        ledger,
        &Arc::new(PcmUsage::new(LIMIT, 64)),
        &Arc::new(PcmUsage::new(LIMIT, 64)),
        2,
    )
}

#[test]
fn all_eighteen_capacities_adopt_without_copy_and_keep_credit_until_cpu_drop() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut control = graph(&cpu, &ledger);
    let default_bytes: usize = control
        .graph_vector_identities()
        .iter()
        .map(|row| row.4)
        .sum();
    let baseline = ledger.used_bytes();
    let mut ctx = AudioContext::new(2, 48000, 2);
    for name in FIELDS {
        ctx.set_native_graph_capacity(name, 1025);
    }
    let original = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let bytes: usize = original.iter().map(|row| row.4).sum();
    assert_eq!(original.len(), 18);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    assert!(
        ledger.used_bytes() - baseline >= baseline - default_bytes + bytes + 18 * 4096,
        "graph adoption omitted actual vector capacities"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        original,
        "graph adoption copied original allocations"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "graph adoption changed original values"
    );
    let host = ctx.response_retirement_host().unwrap().clone();
    let held = ledger.used_bytes();
    ctx.prepare_graph_vectors_for_test(&host).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        held,
        "graph adoption charged duplicate credit"
    );
    let state = Arc::new(Dropped::default());
    for name in FIELDS {
        ctx.observe_graph_vector(name, witness(&state));
    }
    let gate = block(&cpu);
    scheduler.close();
    ctx.close();
    drop(ctx);
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "graph capacity destroyed before original CPU drop"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "graph credit returned before physical destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 18);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "graph storage escaped original CPU"
    );
    drop(host);
    control.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn node_batch_refusal_precedes_every_vector_allocation_and_mutation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let original = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let size = 16385 * 4;
    let available = 7 * (size + 5000);
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - available,
            "late node batch peer",
        )
        .unwrap();
    let held = ledger.used_bytes();
    let begin = migo_alloc_probe::thread_counts();
    assert!(
        ctx.grow_graph_nodes_for_test(16385).is_err(),
        "node graph growth bypassed physical admission"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - begin.bytes_allocated;
    assert!(
        allocated < size as u64,
        "graph vectors allocated before complete batch admission"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        original,
        "refused node batch changed vector allocation"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "refused node batch changed graph state"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "refused node batch leaked capacity credit"
    );
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn failed_node_insertion_keeps_graph_and_recycled_buffer() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = AudioContext::new(1, 48000, 2);
    assert!(ctx.create_gain(10));
    ctx.release_node(10);
    for name in [
        "dense_ids",
        "in_degree",
        "processing_order",
        "ready_queue",
        "collected",
        "native_collected",
    ] {
        ctx.set_native_graph_capacity(name, 1);
    }
    for name in ["input_start", "output_start"] {
        ctx.set_native_graph_capacity(name, 2);
    }
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    ctx.collect_device_controls();
    let identity = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let pool = ctx.render_pool_identity();
    let node_fee = std::mem::size_of::<GainNode>() + 4096;
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - node_fee - 127,
            "node graph peer",
        )
        .unwrap();
    assert!(
        !ctx.create_gain(20),
        "node insertion ignored vector admission"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        identity,
        "failed insertion changed graph capacity"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "failed insertion changed graph values or flags"
    );
    assert_eq!(
        ctx.render_pool_identity(),
        pool,
        "failed insertion consumed recycled buffer"
    );
    assert!(
        !ctx.node_is::<GainNode>(20),
        "failed graph insertion installed node"
    );
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn edge_refusal_preserves_connections_and_dc_liveness() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = AudioContext::new(1, 48000, 2);
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(30))));
    assert!(ctx.set_wave_shaper_curve(30, Some(vec![0.75, 0.75])));
    ctx.connect(900, 901);
    for name in [
        "connections",
        "input_edges",
        "output_edges",
        "edge_src",
        "edge_src_port",
        "edge_dst_port",
    ] {
        ctx.set_native_graph_capacity(name, 1);
    }
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    ctx.collect_device_controls();
    let identity = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes() - 239, "late edge batch peer")
        .unwrap();
    assert!(
        ctx.try_connect_ports(30, 0, crate::context::DESTINATION_NODE_ID, 0)
            .is_err(),
        "edge batch bypassed vector admission"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        identity,
        "refused edge batch changed allocation"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "refused edge batch changed connection or controls"
    );
    assert!(
        !ctx.has_active_sources(),
        "refused connection enabled DC source"
    );
    drop(filler);
    assert!(
        ctx.try_connect_ports(30, 0, crate::context::DESTINATION_NODE_ID, 0)
            .unwrap()
    );
    assert!(ctx.has_active_sources());
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn ended_watch_refusal_keeps_original_watchers_and_flags() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    assert!(ctx.create_gain(10));
    ctx.collect_device_controls();
    let identity = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "ended notification peer")
        .unwrap();
    assert!(
        !ctx.watch_source_ended(10, true),
        "ended watch bypassed vector admission"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        identity,
        "refused ended watch changed capacity"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "refused ended watch changed watchers or controls"
    );
    drop(filler);
    assert!(ctx.watch_source_ended(10, true));
    let credit = ledger.used_bytes();
    assert_eq!(ctx.drain_ended().count(), 0);
    assert_eq!(ledger.used_bytes(), credit);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn growth_preserves_all_prefixes_and_keeps_old_cpu_credit() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let mut data = GraphVec::from_vec((0..1024).map(|i| i as f32 * 0.125).collect());
    data.prepare(&host).unwrap();
    let original = data.as_slice().to_vec();
    let old_bytes = data.capacity() * 4;
    let credit = ledger.used_bytes();
    let state = Arc::new(Dropped::default());
    data.observe(witness(&state));
    let gate = block(&cpu);
    let plan = Plan::reserve(&data, Some(&host), 4097).unwrap().unwrap();
    plan.install(&mut data);
    assert!(
        data.iter()
            .zip(&original)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "graph vector growth lost original prefix"
    );
    assert!(
        ledger.used_bytes() >= credit + data.capacity() * 4 + 4096,
        "graph growth returned old data credit early"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    wait(|| ledger.used_bytes() < credit + data.capacity() * 4 + 4096);
    assert!(!state.wrong.load(Ordering::Acquire));
    assert!(old_bytes >= 4096);
    drop(data);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

struct Element(Arc<Dropped>);
impl Drop for Element {
    fn drop(&mut self) {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            self.0.wrong.store(true, Ordering::Release);
        }
        self.0.count.fetch_add(1, Ordering::Release);
    }
}
#[test]
fn live_cpu_vector_transfers_real_payload_without_allocating_on_drop() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let state = Arc::new(Dropped::default());
    let mut native = Vec::with_capacity(2048);
    native.push(Element(state.clone()));
    let mut data = GraphVec::from_vec(native);
    data.prepare(&host).unwrap();
    let held = ledger.used_bytes();
    let gate = block(&cpu);
    scheduler.close();
    let begin = migo_alloc_probe::thread_counts();
    drop(data);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - begin.allocations,
            end.reallocations - begin.reallocations
        ),
        (0, 0),
        "graph drop allocated a late handoff"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "graph vector payload dropped away from original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "graph vector credit returned before payload destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "graph vector payload escaped original CPU"
    );
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn inline_bulk_boundary_reuse_and_foreign_ledger_remain_exact() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let other = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let foreign = crate::node_retirement::Host::new(cpu, other.clone(), 2).unwrap();
    for count in [1023, 1024] {
        let mut data = GraphVec::from_vec(vec![0u32; count]);
        let baseline = ledger.used_bytes();
        data.prepare(&host).unwrap();
        if count == 1023 {
            assert_eq!(
                ledger.used_bytes() - baseline,
                4092,
                "small graph vector acquired artificial handoff"
            );
        } else {
            assert!(
                ledger.used_bytes() >= baseline + 4096 + 4096,
                "bulk graph vector omitted CPU handoff"
            );
        }
        let held = ledger.used_bytes();
        let pointer = data.as_ptr() as usize;
        assert!(
            data.prepare(&foreign).is_err(),
            "graph vector accepted foreign ledger"
        );
        data.prepare(&host).unwrap();
        assert_eq!(data.as_ptr() as usize, pointer);
        assert_eq!(
            ledger.used_bytes(),
            held,
            "reused graph vector charged twice"
        );
        data.clear();
        data.resize(count, 1);
        assert_eq!(ledger.used_bytes(), held);
    }
    drop((host, foreign));
    wait(|| ledger.used_bytes() == 0 && other.used_bytes() == 0);
}

#[test]
fn closed_host_reuses_capacity_but_refuses_new_growth() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let mut data = GraphVec::from_vec(vec![0.25f32; 1024]);
    data.prepare(&host).unwrap();
    let held = ledger.used_bytes();
    let pointer = data.as_ptr() as usize;
    scheduler.close();
    assert!(
        Plan::reserve(&data, Some(&host), 1024).is_ok_and(|plan| plan.is_none()),
        "existing graph capacity required fresh admission"
    );
    data.clear();
    data.resize(1024, 0.5);
    assert_eq!(ledger.used_bytes(), held);
    assert_eq!(data.as_ptr() as usize, pointer);
    assert!(
        Plan::reserve(&data, Some(&host), 1025).is_err(),
        "closed host admitted fresh graph capacity"
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data.push(0.75)));
    assert!(
        result.is_err(),
        "managed graph vector escaped admitted capacity"
    );
    assert_eq!(data.len(), 1024);
    drop(data);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn active_topology_survives_owned_growth_and_renders_without_heap() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    assert!(ctx.add_node(Box::new(OscillatorNode::new(10, 48000))));
    ctx.with_node_typed::<OscillatorNode, _>(10, |n| n.start(0.0));
    for id in 20..100 {
        assert!(ctx.create_gain(id));
        ctx.connect(10, id);
        ctx.connect(id, crate::context::DESTINATION_NODE_ID);
    }
    let mut output = [0.0; 256];
    let begin = migo_alloc_probe::thread_counts();
    ctx.process_native_device(&mut output, 48000, 2).unwrap();
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - begin.allocations,
            end.reallocations - begin.reallocations
        ),
        (0, 0),
        "owned CSR cold render allocated"
    );
    assert!(output.iter().any(|v| v.abs() > 1e-5));
    let before: Vec<_> = ctx
        .graph_vector_identities()
        .into_iter()
        .map(|(n, _, c, p, b)| (n, c, p, b))
        .collect();
    ctx.disconnect(10, None);
    let after: Vec<_> = ctx
        .graph_vector_identities()
        .into_iter()
        .map(|(n, _, c, p, b)| (n, c, p, b))
        .collect();
    assert_eq!(before, after, "disconnect released live graph capacity");
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
