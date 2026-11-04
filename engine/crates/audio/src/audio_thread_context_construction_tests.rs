//! Managed construction admits its entire initial data set before allocation.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::AudioAggregateLedger;
use std::sync::atomic::{AtomicBool, Ordering};
const LIMIT: usize = 64 << 20;
const LARGE_CHANNELS: u32 = 8192;
fn managed(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> shared::error::EngineResult<AudioContext> {
    AudioContext::new_managed(7, 48000, channels, cpu.clone(), ledger.clone())
}
fn cost(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> usize {
    // Use the retained Native -> adopt route as an independent fee reference.
    // Measuring the candidate constructor here would lower the failure threshold
    // together with a missing ticket and let that fault escape the test.
    let mut ctx = AudioContext::new(7, 48000, channels);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let bytes = ledger.used_bytes();
    ctx.close();
    drop(ctx);
    wait(|| ledger.used_bytes() == 0);
    bytes
}
fn data_bytes(ctx: &AudioContext) -> usize {
    ctx.graph_vector_identities()
        .iter()
        .map(|r| r.4)
        .sum::<usize>()
        + ctx
            .graph_table_identities()
            .iter()
            .map(|r| r.3)
            .sum::<usize>()
        + ctx
            .render_buffer_identity(crate::context::DESTINATION_NODE_ID)
            .unwrap()
            .2
            * 4
        + std::mem::size_of::<crate::nodes::DestinationNode>()
}
#[test]
fn saturated_constructor_refuses_before_any_large_data_allocation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    // Warm process-global scheduler metadata outside the scoped observation.
    cost(&cpu, &ledger, 2);
    let filler = ledger.try_reserve(LIMIT, "constructor peer").unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = managed(&cpu, &ledger, LARGE_CHANNELS);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "saturated constructor bypassed admission");
    assert!(
        allocated < 128 * LARGE_CHANNELS as u64 * 4,
        "constructor allocated data before admission"
    );
    drop(filler);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn final_close_ticket_refusal_precedes_all_vector_table_and_render_data() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let required = cost(&cpu, &ledger, LARGE_CHANNELS);
    let filler = ledger
        .try_reserve(LIMIT - required + 1, "last constructor ticket peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = managed(&cpu, &ledger, LARGE_CHANNELS);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "constructor omitted final admission");
    assert!(
        allocated < 128 * LARGE_CHANNELS as u64 * 4,
        "constructor data allocated before complete admission"
    );
    drop(filler);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn initial_state_and_capacities_match_native_adoption_with_no_transient_data_copy() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    cost(&cpu, &ledger, 2);
    for channels in [0, 1, 2, 32, LARGE_CHANNELS] {
        let before = migo_alloc_probe::thread_counts();
        let mut current = managed(&cpu, &ledger, channels).unwrap();
        let after = migo_alloc_probe::thread_counts();
        let owned = data_bytes(&current);
        let charged = ledger.used_bytes();
        let allocated = after.bytes_allocated - before.bytes_allocated;
        assert!(
            ledger.used_bytes() >= owned,
            "constructor omitted owned allocation credit"
        );
        assert!(
            ledger.used_bytes() >= allocated as usize,
            "constructor charge did not cover caller allocation peak"
        );
        assert!(
            allocated < owned as u64 + 128 * 1024,
            "constructor made transient data copies"
        );
        let mut native = AudioContext::new(7, 48000, channels);
        native
            .prepare_node_retirement(cpu.clone(), ledger.clone())
            .unwrap();
        assert_eq!(
            ledger.used_bytes() - charged,
            charged,
            "managed constructor changed initial owned charge"
        );
        let vectors = |c: &AudioContext| {
            c.graph_vector_identities()
                .into_iter()
                .map(|(n, l, c, _, b)| (n, l, c, b))
                .collect::<Vec<_>>()
        };
        let tables = |c: &AudioContext| {
            c.graph_table_identities()
                .into_iter()
                .map(|(n, l, c, b, _)| (n, l, c, b))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            vectors(&current),
            vectors(&native),
            "managed constructor changed initial vector layout"
        );
        assert_eq!(
            tables(&current),
            tables(&native),
            "managed constructor changed initial table layout"
        );
        assert_eq!(
            current.graph_test_state(),
            native.graph_test_state(),
            "managed constructor changed initial graph values"
        );
        assert_eq!(
            current
                .render_buffer_identity(crate::context::DESTINATION_NODE_ID)
                .unwrap()
                .2,
            native
                .render_buffer_identity(crate::context::DESTINATION_NODE_ID)
                .unwrap()
                .2
        );
        current.close();
        native.close();
        drop((current, native));
        wait(|| ledger.used_bytes() == 0);
    }
}
#[test]
fn newly_constructed_bulk_storage_survives_close_and_cpu_shutdown_until_real_drop() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = managed(&cpu, &ledger, LARGE_CHANNELS).unwrap();
    let dropped = Arc::new(Dropped::default());
    ctx.observe_graph_vector("mix_buffer", witness(&dropped));
    ctx.observe_graph_vector("native_input", witness(&dropped));
    ctx.observe_render_buffer(crate::context::DESTINATION_NODE_ID, witness(&dropped));
    let held = ledger.used_bytes();
    let gate = block(&cpu);
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    ctx.close();
    drop(ctx);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "constructed storage allocated a late close handoff"
    );
    assert_eq!(
        dropped.count.load(Ordering::Acquire),
        0,
        "constructed bulk escaped original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "constructor credit returned before actual CPU destruction"
    );
    gate.open();
    wait(|| dropped.count.load(Ordering::Acquire) == 3 && ledger.used_bytes() == 0);
    assert!(!dropped.wrong.load(Ordering::Acquire));
}
#[test]
fn actor_refusal_keeps_full_context_map_and_existing_addresses() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    cost(&cpu, &ledger, 2);
    let mut contexts = HashMap::with_capacity(3);
    for id in 1..4 {
        contexts.insert(id, AudioContext::new(id, 48000, 2));
    }
    let identity = |c: &HashMap<u32, AudioContext>| {
        let mut rows: Vec<_> = c
            .iter()
            .map(|(id, ctx)| (*id, ctx as *const AudioContext as usize))
            .collect();
        rows.sort_unstable();
        rows
    };
    let original = identity(&contexts);
    let capacity = contexts.capacity();
    let filler = ledger.try_reserve(LIMIT, "actor constructor peer").unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        !create_context_scoped_with_ledger(
            &mut contexts,
            4,
            48000,
            LARGE_CHANNELS,
            Some(&cpu),
            || ledger.clone()
        ),
        "actor published rejected constructor"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert_eq!(
        identity(&contexts),
        original,
        "rejected context moved an existing map entry"
    );
    assert_eq!(
        contexts.capacity(),
        capacity,
        "rejected context grew actor map"
    );
    assert!(
        allocated < 4096,
        "actor allocated context map or data before admission"
    );
    drop(filler);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn duplicate_context_rejects_before_factory_or_ledger_lookup() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let ctx = managed(&cpu, &ledger, 2).unwrap();
    let mut contexts = HashMap::from([(7, ctx)]);
    let called = AtomicBool::new(false);
    let before = migo_alloc_probe::thread_counts();
    let used = ledger.used_bytes();
    assert!(
        !create_context_scoped_with_ledger(&mut contexts, 7, 44100, 32, Some(&cpu), || {
            called.store(true, Ordering::Release);
            ledger.clone()
        }),
        "duplicate context replaced live entry"
    );
    let end = migo_alloc_probe::thread_counts();
    assert!(
        !called.load(Ordering::Acquire),
        "duplicate context entered constructor admission"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "duplicate context allocated"
    );
    assert_eq!(ledger.used_bytes(), used);
    contexts.get_mut(&7).unwrap().close();
    drop(contexts);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_cpu_refuses_before_data_and_native_route_remains_compatible() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    cost(&cpu, &ledger, 2);
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        managed(&cpu, &ledger, LARGE_CHANNELS).is_err(),
        "closed CPU admitted a new context"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated
            < 128 * LARGE_CHANNELS as u64 * 4,
        "closed constructor allocated data before checking CPU"
    );
    let mut contexts = HashMap::new();
    assert!(create_context_scoped_with_ledger(
        &mut contexts,
        9,
        44100,
        2,
        None,
        || panic!("native route must not request managed ledger")
    ));
    let ctx = contexts.get_mut(&9).unwrap();
    let mut output = vec![0.125; 4097 * 2 + 1];
    ctx.process(&mut output);
    assert_eq!(output, vec![0.125; 4097 * 2 + 1]);
}
struct NodeProbe(Arc<Dropped>);
impl Drop for NodeProbe {
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
impl crate::nodes::AudioNodeProcessor for NodeProbe {
    fn id(&self) -> u32 {
        88
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn process(
        &mut self,
        _input: &[f32],
        _output: &mut [f32],
        _rate: u32,
        _channels: u32,
        _time: f64,
    ) -> usize {
        0
    }
}
#[test]
fn preallocated_destination_ticket_retires_real_node_payload_on_original_cpu() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let before_ticket = ledger.used_bytes();
    let ticket = host.reserve_node::<NodeProbe>().unwrap();
    assert_eq!(
        ledger.used_bytes() - before_ticket,
        std::mem::size_of::<NodeProbe>() + 4096,
        "destination ticket omitted node payload bytes"
    );
    let state = Arc::new(Dropped::default());
    let node = ticket.finish(NodeProbe(state.clone()));
    let used = ledger.used_bytes();
    let gate = block(&cpu);
    scheduler.close();
    drop(node);
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "pre-admitted node escaped original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        used,
        "pre-admitted node credit returned early"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn default_constructor_failure_allocates_no_table_bucket_data() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let required = cost(&cpu, &ledger, 2);
    let native = AudioContext::new(8, 48000, 2);
    let table_bytes: usize = native.graph_table_identities().iter().map(|r| r.3).sum();
    drop(native);
    let filler = ledger
        .try_reserve(LIMIT - required + 1, "default last constructor ticket peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        managed(&cpu, &ledger, 2).is_err(),
        "default constructor omitted final admission"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(
        allocated < table_bytes as u64,
        "constructor allocated table buckets before complete admission"
    );
    drop(filler);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn initial_tickets_price_real_capacity_before_allocating_and_finish_without_readmission() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let baseline = ledger.used_bytes();
    let n = 65537usize;
    let bytes = n * 4;
    let before = migo_alloc_probe::thread_counts();
    let vector = crate::graph_vec::Initial::<f32>::reserve(&host, n).unwrap();
    assert!(
        ledger.used_bytes() >= baseline + bytes + 4096,
        "initial vector ticket omitted bytes"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < bytes as u64,
        "initial vector allocated data while reserving"
    );
    let held = ledger.used_bytes();
    let vector = vector.finish();
    assert_eq!(vector.capacity(), n);
    assert_eq!(vector.len(), 0);
    assert_eq!(
        ledger.used_bytes(),
        held,
        "initial vector finish requested duplicate admission"
    );
    drop(vector);
    wait(|| ledger.used_bytes() == baseline);
    let expected = crate::graph_table_layout::Layout::for_entries::<(u32, u32)>(n).unwrap();
    let before = migo_alloc_probe::thread_counts();
    let table = crate::graph_map::Initial::<u32>::reserve(&host, n).unwrap();
    assert!(
        ledger.used_bytes() >= baseline + expected.bytes + 4096,
        "initial table ticket omitted bytes"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated
            < expected.bytes as u64,
        "initial table allocated buckets while reserving"
    );
    let held = ledger.used_bytes();
    let table = table.finish();
    assert_eq!(table.capacity(), expected.capacity);
    assert_eq!(table.len(), 0);
    assert_eq!(
        ledger.used_bytes(),
        held,
        "initial table finish requested duplicate admission"
    );
    drop(table);
    wait(|| ledger.used_bytes() == baseline);
    let before = migo_alloc_probe::thread_counts();
    let render = crate::render_buffer::Initial::reserve(&host, n).unwrap();
    assert!(
        ledger.used_bytes() >= baseline + bytes + 4096,
        "initial render ticket omitted bytes"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < bytes as u64,
        "initial render allocated data while reserving"
    );
    let held = ledger.used_bytes();
    let render = render.finish();
    assert_eq!(render.capacity(), n);
    assert_eq!(render.len(), n);
    assert!(render.as_slice().iter().all(|x| x.to_bits() == 0));
    assert_eq!(
        ledger.used_bytes(),
        held,
        "initial render finish requested duplicate admission"
    );
    drop(render);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
