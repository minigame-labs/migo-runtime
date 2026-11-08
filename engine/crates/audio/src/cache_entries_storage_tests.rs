//! Actual std allocation, refusal custody and the original CPU's real drop.
use super::*;
use crate::cache::retirement_tests::{Seen, block, hook};
use crate::cache::{CacheEntry, GlobalAudioCache, Key};
use crate::decoder::DecodedAudio;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
const LIMIT: usize = 64 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "cache bucket retirement timed out"
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
) -> GlobalAudioCache {
    let cache = GlobalAudioCache::managed(cpu, host.clone());
    cache.inner.lock().unwrap().aggregate = ledger.clone();
    cache
}
fn pcm(value: f32) -> DecodedAudio {
    DecodedAudio {
        samples: vec![value],
        sample_rate: 48000,
        channels: 1,
    }
}
fn fill(cache: &GlobalAudioCache, n: usize) {
    for i in 0..n {
        cache
            .insert(format!("item-{i}"), pcm(i as f32 / 256.0))
            .unwrap();
    }
}
fn fee(
    cpu: Option<&migo_io::audio_cpu::AudioCpu>,
    host: &Host,
    ledger: &Arc<AudioAggregateLedger>,
    bytes: usize,
) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    // Older retired buckets share this ledger. Keep their original CPU from
    // releasing credit during the quote; None means the caller holds its gate.
    let _gate = cpu.map(block);
    let before = ledger.used_bytes();
    let quote = host
        .reserve_value_with_bytes::<Option<RetiredBuckets>>(bytes)
        .unwrap();
    let result = ledger.used_bytes() - before;
    drop(quote);
    result
}
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    capacity: usize,
    bytes: usize,
    counter: u64,
    current: usize,
    entries: Vec<(String, usize, Vec<u32>)>,
    order: Vec<(u64, String)>,
}
fn snapshot(cache: &GlobalAudioCache) -> Snapshot {
    let inner = cache.inner.lock().unwrap();
    let mut entries: Vec<_> = inner
        .entries
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                v.audio.samples.as_ptr() as usize,
                v.audio.samples.iter().map(|v| v.to_bits()).collect(),
            )
        })
        .collect();
    entries.sort_unstable();
    Snapshot {
        capacity: inner.entries.capacity(),
        bytes: inner.bucket_bytes(),
        counter: inner.access_counter,
        current: inner.current_size,
        entries,
        order: inner.lru_iter().map(|(n, k)| (*n, k.to_string())).collect(),
    }
}
#[test]
fn bucket_layout_matches_independent_allocator_and_real_deallocation() {
    for requested in [0, 1, 3, 7, 16, 29, 100, 1000, 4097] {
        let before = migo_alloc_probe::thread_counts();
        let table = Entries::with_capacity(requested);
        let made = migo_alloc_probe::thread_counts();
        let bytes = table.allocation_bytes();
        let capacity = table.capacity();
        drop(table);
        let end = migo_alloc_probe::thread_counts();
        assert_eq!(
            made.bytes_allocated - before.bytes_allocated,
            bytes as u64,
            "cache bucket model missed physical allocation"
        );
        assert_eq!(
            end.bytes_freed - made.bytes_freed,
            bytes as u64,
            "cache bucket model missed physical deallocation"
        );
        assert_eq!(
            capacity,
            Layout::for_entries::<(Key, CacheEntry)>(requested)
                .unwrap()
                .capacity
        );
    }
}
#[test]
fn managed_constructor_is_lazy_before_any_bucket_admission() {
    let (s, host, ledger) = setup();
    let _ = AudioAggregateLedger::process_global();
    let before = migo_alloc_probe::thread_counts();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    let made = migo_alloc_probe::thread_counts();
    assert_eq!(
        cache.inner.lock().unwrap().entries.capacity(),
        0,
        "managed cache constructed unadmitted buckets"
    );
    assert_eq!(
        made.bytes_allocated - before.bytes_allocated,
        0,
        "managed cache constructor allocated buckets"
    );
    assert_eq!(ledger.used_bytes(), HANDOFF_BYTES);
    drop((cache, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn managed_cache_backing_is_charged_by_actual_bucket_allocation() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    let before = ledger.used_bytes();
    let key = Key::new("entry".into(), Some(&host)).unwrap();
    let key_bytes = ledger.used_bytes() - before;
    drop(key);
    cache.insert("entry".into(), pcm(0.25)).unwrap();
    let bytes = cache.inner.lock().unwrap().bucket_bytes();
    assert!(bytes > 0 && bytes < 4096);
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + key_bytes + 4 + bytes,
        "cache buckets omitted actual physical allocation"
    );
    cache.clear();
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + bytes,
        "empty cache returned retained bucket credit"
    );
    drop(cache);
    assert_eq!(ledger.used_bytes(), HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn initial_refusal_precedes_buckets_and_retains_original_input() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    let audio = pcm(-0.75);
    let pointer = audio.samples.as_ptr();
    let permit = ledger.try_reserve(4, "incoming cache PCM").unwrap();
    let url = String::from("refused");
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "cache bucket peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let returned = cache.insert_with_permit(url, audio, permit).unwrap();
    let made = migo_alloc_probe::thread_counts();
    assert_eq!(
        cache.inner.lock().unwrap().entries.capacity(),
        0,
        "cache bucket refusal allocated or published backing"
    );
    assert_eq!(returned.samples.as_ptr(), pointer);
    assert_eq!(returned.samples, [-0.75]);
    assert_eq!(ledger.used_bytes(), LIMIT);
    assert!(
        made.bytes_allocated - before.bytes_allocated
            < Layout::for_entries::<(Key, CacheEntry)>(16).unwrap().bytes as u64,
        "cache backing allocated before physical admission"
    );
    drop((filler, returned, cache, host));
    wait(|| ledger.used_bytes() == 0);
}
fn full(cache: &GlobalAudioCache) {
    cache.insert("seed".into(), pcm(0.5)).unwrap();
    let capacity = cache.inner.lock().unwrap().entries.capacity();
    fill(cache, capacity - 1);
    assert_eq!(cache.stats().unwrap().entry_count, capacity);
}
#[test]
fn growth_refusal_preserves_full_cache_before_lru_eviction_or_publication() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    full(&cache);
    let current_size = cache.stats().unwrap().current_size;
    cache.inner.lock().unwrap().max_size = current_size;
    let original = snapshot(&cache);
    let audio = pcm(-0.5);
    let pointer = audio.samples.as_ptr();
    let permit = ledger.try_reserve(4, "incoming cache PCM").unwrap();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes() - 512, "cache growth peer")
        .unwrap();
    let returned = cache
        .insert_with_permit("new".into(), audio, permit)
        .unwrap();
    assert_eq!(
        snapshot(&cache),
        original,
        "cache bucket refusal evicted or published an entry"
    );
    assert_eq!(returned.samples.as_ptr(), pointer);
    assert_eq!(returned.samples, [-0.5]);
    drop((filler, returned, cache, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn key_refusal_cancels_unused_bucket_ticket_without_changing_original_table() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    full(&cache);
    let original = snapshot(&cache);
    let growth = Layout::for_entries::<(Key, CacheEntry)>(original.capacity + 1)
        .unwrap()
        .bytes;
    let growth_charge = fee(Some(&s.audio_cpu()), &host, &ledger, growth);
    let mut url = String::with_capacity(8192);
    url.push_str("key-refusal");
    let audio = pcm(0.125);
    let pointer = audio.samples.as_ptr();
    let permit = ledger.try_reserve(4, "incoming cache PCM").unwrap();
    let filler = ledger
        .try_reserve(
            LIMIT - ledger.used_bytes() - growth_charge,
            "cache key peer",
        )
        .unwrap();
    let used = ledger.used_bytes();
    let control = cache
        .inner
        .lock()
        .unwrap()
        .entries
        .reserve(Some(&host))
        .unwrap();
    assert!(
        control.is_some(),
        "cache key refusal fixture did not admit bucket growth"
    );
    drop(control);
    let before = migo_alloc_probe::thread_counts();
    let returned = cache.insert_with_permit(url, audio, permit).unwrap();
    let made = migo_alloc_probe::thread_counts();
    assert_eq!(
        snapshot(&cache),
        original,
        "cache key refusal installed unused bucket plan"
    );
    assert_eq!(ledger.used_bytes(), used);
    assert_eq!(returned.samples.as_ptr(), pointer);
    assert!(
        made.bytes_allocated - before.bytes_allocated < growth as u64,
        "cache buckets allocated before all admissions completed"
    );
    drop((filler, returned, cache, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn empty_bulk_cache_drop_retains_buckets_until_original_cpu_destruction_after_close() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    fill(&cache, 128);
    cache.clear();
    let bytes = cache.inner.lock().unwrap().bucket_bytes();
    assert!(bytes >= 4096);
    let charge = fee(Some(&s.audio_cpu()), &host, &ledger, bytes);
    wait(|| ledger.used_bytes() <= HANDOFF_BYTES + charge);
    let seen = Arc::new(Seen::default());
    let freed = cache.inner.lock().unwrap().observe_buckets(hook(&seen));
    let gate = block(&s.audio_cpu());
    s.close();
    let before = migo_alloc_probe::thread_counts();
    drop(cache);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "cache buckets freed on caller before original CPU destruction"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "cache bucket drop allocated a late handoff"
    );
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + charge,
        "cache buckets returned physical credit before actual CPU destruction"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!seen.wrong.load(Ordering::Acquire));
    assert_eq!(
        freed.load(Ordering::Acquire),
        bytes,
        "cache original buckets not actually freed on CPU"
    );
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn growth_moves_original_values_and_retains_both_bucket_allocations_until_cpu_drop() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    fill(&cache, 128);
    let count = cache.stats().unwrap().entry_count;
    let capacity = cache.inner.lock().unwrap().entries.capacity();
    for i in count..capacity {
        cache
            .insert(format!("item-{i}"), pcm(i as f32 / 256.0))
            .unwrap();
    }
    let original = snapshot(&cache);
    let old_bytes = original.bytes;
    let old_fee = fee(Some(&s.audio_cpu()), &host, &ledger, old_bytes);
    let seen = Arc::new(Seen::default());
    let freed = cache.inner.lock().unwrap().observe_buckets(hook(&seen));
    let gate = block(&s.audio_cpu());
    let used = ledger.used_bytes();
    cache.insert("growth".into(), pcm(-0.25)).unwrap();
    let grown = snapshot(&cache);
    for (name, pointer, bits) in &original.entries {
        assert!(
            grown
                .entries
                .iter()
                .any(|(n, p, b)| n == name && p == pointer && b == bits),
            "cache growth copied or discarded original PCM"
        );
    }
    assert!(
        grown.capacity > original.capacity,
        "cache growth did not install admitted capacity"
    );
    let new_fee = fee(None, &host, &ledger, grown.bytes);
    assert!(
        ledger.used_bytes() >= used + new_fee,
        "cache growth omitted overlapping bucket credit"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1);
    assert!(!seen.wrong.load(Ordering::Acquire));
    assert_eq!(
        freed.load(Ordering::Acquire),
        old_bytes,
        "cache original growth buckets not actually freed on CPU"
    );
    cache.clear();
    wait(|| ledger.used_bytes() == HANDOFF_BYTES + new_fee);
    assert!(old_fee > old_bytes);
    drop((cache, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn table_ledger_identity_and_closed_host_refuse_without_allocation() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    cache.insert("kept".into(), pcm(0.75)).unwrap();
    let (_s2, foreign, other) = setup();
    assert!(
        cache
            .inner
            .lock()
            .unwrap()
            .check_bucket_host(&foreign)
            .is_err(),
        "cache table accepted foreign physical ledger"
    );
    assert_eq!(other.used_bytes(), HANDOFF_BYTES);
    s.close();
    let mut empty = Entries::new();
    assert!(
        empty.reserve(Some(&host)).is_err(),
        "cache buckets admitted after CPU close"
    );
    assert_eq!(empty.capacity(), 0);
    empty.clear();
    drop((empty, cache, host, foreign));
    wait(|| ledger.used_bytes() == 0 && other.used_bytes() == 0);
}
#[test]
fn tombstone_churn_clear_and_hits_preserve_physical_bucket_ownership() {
    let (s, host, ledger) = setup();
    let cache = cache(s.audio_cpu(), &host, &ledger);
    fill(&cache, 128);
    for i in 0..96 {
        cache.remove(&format!("item-{i}"));
    }
    let retained = cache.inner.lock().unwrap().bucket_bytes();
    let expected = fee(Some(&s.audio_cpu()), &host, &ledger, retained);
    for i in 96..128 {
        assert!(cache.get(&format!("item-{i}")).is_some());
    }
    cache.clear();
    assert_eq!(
        cache.inner.lock().unwrap().bucket_bytes(),
        retained,
        "tombstone or clear forgot physical bucket capacity"
    );
    wait(|| ledger.used_bytes() == HANDOFF_BYTES + expected);
    drop(cache);
    wait(|| ledger.used_bytes() == HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
