//! Independent allocator/layout observations and production table ownership.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::graph_map::{GraphMap, Plan};
use crate::graph_table_layout::Layout;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use std::sync::atomic::Ordering;
const LIMIT: usize = 32 << 20;
const TABLES: [&str; 6] = [
    "nodes",
    "buffers",
    "node_buffers",
    "dense_index",
    "released",
    "ended_watch",
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
fn allocation_model_matches_real_standard_map_and_set_allocations_and_frees() {
    use std::collections::{HashMap, HashSet};
    fn observe<K, V>(entries: usize) {
        let hash = std::collections::hash_map::RandomState::new();
        let model = Layout::for_entries::<(K, V)>(entries).unwrap();
        let begin = migo_alloc_probe::thread_counts();
        let data = HashMap::<K, V>::with_capacity_and_hasher(entries, hash);
        let allocated = migo_alloc_probe::thread_counts();
        assert_eq!(
            data.capacity(),
            model.capacity,
            "standard table capacity model changed"
        );
        assert_eq!(
            allocated.bytes_allocated - begin.bytes_allocated,
            model.bytes as u64,
            "hash layout omitted real allocation bytes"
        );
        drop(data);
        let freed = migo_alloc_probe::thread_counts();
        assert_eq!(
            freed.bytes_freed - allocated.bytes_freed,
            model.bytes as u64,
            "hash layout differs from actual free bytes"
        );
    }
    #[repr(align(64))]
    struct Aligned {
        _bytes: [u8; 64],
    }
    for count in [
        0, 1, 3, 4, 7, 8, 14, 15, 28, 29, 63, 127, 128, 1023, 1024, 16385,
    ] {
        observe::<(), ()>(count);
        observe::<u8, u8>(count);
        observe::<u32, ()>(count);
        observe::<u32, u8>(count);
        observe::<u32, u32>(count);
        observe::<u32, Vec<u8>>(count);
        observe::<u32, Aligned>(count);
        let hash = std::collections::hash_map::RandomState::new();
        let begin = migo_alloc_probe::thread_counts();
        let data = HashSet::<u32>::with_capacity_and_hasher(count, hash);
        let allocated = migo_alloc_probe::thread_counts();
        let model = Layout::for_entries::<(u32, ())>(count).unwrap();
        assert_eq!(
            allocated.bytes_allocated - begin.bytes_allocated,
            model.bytes as u64,
            "set layout omitted actual allocation"
        );
        drop(data);
        assert_eq!(
            migo_alloc_probe::thread_counts().bytes_freed - allocated.bytes_freed,
            model.bytes as u64
        );
    }
    assert!(Layout::for_entries::<(u32, u32)>(usize::MAX).is_err());
}

#[test]
fn removals_and_empty_tables_keep_original_allocation_credit() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let allocation_begin = migo_alloc_probe::thread_counts();
    let mut map = GraphMap::new();
    for key in 0..1024 {
        map.insert(key, key + 1);
    }
    let allocated = migo_alloc_probe::thread_counts();
    let bytes = map.allocation_bytes();
    assert_eq!(
        (allocated.bytes_allocated - allocation_begin.bytes_allocated)
            - (allocated.bytes_freed - allocation_begin.bytes_freed),
        bytes as u64,
        "native table tracking lost actual allocation"
    );
    let original_capacity = map.capacity();
    for key in 0..1024 {
        map.remove(&key);
    }
    assert!(map.capacity() <= original_capacity);
    assert_eq!(
        map.allocation_bytes(),
        bytes,
        "removal returned table allocation credit"
    );
    let begin = ledger.used_bytes();
    map.prepare(&host).unwrap();
    assert!(
        ledger.used_bytes() >= begin + bytes + 4096,
        "empty table adoption used live entries instead of allocation"
    );
    let held = ledger.used_bytes();
    map.clear();
    assert_eq!(ledger.used_bytes(), held);
    drop(map);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn all_six_tables_adopt_without_copy_and_stay_charged_until_original_cpu_drop() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut control = graph(&cpu, &ledger);
    let default_bytes: usize = control.graph_table_identities().iter().map(|r| r.3).sum();
    let baseline = ledger.used_bytes();
    let mut ctx = AudioContext::new(2, 48000, 2);
    for name in TABLES {
        ctx.set_native_table_capacity(name, 1025);
    }
    let identity = ctx.graph_table_identities();
    let bytes: usize = identity.iter().map(|r| r.3).sum();
    let values = ctx.graph_test_state();
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    assert!(
        ledger.used_bytes() - baseline >= baseline - default_bytes + bytes + 6 * 4096,
        "context table adoption omitted allocated capacity"
    );
    assert_eq!(
        ctx.graph_table_identities(),
        identity,
        "table adoption moved original entries"
    );
    assert_eq!(ctx.graph_test_state(), values);
    let state = Arc::new(Dropped::default());
    for name in TABLES {
        ctx.observe_graph_table(name, witness(&state));
    }
    let held = ledger.used_bytes();
    let gate = block(&cpu);
    scheduler.close();
    ctx.close();
    drop(ctx);
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "table destroyed before original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "table credit returned before actual destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 6);
    assert!(!state.wrong.load(Ordering::Acquire));
    control.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn last_node_table_refusal_keeps_graph_nodes_and_recycled_buffer() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = AudioContext::new(1, 48000, 2);
    for id in 10..13 {
        assert!(ctx.create_gain(id));
    }
    ctx.release_node(12);
    for name in ["nodes", "node_buffers", "dense_index", "released"] {
        ctx.set_native_table_capacity(name, 3);
    }
    ctx.prepare_node_retirement(cpu, ledger.clone()).unwrap();
    ctx.collect_device_controls();
    let tables = ctx.graph_table_identities();
    let vectors = ctx.graph_vector_identities();
    let values = ctx.graph_test_state();
    let pool = ctx.render_pool_identity();
    let new_bytes = Layout::for_entries::<(u32, crate::node_retirement::Node)>(4)
        .unwrap()
        .bytes
        + Layout::for_entries::<(u32, crate::render_buffer::RenderBuffer)>(4)
            .unwrap()
            .bytes
        + Layout::for_entries::<(u32, u32)>(4).unwrap().bytes
        + Layout::for_entries::<(u32, ())>(4).unwrap().bytes;
    let node_fee = std::mem::size_of::<crate::nodes::GainNode>() + 4096;
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - node_fee - new_bytes + 1,
            "last table peer",
        )
        .unwrap();
    assert!(!ctx.create_gain(20), "node table batch bypassed admission");
    assert_eq!(
        ctx.graph_table_identities(),
        tables,
        "refused table batch changed a table"
    );
    assert_eq!(
        ctx.graph_vector_identities(),
        vectors,
        "refused tables changed vector allocation"
    );
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "refused tables changed graph flags"
    );
    assert_eq!(
        ctx.render_pool_identity(),
        pool,
        "refused table batch consumed pool buffer"
    );
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn table_growth_transfers_nonclone_values_and_retains_old_table_credit() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let state = Arc::new(Dropped::default());
    let mut map = GraphMap::with_capacity(1024);
    for key in 0..200 {
        map.insert(key, Element(key, state.clone()));
    }
    map.prepare(&host).unwrap();
    let old = Arc::new(Dropped::default());
    map.observe(witness(&old));
    let held = ledger.used_bytes();
    let gate = block(&cpu);
    Plan::reserve(&map, Some(&host), 4097)
        .unwrap()
        .unwrap()
        .install(&mut map);
    assert_eq!(map.len(), 200, "table growth lost entries");
    for key in 0..200 {
        assert_eq!(
            map.get(&key).unwrap().0,
            key,
            "table growth lost original values"
        );
    }
    assert!(
        ledger.used_bytes() >= held + map.allocation_bytes() + 4096,
        "table growth released old credit early"
    );
    assert_eq!(old.count.load(Ordering::Acquire), 0);
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "table growth dropped nonclone payload"
    );
    gate.open();
    wait(|| old.count.load(Ordering::Acquire) == 1);
    assert!(!old.wrong.load(Ordering::Acquire));
    drop(map);
    drop(host);
    wait(|| state.count.load(Ordering::Acquire) == 200 && ledger.used_bytes() == 0);
    assert!(!state.wrong.load(Ordering::Acquire));
}
struct Element(u32, Arc<Dropped>);
impl Drop for Element {
    fn drop(&mut self) {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            self.1.wrong.store(true, Ordering::Release);
        }
        self.1.count.fetch_add(1, Ordering::Release);
    }
}
#[test]
fn actual_table_payload_drops_on_original_cpu_without_late_allocation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let state = Arc::new(Dropped::default());
    let mut map = GraphMap::with_capacity(1024);
    map.insert(1, Element(1, state.clone()));
    map.prepare(&host).unwrap();
    let held = ledger.used_bytes();
    let gate = block(&cpu);
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    drop(map);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "table drop allocated a late handoff"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "actual table payload escaped original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "table credit returned before payload destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn inline_bulk_foreign_ledger_and_repeated_adoption_are_exact() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let other = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu.clone(), ledger.clone(), 2).unwrap();
    let foreign = crate::node_retirement::Host::new(cpu, other.clone(), 2).unwrap();
    for size in [1, 1024] {
        let mut map = GraphMap::<u32>::with_capacity(size);
        let bytes = map.allocation_bytes();
        let before = ledger.used_bytes();
        map.prepare(&host).unwrap();
        if bytes < 4096 {
            assert_eq!(
                ledger.used_bytes() - before,
                bytes,
                "inline table credit differs from allocation"
            );
        } else {
            assert!(
                ledger.used_bytes() >= before + bytes + 4096,
                "bulk table omitted CPU credit"
            );
        }
        let held = ledger.used_bytes();
        assert!(
            map.prepare(&foreign).is_err(),
            "table accepted foreign ledger"
        );
        map.prepare(&host).unwrap();
        assert_eq!(
            ledger.used_bytes(),
            held,
            "repeated table adoption charged twice"
        );
    }
    drop((host, foreign));
    wait(|| ledger.used_bytes() == 0 && other.used_bytes() == 0);
}

