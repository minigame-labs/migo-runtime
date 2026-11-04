//! Table ownership must not postpone player cancellation or change playback.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::node_retirement::{HANDOFF_BYTES, Host};
use inner_collection::InnerPlayers;
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
    let native = HashMap::<u32, InnerAudioPlayer>::with_capacity(entries);
    let bytes = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout = crate::graph_table_layout::Layout::for_entries::<(u32, InnerAudioPlayer)>(
        native.capacity(),
    )
    .unwrap();
    assert_eq!(bytes, layout.bytes as u64);
    layout.bytes
}
fn fee(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    assert!(bytes >= 4096);
    let baseline = ledger.used_bytes();
    let quote = host
        .reserve_value_with_bytes::<Option<HashMap<u32, InnerAudioPlayer>>>(bytes)
        .unwrap();
    let amount = ledger.used_bytes() - baseline;
    drop(quote);
    amount
}
fn fill(players: &mut InnerPlayers, count: usize) {
    for id in 1..=count as u32 {
        assert!(
            players
                .create_with(id, || Ok(InnerAudioPlayer::new(id, 2)))
                .unwrap()
        );
    }
}
#[test]
fn initial_table_charges_native_layout_and_retires_real_buckets() {
    let (owner, host, ledger) = setup();
    let bytes = table_bytes(16);
    let quote = fee(&host, &ledger, bytes);
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    assert_eq!(
        players.table_bytes(),
        bytes,
        "inner table layout differs from native buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + quote,
        "inner initial table omitted physical charge"
    );
    let state = Arc::new(Dropped::default());
    players.observe_table(witness(&state));
    let gate = block(&owner.audio_cpu());
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop(players);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "inner table freed buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "inner table returned credit before CPU destruction"
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
    let (_owner, host, ledger) = setup();
    let bytes = table_bytes(16);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "inner startup peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = InnerPlayers::new(host.clone());
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "inner initial table bypassed admission");
    assert!(
        allocated < bytes as u64,
        "inner buckets allocated before initial admission"
    );
    drop((filler, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn growth_refusal_precedes_factory_and_keeps_live_players() {
    let (_owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    let capacity = players.identity().1;
    fill(&mut players, capacity);
    let load = players.get_mut(&1).unwrap().begin_load_for(71).unwrap();
    let original = players.identity();
    let target = table_bytes(capacity + 1);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "inner growth peer")
        .unwrap();
    let mut called = false;
    let before = migo_alloc_probe::thread_counts();
    let result = players.create_with(10000, || {
        called = true;
        Ok(InnerAudioPlayer::new(10000, 2))
    });
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "inner growth bypassed admission");
    assert!(!called, "inner factory ran before table admission");
    assert_eq!(
        players.identity(),
        original,
        "inner refusal changed live player addresses"
    );
    assert!(load.is_current(), "inner refusal cancelled live source");
    assert!(
        allocated < target as u64,
        "inner refusal allocated target buckets"
    );
    drop((filler, players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn duplicate_create_is_allocation_free_after_cpu_close() {
    let (owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    let capacity = players.identity().1;
    fill(&mut players, capacity);
    let load = players.get_mut(&1).unwrap().begin_load_for(71).unwrap();
    let original = players.identity();
    let held = ledger.used_bytes();
    owner.close();
    let mut called = false;
    let before = migo_alloc_probe::thread_counts();
    let result = players.create_with(1, || {
        called = true;
        Ok(InnerAudioPlayer::new(1, 2))
    });
    let end = migo_alloc_probe::thread_counts();
    assert!(
        matches!(result, Ok(false)),
        "duplicate inner create attempted new admission"
    );
    assert!(!called, "duplicate inner create invoked factory");
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "duplicate inner create allocated"
    );
    assert_eq!(
        players.identity(),
        original,
        "duplicate inner create replaced live player"
    );
    assert!(
        load.is_current(),
        "duplicate inner create cancelled live source"
    );
    assert_eq!(ledger.used_bytes(), held);
    assert!(
        players
            .create_with(10000, || Ok(InnerAudioPlayer::new(10000, 2)))
            .is_err()
    );
    drop((players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn failed_factory_does_not_install_reserved_growth() {
    let (_owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    let capacity = players.identity().1;
    fill(&mut players, capacity);
    let target = table_bytes(capacity + 1);
    let original = players.identity();
    let held = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    let result = players.create_with(10000, || Err(EngineError::new(ErrorCode::InvalidArgument)));
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err());
    assert_eq!(
        players.identity(),
        original,
        "failed inner factory installed or published growth"
    );
    assert!(
        allocated < target as u64,
        "failed inner factory allocated target buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "failed inner factory leaked reservation"
    );
    drop((players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn successful_growth_preserves_shared_identity_and_overlapping_table_credit() {
    let (owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    let capacity = players.identity().1;
    fill(&mut players, capacity);
    let loads: Vec<_> = (1..=capacity as u32)
        .map(|id| players.get_mut(&id).unwrap().begin_load_for(id).unwrap())
        .collect();
    let original = players.identity().2;
    let bytes = table_bytes(16);
    let target_fee = fee(&host, &ledger, table_bytes(capacity + 1));
    let held = ledger.used_bytes();
    let state = Arc::new(Dropped::default());
    players.observe_table(witness(&state));
    let gate = block(&owner.audio_cpu());
    let before = migo_alloc_probe::thread_counts();
    assert!(
        players
            .create_with(10000, || Ok(InnerAudioPlayer::new(10000, 2)))
            .unwrap()
    );
    let end = migo_alloc_probe::thread_counts();
    assert!(
        players.contains_key(&10000),
        "inner create did not publish new player"
    );
    for (id, _, shared) in original {
        assert_eq!(
            players.get(&id).map(|p| Arc::as_ptr(&p.shared) as usize),
            Some(shared),
            "inner rehash lost or cloned live player"
        );
    }
    assert!(
        loads.iter().all(|load| load.is_current()),
        "inner rehash cancelled live source"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "inner rehash freed original buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held + target_fee,
        "inner rehash returned overlapping table credit"
    );
    gate.open();
    wait(|| {
        state.count.load(Ordering::Acquire) == 1
            && ledger.used_bytes() == HANDOFF_BYTES + target_fee
    });
    assert!(!state.wrong.load(Ordering::Acquire));
    drop((players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn remove_cancels_immediately_and_numeric_id_reuse_rejects_old_completion() {
    let (_owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    players
        .create_with(1, || Ok(InnerAudioPlayer::new(1, 2)))
        .unwrap();
    let old = players.get_mut(&1).unwrap().begin_load_for(71).unwrap();
    let held = ledger.used_bytes();
    assert!(
        players.remove(&1).is_some(),
        "inner remove left player in table"
    );
    assert!(
        !old.is_current(),
        "inner remove postponed source cancellation"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "inner remove returned live table credit"
    );
    players
        .create_with(1, || Ok(InnerAudioPlayer::new(1, 2)))
        .unwrap();
    let current = players.get_mut(&1).unwrap().begin_load_for(72).unwrap();
    let (send, mut receive) = tokio::sync::oneshot::channel();
    integrate_inner_audio_decode_result(
        &mut players,
        1,
        old,
        Ok(DecodedAudio {
            samples: vec![1.0; 8],
            sample_rate: 48000,
            channels: 2,
        }),
        send,
    );
    assert_eq!(
        receive.try_recv().unwrap().unwrap_err().code,
        ErrorCode::Cancelled,
        "reused inner id accepted old source completion"
    );
    assert!(players.get(&1).unwrap().shared.owns_load(&current));
    drop((players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn shutdown_cancels_live_players_before_closed_cpu_releases_empty_table() {
    let (owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    fill(&mut players, 2);
    let loads: Vec<_> = (1..=2)
        .map(|id| players.get_mut(&id).unwrap().begin_load_for(id).unwrap())
        .collect();
    let state = Arc::new(Dropped::default());
    players.observe_table(witness(&state));
    let gate = block(&owner.audio_cpu());
    owner.close();
    let held = ledger.used_bytes();
    let bytes = players.table_bytes();
    let before = migo_alloc_probe::thread_counts();
    drop((players, host));
    let end = migo_alloc_probe::thread_counts();
    assert!(
        loads.iter().all(|load| !load.is_current()),
        "inner shutdown postponed source cancellation"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "inner shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "inner shutdown freed table on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "inner shutdown returned table credit before CPU drop"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "inner table escaped original CPU"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == 0);
    assert!(!state.wrong.load(Ordering::Acquire));
}
#[test]
fn managed_players_share_original_service_and_media_controls() {
    let (_owner, host, ledger) = setup();
    let mut players = InnerPlayers::new(host.clone()).unwrap();
    fill(&mut players, 2);
    for player in players.values_mut() {
        player.shared.set_loaded(true);
    }
    let mut media = media_membership::MediaPlayers::new(host.clone()).unwrap();
    media.create(10).unwrap();
    media.add_source(10, 2).unwrap();
    media.add_source(10, 999).unwrap();
    assert!(media.start(10, &mut players));
    assert!(players.get(&2).unwrap().is_active() && !players.get(&1).unwrap().is_active());
    let mut intents = PlaybackIntents::new();
    intents.push(PlaybackIntent::Media(10));
    assert!(intents.has_active_target(&HashMap::new(), &players, &media));
    let cache = GlobalAudioCache::new();
    service_players(&mut players, &cache, |_event| {});
    media.stop(10, &mut players);
    assert!(!intents.has_active_target(&HashMap::new(), &players, &media));
    drop((media, players, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn complete_inner_player_table_pcm_and_lifecycle_trace() {
    let (_owner, host, ledger) = setup();
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            let mut players = InnerPlayers::new(host.clone()).unwrap();
            for id in [1, 2] {
                players
                    .create_with(id, || {
                        let mut player = InnerAudioPlayer::new(id, channels);
                        player.load_audio(DecodedAudio {
                            samples: (0..6000 * channels as usize)
                                .map(|i| id as f32 * 0.125 + (i % 13) as f32 * 0.005)
                                .collect(),
                            sample_rate: rate,
                            channels,
                        })?;
                        player.play();
                        Ok(player)
                    })
                    .unwrap();
            }
            let mut audible = false;
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
                // Canonical fixture iteration; production HashMap iteration stays unordered.
                for id in [1, 2] {
                    players.get_mut(&id).unwrap().process(&mut output);
                }
                audible |= output.iter().any(|value| (*value - 0.375).abs() > 1e-6);
                let bits: Vec<_> = output.iter().map(|value| value.to_bits()).collect();
                let states: Vec<_> = [1, 2]
                    .iter()
                    .map(|id| players.get(id).unwrap().shared.state() as u8)
                    .collect();
                let positions: Vec<_> = [1, 2]
                    .iter()
                    .map(|id| players.get(id).unwrap().shared.position_frames())
                    .collect();
                println!(
                    "MIGO_INNER_PCM {rate}:{channels}:{frames} {:?} {:?} {:?}",
                    bits, states, positions
                );
                match frames {
                    1 => players.get_mut(&2).unwrap().pause(),
                    128 => players.get_mut(&1).unwrap().stop(),
                    129 => {
                        for player in players.values_mut() {
                            player.play();
                        }
                    }
                    257 => {
                        assert!(
                            !players
                                .create_with(1, || panic!("duplicate factory"))
                                .unwrap()
                        );
                    }
                    _ => {}
                }
            }
            assert!(audible);
            drop(players);
        }
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
