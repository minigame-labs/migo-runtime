//! Context buckets own their capacity without delaying route revocation.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::node_retirement::{HANDOFF_BYTES, Host};
use context_table::ContextTable;
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
fn table_bytes(entries: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let native = HashMap::<u32, AudioContext>::with_capacity(entries);
    let bytes = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout =
        crate::graph_table_layout::Layout::for_entries::<(u32, AudioContext)>(native.capacity())
            .unwrap();
    assert_eq!(bytes, layout.bytes as u64);
    layout.bytes
}
fn fee(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    assert!(bytes >= 4096);
    let used = ledger.used_bytes();
    let quote = host
        .reserve_value_with_bytes::<Option<HashMap<u32, AudioContext>>>(bytes)
        .unwrap();
    let amount = ledger.used_bytes() - used;
    drop(quote);
    amount
}
fn fill(table: &mut ContextTable, count: usize) {
    for id in 1..=count as u32 {
        assert!(
            table
                .create_with(id, || Ok(AudioContext::new(id, 48000, 2)))
                .unwrap()
        );
    }
}
#[test]
fn initial_table_charges_native_layout_and_retires_original_buckets() {
    let (scheduler, host, ledger) = setup();
    let bytes = table_bytes(4);
    let expected = fee(&host, &ledger, bytes);
    let mut table = ContextTable::new(host.clone()).unwrap();
    assert_eq!(
        table.table_bytes(),
        bytes,
        "context table layout differs from native buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + expected,
        "context initial table omitted physical charge"
    );
    let state = Arc::new(Dropped::default());
    table.observe_table(witness(&state));
    let gate = block(&scheduler.audio_cpu());
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop(table);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "context table freed original buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "context table returned credit before CPU destruction"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn initial_refusal_precedes_bucket_allocation() {
    let (_scheduler, host, ledger) = setup();
    let bytes = table_bytes(4);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "context startup peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = ContextTable::new(host.clone());
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "context initial table bypassed admission");
    assert!(
        allocated < bytes as u64,
        "context buckets allocated before initial admission"
    );
    drop((filler, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn growth_refusal_precedes_context_factory_and_ledger_lookup() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    let capacity = table.identity().1;
    fill(&mut table, capacity);
    let original = table.identity();
    let epoch = table.get(&1).unwrap().device_epoch();
    let target = table_bytes(capacity + 1);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "context growth peer")
        .unwrap();
    let mut called = false;
    let before = migo_alloc_probe::thread_counts();
    let result = table.create_with(10000, || {
        called = true;
        Ok(AudioContext::new(10000, 44100, 32))
    });
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "context growth bypassed admission");
    assert!(!called, "context factory entered before table admission");
    assert_eq!(
        table.identity(),
        original,
        "context refusal moved live entries"
    );
    assert!(epoch.is_live(), "context refusal invalidated live route");
    assert!(
        allocated < target as u64,
        "context refusal allocated target buckets"
    );
    let mut ledger_called = false;
    assert!(
        !table.create_context(10000, 44100, 32, Some(&scheduler.audio_cpu()), || {
            ledger_called = true;
            ledger.clone()
        })
    );
    assert!(
        !ledger_called,
        "context ledger lookup preceded table admission"
    );
    drop((filler, table, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn duplicate_create_is_allocation_free_after_cpu_close() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    let capacity = table.identity().1;
    fill(&mut table, capacity);
    let original = table.identity();
    let epoch = table.get(&1).unwrap().device_epoch();
    scheduler.close();
    let mut called = false;
    let before = migo_alloc_probe::thread_counts();
    let result = table.create_with(1, || {
        called = true;
        Ok(AudioContext::new(1, 44100, 32))
    });
    let end = migo_alloc_probe::thread_counts();
    assert!(
        matches!(result, Ok(false)),
        "duplicate context attempted new table admission"
    );
    assert!(!called, "duplicate context invoked factory");
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "duplicate context allocated"
    );
    assert_eq!(
        table.identity(),
        original,
        "duplicate context replaced live entry"
    );
    assert!(epoch.is_live() && epoch.same(&table.get(&1).unwrap().device_epoch()));
    assert_eq!(table.get(&1).unwrap().sample_rate(), 48000);
    assert_eq!(table.get(&1).unwrap().channels(), 2);
    drop((table, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn actual_constructor_refusal_does_not_install_reserved_table_growth() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    let capacity = table.identity().1;
    fill(&mut table, capacity);
    let original = table.identity();
    let epoch = table.get(&1).unwrap().device_epoch();
    let target = table_bytes(capacity + 1);
    let target_fee = fee(&host, &ledger, target);
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - target_fee,
            "context last factory peer",
        )
        .unwrap();
    let held = ledger.used_bytes();
    let mut called = false;
    let before = migo_alloc_probe::thread_counts();
    let created = table.create_context(10000, 48000, 8192, Some(&scheduler.audio_cpu()), || {
        called = true;
        ledger.clone()
    });
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(!created, "context actor published rejected constructor");
    assert!(
        called,
        "context factory refusal fixture did not reach constructor"
    );
    assert_eq!(
        table.identity(),
        original,
        "failed context factory installed or published table growth"
    );
    assert!(
        epoch.is_live(),
        "failed context factory revoked existing route"
    );
    assert!(
        allocated < target as u64,
        "failed context factory allocated target buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "failed context factory leaked table reservation"
    );
    drop((filler, table, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn successful_growth_moves_graphs_and_keeps_overlapping_table_credit() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    let capacity = table.identity().1;
    fill(&mut table, capacity);
    let graphs: Vec<_> = (1..=capacity as u32)
        .map(|id| {
            let ctx = table.get(&id).unwrap();
            (id, ctx.graph_vector_identities(), ctx.device_epoch())
        })
        .collect();
    let bytes = table.table_bytes();
    let target_fee = fee(&host, &ledger, table_bytes(capacity + 1));
    let state = Arc::new(Dropped::default());
    table.observe_table(witness(&state));
    let gate = block(&scheduler.audio_cpu());
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        table
            .create_with(10000, || Ok(AudioContext::new(10000, 48000, 2)))
            .unwrap()
    );
    let end = migo_alloc_probe::thread_counts();
    assert!(
        table.contains_key(&10000),
        "context creation did not publish new entry"
    );
    assert!(
        graphs.iter().all(|(id, _, _)| table.contains_key(id)),
        "context rehash lost live entries"
    );
    for (id, vectors, epoch) in graphs {
        let ctx = table.get(&id).unwrap();
        assert_eq!(
            ctx.graph_vector_identities(),
            vectors,
            "context rehash copied graph allocations"
        );
        assert!(
            epoch.is_live() && epoch.same(&ctx.device_epoch()),
            "context rehash replaced live route identity"
        );
    }
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "context rehash freed old buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held + target_fee,
        "context rehash returned overlapping table credit"
    );
    gate.open();
    wait(|| {
        state.count.load(Ordering::Acquire) == 1
            && ledger.used_bytes() == HANDOFF_BYTES + target_fee
    });
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((table, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn scoped_close_revokes_only_claimed_route_and_preserves_table_capacity() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    for id in [1, 2] {
        assert!(
            table.create_context(id, 48000, 32, Some(&scheduler.audio_cpu()), || ledger
                .clone())
        );
    }
    let first = table.get(&1).unwrap().device_epoch();
    let second = table.get(&2).unwrap().device_epoch();
    let capacity = table.identity().1;
    let mut index = NodeContextIndex::new();
    index.register(11, 1);
    index.register(22, 2);
    let state = Arc::new(Dropped::default());
    table
        .get_mut(&1)
        .unwrap()
        .observe_graph_vector("mix_buffer", witness(&state));
    let gate = block(&scheduler.audio_cpu());
    scheduler.close();
    let held = ledger.used_bytes();
    assert!(
        release_context_scoped(&mut table, &mut index, 1),
        "scoped context release left table entry"
    );
    assert!(
        !first.is_live(),
        "scoped context close postponed route revocation"
    );
    assert!(
        second.is_live(),
        "scoped context close revoked another route"
    );
    assert!(table.get(&1).is_none() && table.get(&2).is_some());
    assert_eq!(index.get_context(11), None);
    assert_eq!(index.get_context(22), Some(2));
    assert_eq!(table.identity().1, capacity);
    assert_eq!(
        ledger.used_bytes(),
        held,
        "context close returned retained capacity credit early"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    assert!(!release_context_scoped(&mut table, &mut index, 1));
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((table, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn owned_restart_barrier_closes_routes_and_retains_empty_table_credit() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    fill(&mut table, 2);
    let epochs: Vec<_> = [1, 2]
        .map(|id| table.get(&id).unwrap().device_epoch())
        .into();
    let capacity = table.identity().1;
    let held = ledger.used_bytes();
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(AudioCmd::CreateContext {
        ctx_id: 99,
        sample_rate: None,
    })
    .unwrap();
    let backlog = vec![rx.try_recv_owned().unwrap()];
    tx.try_send(AudioCmd::CreateContext {
        ctx_id: 100,
        sample_rate: None,
    })
    .unwrap();
    let ticket = tx.request_release_all_contexts().unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, scheduler.audio_cpu());
    let mut index = NodeContextIndex::new();
    index.register(11, 1);
    index.register(22, 2);
    assert!(matches!(
        inbox.next().unwrap().command(),
        AudioCmd::ReleaseAllContexts
    ));
    handle_owned_release_all_contexts(&mut inbox, &mut table, &mut index);
    assert!(
        epochs.iter().all(|epoch| !epoch.is_live()),
        "context restart acknowledged before route revocation"
    );
    assert!(
        ticket.is_complete(),
        "context restart failed to acknowledge barrier"
    );
    assert!(
        table.view().is_empty() && inbox.next().is_none(),
        "context restart retained old commands or entries"
    );
    assert_eq!(index.get_context(11), None);
    assert_eq!(index.get_context(22), None);
    assert_eq!(table.identity().1, capacity);
    assert_eq!(
        ledger.used_bytes(),
        held,
        "context restart returned empty table capacity credit"
    );
    assert!(create_context_scoped(&mut table, 3, 44100, 2, None));
    assert!(table.get(&3).unwrap().device_epoch().is_live());
    drop((table, host, inbox));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn shutdown_revokes_routes_before_closed_cpu_destroys_original_empty_table() {
    let (scheduler, host, ledger) = setup();
    let mut table = ContextTable::new(host.clone()).unwrap();
    for id in [1, 2] {
        assert!(
            table.create_context(id, 48000, 32, Some(&scheduler.audio_cpu()), || ledger
                .clone())
        );
    }
    let epochs: Vec<_> = [1, 2]
        .map(|id| table.get(&id).unwrap().device_epoch())
        .into();
    let state = Arc::new(Dropped::default());
    table.observe_table(witness(&state));
    for ctx in table.values_mut() {
        ctx.observe_graph_vector("mix_buffer", witness(&state));
    }
    let gate = block(&scheduler.audio_cpu());
    scheduler.close();
    let held = ledger.used_bytes();
    let bytes = table.table_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop((table, host));
    let end = migo_alloc_probe::thread_counts();
    assert!(
        epochs.iter().all(|epoch| !epoch.is_live()),
        "context shutdown postponed route revocation"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "context shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "context shutdown freed table on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "context shutdown returned table or payload credit early"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "context storage escaped original CPU"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 3 && ledger.used_bytes() == 0);
    assert!(!state.wrong.load(Ordering::Acquire));
}
#[test]
fn complete_managed_context_table_factory_pcm_clock_and_route_trace() {
    let (scheduler, host, ledger) = setup();
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            let mut table = ContextTable::new(host.clone()).unwrap();
            assert!(
                table.create_context(7, rate, channels, Some(&scheduler.audio_cpu()), || ledger
                    .clone())
            );
            let mut index = NodeContextIndex::with_host(host.clone()).unwrap();
            for command in [
                AudioCmd::CreateOscillator {
                    ctx_id: 7,
                    node_id: 10,
                },
                AudioCmd::CreateGain {
                    ctx_id: 7,
                    node_id: 20,
                },
            ] {
                assert!(node_creation::apply(&command, &mut table, &mut index, rate));
            }
            let ctx = table.get_mut(&7).unwrap();
            ctx.connect(10, 20);
            ctx.connect(20, crate::context::DESTINATION_NODE_ID);
            ctx.set_gain(20, 0.25);
            ctx.with_node_typed::<OscillatorNode, _>(10, |node| {
                node.start(0.0);
                node.stop(0.012);
            });
            let epoch = ctx.device_epoch();
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
                let mut collected = ctx
                    .process_native_device(&mut output, rate, channels)
                    .unwrap()
                    .to_vec();
                collected.sort_unstable();
                let mut ended: Vec<_> = ctx.drain_ended().collect();
                ended.sort_unstable();
                let bits: Vec<_> = output.iter().map(|value| value.to_bits()).collect();
                println!(
                    "MIGO_CONTEXT_PCM {rate}:{channels}:{frames} {:?} {:?} {:?} {}",
                    bits,
                    collected,
                    ended,
                    ctx.current_time().to_bits()
                );
                assert!(epoch.is_live() && epoch.same(&ctx.device_epoch()));
            }
            assert!(release_context_scoped(&mut table, &mut index, 7));
            assert!(!epoch.is_live());
            drop((table, index));
        }
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