#[test]
fn closed_host_reuses_owned_slots_and_guard_blocks_unadmitted_insert() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let mut map = GraphMap::with_capacity(3);
    for key in 0..3 {
        map.insert(key, key);
    }
    map.prepare(&host).unwrap();
    let held = ledger.used_bytes();
    scheduler.close();
    assert!(
        Plan::reserve(&map, Some(&host), 3).is_ok_and(|p| p.is_none()),
        "owned table slot required fresh admission"
    );
    assert!(
        Plan::reserve(&map, Some(&host), 4).is_err(),
        "closed host admitted new table storage"
    );
    let before = migo_alloc_probe::thread_counts();
    assert_eq!(map.insert(1, 9), Some(1));
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "full table replacement allocated"
    );
    assert_eq!(ledger.used_bytes(), held);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| map.insert(4, 4)));
    assert!(failed.is_err(), "managed table bypassed capacity guard");
    assert_eq!(map.len(), 3);
    drop(map);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn watch_table_refusal_keeps_notifications_and_original_tables() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = AudioContext::new(1, 48000, 2);
    for id in 10..14 {
        assert!(ctx.create_gain(id));
    }
    for id in 10..13 {
        assert!(ctx.watch_source_ended(id, true));
    }
    ctx.set_native_table_capacity("ended_watch", 3);
    ctx.prepare_node_retirement(cpu, ledger.clone()).unwrap();
    ctx.collect_device_controls();
    let tables = ctx.graph_table_identities();
    let values = ctx.graph_test_state();
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - Layout::for_entries::<(u32, ())>(4).unwrap().bytes + 1,
            "watch table peer",
        )
        .unwrap();
    assert!(
        !ctx.watch_source_ended(13, true),
        "ended watch table bypassed admission"
    );
    assert_eq!(ctx.graph_table_identities(), tables);
    assert_eq!(
        ctx.graph_test_state(),
        values,
        "refused table watch changed controls"
    );
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn many_node_release_requests_remain_allocation_free_and_keep_table_credit() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    assert!(ctx.add_node(Box::new(crate::nodes::OscillatorNode::new(10, 48000))));
    ctx.with_node_typed::<crate::nodes::OscillatorNode, _>(10, |n| n.start(0.0));
    for id in 20..100 {
        assert!(ctx.create_gain(id));
        ctx.connect(10, id);
        ctx.connect(id, crate::context::DESTINATION_NODE_ID);
    }
    let before: Vec<_> = ctx
        .graph_table_identities()
        .iter()
        .map(|r| (r.0, r.3))
        .collect();
    let mut output = [0.0; 256];
    let start = migo_alloc_probe::thread_counts();
    ctx.process_native_device(&mut output, 48000, 2).unwrap();
    for id in 20..100 {
        ctx.release_node(id);
    }
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - start.allocations,
            end.reallocations - start.reallocations
        ),
        (0, 0),
        "managed graph table cold render/release allocated"
    );
    assert!(output.iter().any(|v| v.abs() > 1e-5));
    assert_eq!(
        before,
        ctx.graph_table_identities()
            .iter()
            .map(|r| (r.0, r.3))
            .collect::<Vec<_>>(),
        "release returned retained table allocation"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn growth_batch_reserves_before_any_table_allocation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let n = 16385;
    let sizes = [
        Layout::for_entries::<(u32, crate::node_retirement::Node)>(n)
            .unwrap()
            .bytes,
        Layout::for_entries::<(u32, crate::render_buffer::RenderBuffer)>(n)
            .unwrap()
            .bytes,
        Layout::for_entries::<(u32, u32)>(n).unwrap().bytes,
        Layout::for_entries::<(u32, ())>(n).unwrap().bytes,
    ];
    let available = sizes.iter().sum::<usize>() - sizes[3] / 2 + 3 * 5000;
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes() - available, "table batch peer")
        .unwrap();
    let held = ledger.used_bytes();
    let tables = ctx.graph_table_identities();
    let values = ctx.graph_test_state();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        ctx.grow_graph_tables_for_test(n).is_err(),
        "table batch bypassed last reservation"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(
        allocated < *sizes.iter().min().unwrap() as u64,
        "table allocated before complete batch admission"
    );
    assert_eq!(
        ctx.graph_table_identities(),
        tables,
        "refused table batch moved entries"
    );
    assert_eq!(ctx.graph_test_state(), values);
    assert_eq!(ledger.used_bytes(), held, "table batch leaked credit");
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
