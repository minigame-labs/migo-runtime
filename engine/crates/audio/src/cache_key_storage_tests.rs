//! Real string/Arc ownership, shared index keys and unchanged cached playback.
use super::super::GlobalAudioCache;
use super::*;
use crate::cache::retirement_tests::{Seen, block, hook};
use crate::decoder::DecodedAudio;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use std::{
    sync::{
        Barrier,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
const LIMIT: usize = 64 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "cache key retirement did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
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
fn cache(
    cpu: migo_io::audio_cpu::AudioCpu,
    host: &Host,
    ledger: &Arc<AudioAggregateLedger>,
    max: usize,
) -> GlobalAudioCache {
    let cache = GlobalAudioCache::managed(cpu, host.clone());
    {
        let mut inner = cache.inner.lock().unwrap();
        inner.aggregate = ledger.clone();
        inner.max_size = max;
    }
    cache
}
fn text(tag: &str, capacity: usize) -> String {
    let mut text = String::with_capacity(capacity);
    text.push_str(tag);
    text
}
fn long(tag: &str) -> String {
    format!("{tag}{}", "x".repeat(8192))
}
fn pcm(value: f32) -> DecodedAudio {
    DecodedAudio {
        samples: vec![value],
        sample_rate: 48000,
        channels: 1,
    }
}
// Observe std Arc itself, independently of Key::new and its layout quotation.
fn arc_bytes() -> usize {
    let before = migo_alloc_probe::thread_counts();
    let cell = Arc::new(Data::Native(String::new()));
    let result = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    drop(cell);
    result as usize
}
fn fee(host: &Host, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    let before = ledger.used_bytes();
    let quote = host.reserve_value_with_bytes::<String>(bytes).unwrap();
    let result = ledger.used_bytes() - before;
    drop(quote);
    result
}
type CacheSnapshot = (usize, u64, Vec<(String, usize)>, Vec<(u64, String)>);
fn snapshot(cache: &GlobalAudioCache) -> CacheSnapshot {
    let inner = cache.inner.lock().unwrap();
    let mut entries: Vec<_> = inner
        .entries
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.audio.samples.as_ptr() as usize))
        .collect();
    entries.sort_unstable();
    (
        inner.current_size,
        inner.access_counter,
        entries,
        inner.lru_iter().map(|(n, k)| (*n, k.to_string())).collect(),
    )
}
#[test]
fn native_arc_layout_adoption_hash_lookup_and_clones_match_actual_allocation() {
    assert_eq!(
        arc_bytes(),
        Key::cell_bytes(),
        "cache key Arc layout differs from actual allocation"
    );
    let source = text("你好/é\0tail", 8192);
    let pointer = source.as_ptr();
    let capacity = source.capacity();
    let original = source.clone();
    let before = migo_alloc_probe::thread_counts();
    let key = Key::new(source, None).unwrap();
    let made = migo_alloc_probe::thread_counts();
    assert_eq!(
        made.bytes_allocated - before.bytes_allocated,
        arc_bytes() as u64,
        "cache key copied incoming string"
    );
    assert_eq!(key.as_str().as_ptr(), pointer);
    assert_eq!(key.capacity(), capacity);
    let before = migo_alloc_probe::thread_counts();
    let a = key.clone();
    let b = key.clone();
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "cache key clone allocated"
    );
    let mut cache = super::super::AudioCache::with_max_size_and_aggregate(
        32,
        Arc::new(AudioAggregateLedger::new(LIMIT)),
    );
    cache.insert(original.clone(), pcm(7.0)).unwrap();
    assert_eq!(
        cache.get(original.as_str()).map(|audio| audio.samples[0]),
        Some(7.0),
        "cache key borrowed lookup changed string hashing"
    );
    cache.insert(original.clone(), pcm(8.0)).unwrap();
    assert_eq!(
        cache.stats().entry_count,
        1,
        "cache key equality changed replacement semantics"
    );
    assert!(cache.get("你好/é").is_none());
    assert_eq!(b.as_str(), original);
    drop((key, a, b, cache));
}
#[test]
fn managed_cache_charges_one_shared_key_without_copying_original_backing() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger, 32);
    let url = text("primary", 8192);
    let lookup = url.clone();
    let pointer = url.as_ptr();
    let bytes = url.capacity() + arc_bytes();
    let charge = fee(&host, &ledger, bytes);
    let audio = cache.insert(url, pcm(0.75)).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES
            + charge
            + 4
            + crate::graph_table_layout::Layout::for_entries::<(Key, super::super::CacheEntry)>(
                cache.inner.lock().unwrap().entries.capacity(),
            )
            .unwrap()
            .bytes,
        "stored cache key omitted physical credit"
    );
    {
        let inner = cache.inner.lock().unwrap();
        let key = inner.entries.get_key_value(lookup.as_str()).unwrap().0;
        let lru = inner.lru_keys().next().unwrap();
        assert_eq!(
            key.as_str().as_ptr(),
            pointer,
            "stored cache key copied original backing"
        );
        assert_eq!(
            key.as_str().as_ptr(),
            lru.as_str().as_ptr(),
            "cache and LRU retained duplicate URL allocations"
        );
    }
    drop((audio, cache));
    wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn hit_reuses_large_lru_key_under_full_budget_and_closed_cpu() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger, 32);
    let url = long("hit");
    cache.insert(url.clone(), pcm(0.25)).unwrap();
    let pointer = {
        let inner = cache.inner.lock().unwrap();
        inner.lru_keys().next().unwrap().as_str().as_ptr() as usize
    };
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "cache hit peer")
        .unwrap();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    let audio = cache.get(&url).unwrap();
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(
        allocated < url.len() as u64,
        "cache hit cloned or re-admitted large LRU URL"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    assert_eq!(audio.samples, [0.25]);
    assert_eq!(
        cache
            .inner
            .lock()
            .unwrap()
            .lru_keys()
            .next()
            .unwrap()
            .as_str()
            .as_ptr() as usize,
        pointer,
        "cache hit replaced original LRU backing"
    );
    drop((filler, audio, cache));
    wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn refused_key_admission_bypasses_cache_before_eviction_or_counter_mutation() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger, 4);
    cache.insert("old".into(), pcm(0.5)).unwrap();
    let original = snapshot(&cache);
    let audio = pcm(-0.75);
    let pointer = audio.samples.as_ptr();
    let permit = ledger.try_reserve(4, "cache incoming PCM").unwrap();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "cache metadata peer")
        .unwrap();
    let returned = cache
        .insert_with_permit("new".into(), audio, permit)
        .unwrap();
    assert_eq!(returned.samples.as_ptr(), pointer);
    assert_eq!(returned.samples, [-0.75]);
    assert_eq!(
        snapshot(&cache),
        original,
        "cache metadata refusal evicted or published an entry"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop((filler, returned, cache));
    wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn key_refusal_precedes_arc_allocation_and_matches_direct_admission_cost() {
    let (_s, host, ledger) = setup();
    let source = long("refuse");
    let bytes = source.capacity() + arc_bytes();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "cache key first peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let control = host.reserve_value_with_bytes::<String>(bytes);
    let end = migo_alloc_probe::thread_counts();
    assert!(control.is_err());
    let expected = end.allocations - before.allocations;
    let before = migo_alloc_probe::thread_counts();
    let result = Key::new(source, Some(&host));
    let end = migo_alloc_probe::thread_counts();
    assert!(result.is_err(), "cache key admission was bypassed");
    assert_eq!(
        end.allocations - before.allocations,
        expected,
        "cache key Arc allocated before admission"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop((result, control, filler, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn inline_credit_returns_after_both_string_and_arc_cell_are_freed() {
    let (_s, host, ledger) = setup();
    let source = text("small", 128);
    let owned = source.capacity() + arc_bytes();
    assert!(owned < 4096);
    let mut key = Key::new(source, Some(&host)).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + owned,
        "cache key inline capacity omitted physical credit"
    );
    let base = Arc::new(AtomicU64::new(0));
    let complete = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicBool::new(false));
    let start = base.clone();
    let correct = complete.clone();
    let ran = seen.clone();
    let usage = ledger.clone();
    key.observe(Arc::new(move || {
        ran.store(true, Ordering::Release);
        correct.store(
            usage.used_bytes() == HANDOFF_BYTES
                && migo_alloc_probe::thread_counts().bytes_freed - start.load(Ordering::Acquire)
                    >= owned as u64,
            Ordering::Release,
        );
    }));
    base.store(
        migo_alloc_probe::thread_counts().bytes_freed,
        Ordering::Release,
    );
    drop(key);
    assert!(seen.load(Ordering::Acquire));
    assert!(
        complete.load(Ordering::Acquire),
        "cache key inline refund preceded Arc deallocation"
    );
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn bulk_key_clones_keep_credit_until_original_cpu_destruction_after_close() {
    let (s, host, ledger) = setup();
    let source = long("bulk");
    let bytes = source.capacity() + arc_bytes();
    let expected = HANDOFF_BYTES + fee(&host, &ledger, bytes);
    let mut key = Key::new(source, Some(&host)).unwrap();
    let seen = Arc::new(Seen::default());
    key.observe(hook(&seen));
    let a = key.clone();
    let b = key.clone();
    let gate = block(&s.audio_cpu());
    s.close();
    let before = migo_alloc_probe::thread_counts();
    drop((key, a, b));
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "cache key shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < 8192,
        "cache key freed large string on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "cache key returned physical credit before CPU drop"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!seen.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn concurrent_final_handles_retire_exactly_one_original_key() {
    let (s, host, ledger) = setup();
    let mut key = Key::new(long("parallel"), Some(&host)).unwrap();
    let seen = Arc::new(Seen::default());
    key.observe(hook(&seen));
    let other = key.clone();
    let used = ledger.used_bytes();
    let gate = block(&s.audio_cpu());
    let barrier = Arc::new(Barrier::new(3));
    let first = barrier.clone();
    let second = barrier.clone();
    let a = std::thread::spawn(move || {
        first.wait();
        drop(key);
    });
    let b = std::thread::spawn(move || {
        second.wait();
        drop(other);
    });
    barrier.wait();
    a.join().unwrap();
    b.join().unwrap();
    assert_eq!(ledger.used_bytes(), used);
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!seen.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn removal_replace_lru_clear_and_drop_release_one_shared_large_key() {
    for mode in ["remove", "replace", "lru", "clear", "drop"] {
        let (s, host, ledger) = setup();
        let mut cache = Some(cache(s.audio_cpu(), &host, &ledger, 4));
        let url = long("lifecycle");
        let seen = Arc::new(Seen::default());
        let audio = observe_new(hook(&seen), || {
            cache.as_ref().unwrap().insert(url.clone(), pcm(0.25))
        })
        .unwrap();
        let gate = block(&s.audio_cpu());
        let before = migo_alloc_probe::thread_counts();
        match mode {
            "remove" => cache.as_ref().unwrap().remove(&url),
            "replace" => {
                cache
                    .as_ref()
                    .unwrap()
                    .insert(url.clone(), pcm(-0.25))
                    .unwrap();
            }
            "lru" => {
                cache
                    .as_ref()
                    .unwrap()
                    .insert("next".into(), pcm(-0.5))
                    .unwrap();
            }
            "clear" => cache.as_ref().unwrap().clear(),
            "drop" => drop(cache.take()),
            _ => unreachable!(),
        }
        if mode == "clear" {
            assert!(
                cache.as_ref().unwrap().inner.lock().unwrap().lru_empty(),
                "cache clear retained LRU key owner"
            );
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        assert!(
            freed < 8192,
            "cache index removal freed large shared key on caller"
        );
        assert_eq!(
            seen.count.load(Ordering::Acquire),
            0,
            "cache indexes destroyed key before CPU drop"
        );
        gate.open();
        wait(|| seen.count.load(Ordering::Acquire) == 1);
        assert!(!seen.wrong.load(Ordering::Acquire));
        drop((audio, cache));
        wait(|| ledger.used_bytes() == HANDOFF_BYTES);
        drop(host);
        wait(|| ledger.used_bytes() == 0);
    }
}
#[test]
fn complete_cache_lru_replacement_and_cached_player_pcm_trace() {
    let (s, host, ledger) = setup();
    for channels in [1, 2, 32] {
        let bytes = 8192 * channels as usize * 4;
        let cache = cache(s.audio_cpu(), &host, &ledger, bytes * 2);
        let a = long("A");
        let b = long("B");
        let c = long("C");
        let make = |sentinel: bool| DecodedAudio {
            samples: (0..8192 * channels as usize)
                .map(|i| {
                    if sentinel {
                        0.8125
                    } else {
                        (i % 127) as f32 / 256.0 - 0.25
                    }
                })
                .collect(),
            sample_rate: 48000,
            channels,
        };
        cache.insert(a.clone(), make(false)).unwrap();
        cache.insert(b.clone(), make(true)).unwrap();
        let mut player = crate::inner_audio::InnerAudioPlayer::new(7, channels);
        let load = player.begin_load().unwrap();
        player
            .complete_cached_load(load, cache.get(&a).unwrap())
            .unwrap();
        player.play();
        let mut played = 0usize;
        for frames in [1, 127, 128, 129, 257, 4097] {
            let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
            player.process(&mut output);
            let expected: Vec<_> = (0..frames * channels as usize)
                .map(|i| {
                    (0.375 + (((played * channels as usize + i) % 127) as f32 / 256.0 - 0.25))
                        .to_bits()
                })
                .collect();
            assert_eq!(
                output[..frames * channels as usize]
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                expected,
                "cache LRU changed player's original complete PCM"
            );
            assert!(
                output[frames * channels as usize..]
                    .iter()
                    .all(|v| *v == 0.375)
            );
            played += frames;
            if frames == 127 {
                cache.insert(c.clone(), make(true)).unwrap();
                assert!(cache.get(&b).is_none());
            }
            if frames == 129 {
                cache.insert(a.clone(), make(true)).unwrap();
                assert_eq!(cache.get(&a).unwrap().samples[0], 0.8125);
            }
            if frames == 257 {
                cache.clear();
            }
            let inner = cache.inner.lock().unwrap();
            let order: Vec<_> = inner
                .lru_keys()
                .map(|key| {
                    if key.as_str() == a {
                        1
                    } else if key.as_str() == b {
                        2
                    } else {
                        3
                    }
                })
                .collect();
            let bits: Vec<_> = output.iter().map(|v| v.to_bits()).collect();
            println!(
                "MIGO_CACHE_KEY_PCM {channels}:{frames} {:?} {:?} {:?}",
                bits,
                [
                    player.shared.current_time().to_bits(),
                    player.is_active() as u64,
                    inner.entries.len() as u64,
                    inner.current_size as u64
                ],
                order
            );
        }
        drop((player, cache));
        wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
