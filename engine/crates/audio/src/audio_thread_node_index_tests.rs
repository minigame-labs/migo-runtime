//! Actor index admission, original factory routes and physical table lifetime.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::graph_table_layout::Layout;
use crate::node_retirement::{HANDOFF_BYTES, Host};
use std::cell::Cell;
use std::sync::atomic::Ordering;
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
fn bytes(entries: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let native = HashMap::<u32, u32>::with_capacity(entries);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout = Layout::for_entries::<(u32, u32)>(native.capacity()).unwrap();
    assert_eq!(allocated, layout.bytes as u64);
    layout.bytes
}
fn fee(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    let used = ledger.used_bytes();
    let quote = host
        .reserve_value_with_bytes::<Option<HashMap<u32, u32>>>(bytes)
        .unwrap();
    let amount = ledger.used_bytes() - used;
    drop(quote);
    amount
}
fn fill(index: &mut NodeContextIndex, count: usize) {
    for id in 1..=count as u32 {
        assert!(index.create(id, 1 + id % 2, || true));
    }
}
fn commands(ctx_id: u32, start: u32) -> Vec<AudioCmd> {
    vec![
        AudioCmd::CreateBufferSource {
            ctx_id,
            node_id: start,
        },
        AudioCmd::CreateGain {
            ctx_id,
            node_id: start + 1,
        },
        AudioCmd::CreateOscillator {
            ctx_id,
            node_id: start + 2,
        },
        AudioCmd::CreateDelay {
            ctx_id,
            node_id: start + 3,
            max_delay_time: 0.001,
        },
        AudioCmd::CreateBiquadFilter {
            ctx_id,
            node_id: start + 4,
        },
        AudioCmd::CreateWaveShaper {
            ctx_id,
            node_id: start + 5,
        },
        AudioCmd::CreateAnalyser {
            ctx_id,
            node_id: start + 6,
        },
        AudioCmd::CreateDynamicsCompressor {
            ctx_id,
            node_id: start + 7,
        },
        AudioCmd::CreatePanner {
            ctx_id,
            node_id: start + 8,
        },
        AudioCmd::CreateChannelMerger {
            ctx_id,
            node_id: start + 9,
            number_of_inputs: 2,
        },
        AudioCmd::CreateChannelSplitter {
            ctx_id,
            node_id: start + 10,
            number_of_outputs: 2,
        },
        AudioCmd::CreateConstantSource {
            ctx_id,
            node_id: start + 11,
        },
        AudioCmd::CreateIIRFilter {
            ctx_id,
            node_id: start + 12,
            feedforward: vec![1.0, 0.2],
            feedback: vec![1.0, -0.1],
        },
    ]
}
#[test]
fn initial_index_charges_actual_standard_table_before_allocating() {
    let (_scheduler, host, ledger) = setup();
    let expected = bytes(64);
    let baseline = ledger.used_bytes();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    assert_eq!(
        index.allocation_bytes(),
        expected,
        "index constructor changed actual table capacity"
    );
    assert_eq!(
        ledger.used_bytes() - baseline,
        expected,
        "index constructor omitted actual table allocation charge"
    );
    fill(&mut index, 64);
    index.clear_all();
    assert_eq!(
        ledger.used_bytes() - baseline,
        expected,
        "empty index returned live table credit"
    );
    drop(index);
    assert_eq!(ledger.used_bytes(), baseline);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn saturated_index_constructor_refuses_before_bucket_allocation() {
    let (_scheduler, host, ledger) = setup();
    let expected = bytes(64);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "index peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = NodeContextIndex::with_host(host.clone());
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "index constructor bypassed admission");
    assert!(
        allocated < expected as u64,
        "index allocated buckets before admission"
    );
    drop(filler);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn index_refusal_precedes_factory_and_preserves_every_existing_entry() {
    let (_scheduler, host, ledger) = setup();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let capacity = index.identity().1;
    fill(&mut index, capacity);
    let original = index.identity();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "full index peer")
        .unwrap();
    let called = Cell::new(false);
    let made = index.create(10000, 7, || {
        called.set(true);
        true
    });
    assert!(!called.get(), "index refusal entered node factory");
    assert!(!made);
    assert_eq!(
        index.identity(),
        original,
        "index refusal changed existing entries"
    );
    drop(filler);
    drop((index, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn all_thirteen_factory_routes_refuse_without_changing_graph_or_index() {
    let (_scheduler, host, ledger) = setup();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let capacity = index.identity().1;
    fill(&mut index, capacity);
    // An independently unmanaged graph makes any premature factory mutation
    // observable even when the actor index's ledger is completely full.
    let mut contexts = HashMap::from([(7, AudioContext::new(7, 44100, 2))]);
    let graph = contexts[&7].graph_test_state();
    let original = index.identity();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "factory index peer")
        .unwrap();
    for command in commands(7, 10000) {
        assert!(!node_creation::apply(
            &command,
            &mut contexts,
            &mut index,
            48000
        ));
        assert_eq!(
            contexts[&7].graph_test_state(),
            graph,
            "index admission changed graph before refusing"
        );
        assert_eq!(index.identity(), original, "factory refusal changed index");
    }
    drop(filler);
    drop((index, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn failed_factory_never_installs_reserved_table_or_publishes_binding() {
    let (_scheduler, host, ledger) = setup();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let capacity = index.identity().1;
    fill(&mut index, capacity);
    let original = index.identity();
    let held = ledger.used_bytes();
    let target = bytes(capacity + 1);
    let before = migo_alloc_probe::thread_counts();
    assert!(
        !index.create(10000, 7, || false),
        "index published binding after failed factory"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert_eq!(
        index.identity(),
        original,
        "failed factory installed index table"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "failed factory retained index reservation"
    );
    assert!(
        allocated < target as u64,
        "failed factory allocated index buckets"
    );
    assert_eq!(
        index.get_context(10000),
        None,
        "failed factory published binding"
    );
    drop((index, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn growth_and_drop_keep_old_and_new_bulk_tables_charged_until_original_cpu_drop() {
    let (scheduler, host, ledger) = setup();
    let cpu = scheduler.audio_cpu();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    // Quote before growth starts retiring other tables on the CPU.
    let old_bytes = bytes(896);
    let expected = HANDOFF_BYTES + fee(&host, &ledger, old_bytes);
    fill(&mut index, 896);
    assert_eq!(index.identity().1, 896);
    wait(|| ledger.used_bytes() == expected);
    let state = Arc::new(Dropped::default());
    index.observe(witness(&state));
    let old_rows = index.identity().2;
    let gate = block(&cpu);
    let before_growth = migo_alloc_probe::thread_counts();
    assert!(index.create(10000, 7, || true));
    let after_growth = migo_alloc_probe::thread_counts();
    assert!(
        after_growth.bytes_freed - before_growth.bytes_freed < old_bytes as u64,
        "index growth freed old buckets on caller"
    );
    assert_eq!(
        index.get_context(10000),
        Some(7),
        "index success lost new binding"
    );
    for (id, owner, _) in old_rows {
        assert_eq!(
            index.get_context(id),
            Some(owner),
            "index growth lost existing binding"
        );
    }
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "old index table escaped original CPU"
    );
    assert!(
        ledger.used_bytes() >= expected + bytes(897),
        "index growth failed to charge overlapping tables"
    );
    index.observe(witness(&state));
    scheduler.close();
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop((index, host));
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "index drop allocated a late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < old_bytes as u64,
        "index drop freed live buckets on caller"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "live index table escaped original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "index credit returned before actual CPU destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 2 && ledger.used_bytes() == 0);
    assert!(!state.wrong.load(Ordering::Acquire));
}
#[test]
fn replacement_removal_clear_and_closed_cpu_reuse_keep_live_allocation_credit() {
    let (scheduler, host, ledger) = setup();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let capacity = index.identity().1;
    fill(&mut index, capacity);
    let held = ledger.used_bytes();
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    assert!(index.create(1, 9, || true));
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "full index replacement allocated"
    );
    assert_eq!(
        index.get_context(1),
        Some(9),
        "index replacement lost owner"
    );
    index.clear_context(2);
    assert_eq!(index.get_context(2), Some(1));
    assert_eq!(
        index.get_context(3),
        None,
        "context clear left stale index binding"
    );
    index.unregister(2);
    assert_eq!(
        index.get_context(2),
        None,
        "node unregister left stale binding"
    );
    index.clear_all();
    assert_eq!(index.identity().0, 0, "index clear all left stale bindings");
    assert_eq!(
        ledger.used_bytes(),
        held,
        "index deletion returned allocated capacity credit"
    );
    assert!(index.create(9000, 8, || true));
    assert_eq!(ledger.used_bytes(), held);
    fill(&mut index, capacity - 1);
    let called = Cell::new(false);
    assert!(!index.create(10000, 8, || {
        called.set(true);
        false
    }));
    assert!(!called.get(), "closed CPU index growth entered factory");
    drop((index, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn borrowed_iir_index_refusal_keeps_original_transport_ticket_until_cpu_drop() {
    let (scheduler, host, ledger) = setup();
    let cpu = scheduler.audio_cpu();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let capacity = index.identity().1;
    fill(&mut index, capacity);
    let mut contexts = HashMap::from([(7, AudioContext::new(7, 48000, 2))]);
    let original = contexts[&7].graph_test_state();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "IIR index peer")
        .unwrap();
    let mut feedforward = Vec::with_capacity((4 << 20) / 8);
    feedforward.extend_from_slice(&[1.0, 0.2]);
    let command = AudioCmd::CreateIIRFilter {
        ctx_id: 7,
        node_id: 10000,
        feedforward,
        feedback: vec![1.0, -0.1],
    };
    let bytes = command.queued_payload_bytes();
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(command).unwrap();
    let byte_filler = tx
        .try_reserve_data(shared::audio_channel::MAX_AUDIO_COMMAND_QUEUED_BYTES - bytes)
        .unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, Vec::new(), cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    assert!(
        command_iir::forward(inbox.next().unwrap(), &mut contexts, &mut index, &mut inbox)
            .is_none()
    );
    assert_eq!(
        contexts[&7].graph_test_state(),
        original,
        "IIR index refusal entered node factory"
    );
    assert_eq!(index.get_context(10000), None);
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "index refusal freed IIR input on caller"
    );
    assert!(
        matches!(
            tx.try_reserve_data(1),
            Err(shared::audio_channel::AudioCommandReserveError::ByteLimit)
        ),
        "index refusal returned original queue byte ticket early"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((byte_filler, filler, index, host, inbox));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn real_factory_routes_keep_types_and_complete_pcm_and_collection() {
    let (scheduler, host, ledger) = setup();
    let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
    let mut contexts = HashMap::from([(7, AudioContext::new(7, 44100, 2))]);
    for command in commands(7, 10) {
        assert!(node_creation::apply(
            &command,
            &mut contexts,
            &mut index,
            48000
        ));
    }
    let ctx = contexts.get_mut(&7).unwrap();
    assert!(ctx.node_is::<crate::nodes::BufferSourceNode>(10));
    assert!(ctx.node_is::<crate::nodes::GainNode>(11));
    assert!(ctx.node_is::<OscillatorNode>(12));
    assert!(ctx.node_is::<DelayNode>(13));
    assert!(ctx.node_is::<crate::nodes::BiquadFilterNode>(14));
    assert!(ctx.node_is::<WaveShaperNode>(15));
    assert!(ctx.node_is::<crate::nodes::AnalyserNode>(16));
    assert!(ctx.node_is::<DynamicsCompressorNode>(17));
    assert!(ctx.node_is::<PannerNode>(18));
    assert!(ctx.node_is::<ChannelMergerNode>(19));
    assert!(ctx.node_is::<ChannelSplitterNode>(20));
    assert!(ctx.node_is::<ConstantSourceNode>(21));
    assert!(ctx.node_is::<crate::nodes::IIRFilterNode>(22));
    for id in 10..23 {
        assert_eq!(
            index.get_context(id),
            Some(7),
            "node factory published wrong index owner"
        );
    }
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            let mut ctx =
                AudioContext::new_managed(8, rate, channels, scheduler.audio_cpu(), ledger.clone())
                    .unwrap();
            ctx.set_test_dense_order();
            let mut actual = HashMap::from([(8, ctx)]);
            let mut current = NodeContextIndex::with_host(host.clone()).unwrap();
            for command in [
                AudioCmd::CreateOscillator {
                    ctx_id: 8,
                    node_id: 10,
                },
                AudioCmd::CreateGain {
                    ctx_id: 8,
                    node_id: 20,
                },
            ] {
                assert!(node_creation::apply(
                    &command,
                    &mut actual,
                    &mut current,
                    rate
                ));
            }
            let ctx = actual.get_mut(&8).unwrap();
            ctx.connect(10, 20);
            ctx.connect(20, crate::context::DESTINATION_NODE_ID);
            ctx.set_gain(20, 0.25);
            ctx.with_node_typed::<OscillatorNode, _>(10, |node| {
                node.start(0.0);
                node.stop(0.012);
            });
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut out = vec![0.375; frames * channels as usize + channels as usize - 1];
                let mut collected = ctx
                    .process_native_device(&mut out, rate, channels)
                    .unwrap()
                    .to_vec();
                collected.sort_unstable();
                let mut ended: Vec<_> = ctx.drain_ended().collect();
                ended.sort_unstable();
                let bits: Vec<_> = out.iter().map(|v| v.to_bits()).collect();
                println!(
                    "MIGO_INDEX_PCM {rate}:{channels}:{frames} {:?} {:?} {:?} {}",
                    bits,
                    collected,
                    ended,
                    ctx.current_time().to_bits()
                );
            }
            ctx.close();
            drop((actual, current));
        }
    }
    drop((contexts, index, host));
    wait(|| ledger.used_bytes() == 0);
}
