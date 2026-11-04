//! Physical media membership ownership and original ordered controls.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::graph_vec::GraphVec;
use crate::node_retirement::{HANDOFF_BYTES, Host};
use media_membership::MediaPlayers;
use std::sync::atomic::Ordering;
const LIMIT: usize = 64 << 20;
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    Host,
    Arc<AudioAggregateLedger>,
) {
    let owner = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = Host::new(owner.audio_cpu(), ledger.clone(), 2).unwrap();
    (owner, host, ledger)
}
fn table_bytes(entries: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let native = HashMap::<u32, GraphVec<InnerAudioId>>::with_capacity(entries);
    let actual = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout = crate::graph_table_layout::Layout::for_entries::<(u32, GraphVec<InnerAudioId>)>(
        native.capacity(),
    )
    .unwrap();
    assert_eq!(actual, layout.bytes as u64);
    layout.bytes
}
fn fee<T: Send + 'static>(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    let baseline = ledger.used_bytes();
    let quote = host.reserve_value_with_bytes::<T>(bytes).unwrap();
    let amount = ledger.used_bytes() - baseline;
    drop(quote);
    amount
}
fn fill(media: &mut MediaPlayers, count: usize) {
    for id in 1..=count as u32 {
        media.create(id).unwrap();
    }
}
fn sources(media: &mut MediaPlayers, id: u32, count: u32) {
    for value in 0..count {
        media.add_source(id, value).unwrap();
    }
}
#[test]
fn initial_table_charges_real_owned_layout_and_drops_after_data() {
    let (_owner, host, ledger) = setup();
    let expected = table_bytes(4);
    let baseline = ledger.used_bytes();
    let media = MediaPlayers::new(host.clone()).unwrap();
    assert!(
        ledger.used_bytes() - baseline >= expected,
        "media initial table omitted physical charge"
    );
    assert_eq!(
        media.table_bytes(),
        expected,
        "media table layout disagrees with real standard allocation"
    );
    assert_eq!(media.identity().1, 7);
    drop(media);
    assert_eq!(ledger.used_bytes(), baseline);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn initial_refusal_allocates_no_table_buckets() {
    let (_owner, host, ledger) = setup();
    let expected = table_bytes(4);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "media startup peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = MediaPlayers::new(host.clone());
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "media initial table bypassed admission");
    assert!(
        allocated < expected as u64,
        "media table buckets allocated before admission"
    );
    drop((filler, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn table_refusal_preserves_all_membership_and_vector_addresses() {
    let (_owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    let capacity = media.identity().1;
    fill(&mut media, capacity);
    for id in 1..=capacity as u32 {
        sources(&mut media, id, 3);
    }
    let original = media.identity();
    let target = table_bytes(capacity + 1);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "media table growth peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = media.create(10000);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "media table growth bypassed admission");
    assert_eq!(
        media.identity(),
        original,
        "media table refusal changed live membership"
    );
    assert!(
        allocated < target as u64,
        "media table refusal allocated target buckets"
    );
    drop((filler, media, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn vector_refusal_precedes_copy_and_keeps_duplicate_and_unknown_noops() {
    let (_owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    media.create(1).unwrap();
    sources(&mut media, 1, 1024);
    let original = media.identity();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "media vector peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = media.add_source(1, 1024);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "media vector growth bypassed admission");
    assert_eq!(
        media.identity(),
        original,
        "media vector refusal changed ordered membership"
    );
    assert!(allocated < 4096, "media vector copied before admission");
    let before = migo_alloc_probe::thread_counts();
    media.add_source(1, 0).unwrap();
    media.add_source(9000, 0).unwrap();
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "media duplicate or missing add allocated"
    );
    assert_eq!(
        media.identity(),
        original,
        "media duplicate add changed membership"
    );
    drop((filler, media, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn removal_keeps_capacity_credit_and_reuses_slots_after_cpu_close() {
    let (owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    media.create(1).unwrap();
    sources(&mut media, 1, 8);
    let held = ledger.used_bytes();
    let capacity = media.identity().2[0].3;
    owner.close();
    let before = migo_alloc_probe::thread_counts();
    media.remove_source(1, 3);
    media.remove_source(99, 1);
    media.add_source(1, 100).unwrap();
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "media remove/reuse reached allocator"
    );
    assert_eq!(
        media.get(&1),
        Some([0, 1, 2, 4, 5, 6, 7, 100].as_slice()),
        "media removal changed survivor order"
    );
    assert_eq!(media.identity().2[0].3, capacity);
    assert_eq!(
        ledger.used_bytes(),
        held,
        "media removal returned live capacity charge"
    );
    assert!(
        media.add_source(1, 101).is_err(),
        "closed CPU admitted new media vector capacity"
    );
    assert_eq!(ledger.used_bytes(), held);
    drop((media, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn duplicate_create_and_destroy_retire_real_large_vectors_without_late_allocation() {
    let (owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    let cap = media.identity().1;
    fill(&mut media, cap);
    let quote = fee::<Option<Vec<InnerAudioId>>>(&host, &ledger, 4096);
    sources(&mut media, 1, 1024);
    sources(&mut media, 2, 1024);
    let baseline = HANDOFF_BYTES + table_bytes(4);
    wait(|| ledger.used_bytes() == baseline + 2 * quote);
    let state = Arc::new(Dropped::default());
    media.observe_sources(1, witness(&state));
    media.observe_sources(2, witness(&state));
    let gate = block(&owner.audio_cpu());
    owner.close();
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    media.create(1).unwrap();
    media.destroy(2);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "media reset or destroy allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < 4096,
        "media reset freed source data on caller"
    );
    assert_eq!(
        media.get(&1),
        Some([].as_slice()),
        "repeated media create did not reset members"
    );
    assert_eq!(media.get(&2), None, "media destroy left membership");
    assert_eq!(
        ledger.used_bytes(),
        held,
        "media source credit returned before real CPU drop"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "media sources escaped original CPU"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 2 && ledger.used_bytes() == baseline);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((media, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn bulk_table_growth_moves_nested_vectors_and_keeps_both_allocations_until_cpu_drop() {
    let (owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    let old_bytes = table_bytes(112);
    assert!(old_bytes >= 4096);
    let table_fee = fee::<Option<HashMap<u32, GraphVec<InnerAudioId>>>>(&host, &ledger, old_bytes);
    let vector_fee = fee::<Option<Vec<InnerAudioId>>>(&host, &ledger, 4096);
    fill(&mut media, 112);
    sources(&mut media, 1, 1024);
    sources(&mut media, 2, 1024);
    let expected = HANDOFF_BYTES + table_fee + 2 * vector_fee;
    wait(|| ledger.used_bytes() == expected);
    let original = media.identity().2;
    let state = Arc::new(Dropped::default());
    media.observe_table(witness(&state));
    media.observe_sources(1, witness(&state));
    media.observe_sources(2, witness(&state));
    let gate = block(&owner.audio_cpu());
    let before = migo_alloc_probe::thread_counts();
    media.create(10000).unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert!(
        after.bytes_freed - before.bytes_freed < old_bytes as u64,
        "media growth freed old table on caller"
    );
    assert_eq!(
        media.get(&10000),
        Some([].as_slice()),
        "media creation lost new player"
    );
    let current = media.identity();
    for row in original {
        assert_eq!(
            current.2.iter().find(|item| item.0 == row.0),
            Some(&row),
            "media rehash copied or lost nested vector"
        );
    }
    assert!(
        ledger.used_bytes() >= expected + table_bytes(113),
        "media rehash released overlapping table charge"
    );
    media.observe_table(witness(&state));
    owner.close();
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop((media, host));
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "media table drop allocated late handoff"
    );
    assert!(
        after.bytes_freed - before.bytes_freed < old_bytes as u64,
        "media drop freed nested storage on caller"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "media table escaped original CPU"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "media table credit returned before CPU destruction"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 4 && ledger.used_bytes() == 0);
    assert!(!state.wrong.load(Ordering::Acquire));
}
#[test]
fn successful_table_growth_preserves_existing_order_and_vector_identity() {
    let (_owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    let capacity = media.identity().1;
    fill(&mut media, capacity);
    for id in 1..=capacity as u32 {
        for value in [3, 1, 7] {
            media.add_source(id, value).unwrap();
        }
    }
    let original = media.identity().2;
    media.create(10000).unwrap();
    assert_eq!(
        media.get(&10000),
        Some([].as_slice()),
        "media creation lost new player"
    );
    let current = media.identity();
    for row in original {
        assert_eq!(
            current.2.iter().find(|item| item.0 == row.0),
            Some(&row),
            "media successful rehash lost existing player or moved source allocation"
        );
    }
    drop((media, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn playback_lookup_and_controls_follow_only_current_membership() {
    let (owner, host, ledger) = setup();
    let mut media = MediaPlayers::new(host.clone()).unwrap();
    media.create(10).unwrap();
    let mut players = HashMap::new();
    for id in [1, 2] {
        let player = InnerAudioPlayer::new(id, 2);
        player.shared.set_loaded(true);
        players.insert(id, player);
    }
    assert!(
        media.start(10, &mut players),
        "empty existing media player lost start intent"
    );
    assert!(
        !media.start(99, &mut players),
        "missing media player gained start intent"
    );
    media.add_source(10, 2).unwrap();
    media.add_source(10, 1).unwrap();
    media.add_source(10, 2).unwrap();
    media.add_source(10, 999).unwrap();
    assert_eq!(
        media.get(&10),
        Some([2, 1, 999].as_slice()),
        "media add changed order or duplicated source"
    );
    assert!(media.start(10, &mut players));
    assert!(
        players[&1].is_active() && players[&2].is_active(),
        "media start skipped current source"
    );
    media.remove_source(10, 2);
    media.stop(10, &mut players);
    assert!(
        !players[&1].is_active() && players[&2].is_active(),
        "media stop changed an unrelated source"
    );
    let mut intents = PlaybackIntents::new();
    intents.push(PlaybackIntent::Media(10));
    assert!(!intents.has_active_target(&HashMap::new(), &players, &media));
    assert!(media.start(10, &mut players));
    assert!(intents.has_active_target(&HashMap::new(), &players, &media));
    media.create(10).unwrap();
    assert!(!intents.has_active_target(&HashMap::new(), &players, &media));
    media.stop(10, &mut players);
    assert!(
        players[&1].is_active() && players[&2].is_active(),
        "media reset implicitly stopped detached source"
    );
    media.destroy(10);
    assert!(!media.start(10, &mut players));
    assert!(!intents.has_active_target(&HashMap::new(), &players, &media));
    drop((media, host, players, owner));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn complete_media_controls_pcm_and_ordered_trace() {
    let (owner, host, ledger) = setup();
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            let mut media = MediaPlayers::new(host.clone()).unwrap();
            media.create(10).unwrap();
            let mut players = HashMap::new();
            for id in [1, 2] {
                let mut player = InnerAudioPlayer::new(id, channels);
                player
                    .load_audio(DecodedAudio {
                        samples: (0..6000 * channels as usize)
                            .map(|i| id as f32 * 0.125 + (i % 13) as f32 * 0.005)
                            .collect(),
                        sample_rate: rate,
                        channels,
                    })
                    .unwrap();
                players.insert(id, player);
            }
            for source in [2, 1, 2, 999] {
                media.add_source(10, source).unwrap();
            }
            assert!(media.start(10, &mut players));
            let mut audible = false;
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
                // Fix the fixture's mixer iteration order; production mixing is unchanged.
                for id in [1, 2] {
                    players.get_mut(&id).unwrap().process(&mut output);
                }
                audible |= output.iter().any(|value| (*value - 0.375).abs() > 1e-6);
                let bits: Vec<_> = output.iter().map(|value| value.to_bits()).collect();
                let states: Vec<_> = [1, 2]
                    .iter()
                    .map(|id| players[id].shared.state() as u8)
                    .collect();
                let positions: Vec<_> = [1, 2]
                    .iter()
                    .map(|id| players[id].shared.position_frames())
                    .collect();
                println!(
                    "MIGO_MEDIA_PCM {rate}:{channels}:{frames} {:?} {:?} {:?}",
                    bits, states, positions
                );
                match frames {
                    1 => media.remove_source(10, 2),
                    128 => media.stop(10, &mut players),
                    129 => {
                        assert!(media.start(10, &mut players));
                    }
                    257 => {
                        media.create(10).unwrap();
                        media.stop(10, &mut players);
                    }
                    _ => {}
                }
                println!(
                    "MIGO_MEDIA_MEMBERS {rate}:{channels}:{frames} {:?}",
                    media.get(&10).unwrap()
                );
            }
            assert!(audible, "media fixture produced no nonzero PCM");
            drop((media, players));
        }
    }
    drop((owner, host));
    wait(|| ledger.used_bytes() == 0);
}
