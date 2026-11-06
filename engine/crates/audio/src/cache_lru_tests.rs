//! Independent LRU model, complete PCM, physical slots and original CPU frees.
use crate::cache::retirement_tests::{Seen, block, hook};
use crate::cache::{CacheEntry, GlobalAudioCache, Key, entries_storage::RetiredBuckets};
use crate::decoder::DecodedAudio;
use crate::graph_table_layout::Layout;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::{HANDOFF_BYTES, Host};
use std::{
    collections::VecDeque,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
const LIMIT: usize = 64 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "cache LRU retirement timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    Host,
    Arc<AudioAggregateLedger>,
) {
    let s = migo_io::scheduler::IoScheduler::new(1);
    let l = Arc::new(AudioAggregateLedger::new(LIMIT));
    let h = Host::new(s.audio_cpu(), l.clone(), 2).unwrap();
    (s, h, l)
}
fn cache(
    s: &migo_io::scheduler::IoScheduler,
    h: &Host,
    l: &Arc<AudioAggregateLedger>,
    max: usize,
) -> GlobalAudioCache {
    let c = GlobalAudioCache::managed(s.audio_cpu(), h.clone());
    {
        let mut inner = c.inner.lock().unwrap();
        inner.aggregate = l.clone();
        inner.max_size = max;
    }
    c
}
fn pcm(v: f32) -> DecodedAudio {
    DecodedAudio {
        samples: vec![v],
        sample_rate: 48000,
        channels: 1,
    }
}
fn order(c: &GlobalAudioCache) -> Vec<String> {
    let i = c.inner.lock().unwrap();
    assert!(i.lru_valid(), "cache LRU topology invalid");
    i.lru_keys().map(|k| k.to_string()).collect()
}
fn expected(c: &GlobalAudioCache, want: &[&str]) {
    assert_eq!(order(c), want, "cache LRU order changed");
}
fn fee(s: &migo_io::scheduler::IoScheduler, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    // Other cache owners may retire between these reads. Measure the actual
    // reservation on its own ledger, using the same original CPU and type.
    let l = Arc::new(AudioAggregateLedger::new(LIMIT));
    let h = Host::new(s.audio_cpu(), l.clone(), 2).unwrap();
    let b = l.used_bytes();
    let p = h
        .reserve_value_with_bytes::<Option<RetiredBuckets>>(bytes)
        .unwrap();
    let n = l.used_bytes() - b;
    drop(p);
    drop(h);
    wait(|| l.used_bytes() == 0);
    n
}
#[derive(Debug, PartialEq, Eq)]
struct State {
    counter: u64,
    size: usize,
    capacity: usize,
    bytes: usize,
    entries: Vec<(String, usize, Vec<u32>)>,
    order: Vec<(u64, String)>,
}
fn state(c: &GlobalAudioCache) -> State {
    let i = c.inner.lock().unwrap();
    assert!(i.lru_valid(), "cache LRU topology invalid");
    let mut entries: Vec<_> = i
        .entries
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                v.audio.samples.as_ptr() as usize,
                v.audio.samples.iter().map(|x| x.to_bits()).collect(),
            )
        })
        .collect();
    entries.sort_unstable();
    State {
        counter: i.access_counter,
        size: i.current_size,
        capacity: i.entries.capacity(),
        bytes: i.bucket_bytes(),
        entries,
        order: i.lru_iter().map(|(n, k)| (*n, k.to_string())).collect(),
    }
}
#[test]
fn embedded_links_are_in_the_actual_admitted_bucket_allocation() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 32);
    let b = l.used_bytes();
    let k = Key::new("entry".into(), Some(&h)).unwrap();
    let key_fee = l.used_bytes() - b;
    drop(k);
    c.insert("entry".into(), pcm(0.25)).unwrap();
    let i = c.inner.lock().unwrap();
    let bytes = Layout::for_entries::<(Key, CacheEntry)>(i.entries.capacity())
        .unwrap()
        .bytes;
    assert_eq!(
        i.bucket_bytes(),
        bytes,
        "cache LRU links omitted from physical bucket layout"
    );
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES + key_fee + 4 + bytes,
        "cache LRU links omitted physical credit"
    );
    assert!(i.lru_valid());
    drop(i);
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn hits_use_no_heap_under_full_budget_and_closed_original_cpu() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 64 * 4);
    let names: Vec<_> = (0..64).map(|n| format!("hot-{n}")).collect();
    for (n, name) in names.iter().enumerate() {
        c.insert(name.clone(), pcm(n as f32 / 128.0)).unwrap();
    }
    c.clear();
    let bytes = c.inner.lock().unwrap().bucket_bytes();
    let bucket_fee = fee(&s, bytes);
    wait(|| l.used_bytes() == HANDOFF_BYTES + bucket_fee);
    let held: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(n, name)| c.insert(name.clone(), pcm(n as f32 / 128.0)).unwrap())
        .collect();
    let pointers: Vec<_> = held.iter().map(|a| a.samples.as_ptr()).collect();
    let fill = l
        .try_reserve(LIMIT - l.used_bytes(), "LRU hit peer")
        .unwrap();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    for n in 0..1024 {
        let index = n % names.len();
        let a = c.get(&names[index]);
        assert_eq!(
            a.as_ref().map(|v| v.samples.as_ptr()),
            Some(pointers[index]),
            "cache LRU hit changed cached PCM"
        );
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations,
            after.deallocations - before.deallocations
        ),
        (0, 0, 0),
        "cache LRU hit allocated or freed metadata"
    );
    assert_eq!(l.used_bytes(), LIMIT);
    assert_eq!(order(&c), names, "cache LRU order changed");
    drop((fill, held, c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn head_middle_tail_singleton_remove_and_clear_keep_bidirectional_order() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 128);
    for (n, v) in [("a", 0.25), ("b", 0.5), ("c", 0.75), ("d", 1.0)] {
        c.insert(n.into(), pcm(v)).unwrap();
    }
    expected(&c, &["a", "b", "c", "d"]);
    assert_eq!(
        c.get("b").map(|a| a.samples[0]),
        Some(0.5),
        "cache LRU hit changed cached PCM"
    );
    expected(&c, &["a", "c", "d", "b"]);
    c.get("a").unwrap();
    expected(&c, &["c", "d", "b", "a"]);
    c.get("a").unwrap();
    expected(&c, &["c", "d", "b", "a"]);
    c.remove("d");
    expected(&c, &["c", "b", "a"]);
    c.remove("c");
    expected(&c, &["b", "a"]);
    c.remove("a");
    expected(&c, &["b"]);
    c.get("b").unwrap();
    expected(&c, &["b"]);
    c.remove("b");
    expected(&c, &[]);
    c.insert("again".into(), pcm(-0.0)).unwrap();
    expected(&c, &["again"]);
    c.clear();
    expected(&c, &[]);
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn bucket_refusal_keeps_complete_old_lru_and_original_incoming_pcm() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 1 << 20);
    for n in 0..28 {
        c.insert(
            format!("old-{n}"),
            DecodedAudio {
                samples: vec![n as f32 / 32.0; 512],
                sample_rate: 48000,
                channels: 1,
            },
        )
        .unwrap();
    }
    {
        let mut i = c.inner.lock().unwrap();
        assert_eq!(i.entries.len(), i.entries.capacity());
        i.max_size = i.current_size;
    }
    let original = state(&c);
    let mut samples = Vec::with_capacity(8192);
    samples.extend((0..4097).map(|n| {
        if n == 0 {
            -0.0
        } else {
            (n % 127) as f32 / 256.0 - 0.25
        }
    }));
    let pointer = samples.as_ptr();
    let expected_pcm: Vec<_> = samples.iter().map(|v| (0.375 + v).to_bits()).collect();
    let bytes = samples.capacity() * 4;
    let permit = l.try_reserve(bytes, "incoming LRU PCM").unwrap();
    let peer = l
        .try_reserve(LIMIT - l.used_bytes() - 512, "LRU table peer")
        .unwrap();
    let returned = c
        .insert_with_permit(
            "denied".into(),
            DecodedAudio {
                samples,
                sample_rate: 48000,
                channels: 1,
            },
            permit,
        )
        .unwrap();
    assert_eq!(
        state(&c),
        original,
        "cache LRU refusal changed original cache"
    );
    assert_eq!(returned.samples.as_ptr(), pointer);
    let mut player = crate::inner_audio::InnerAudioPlayer::new(7, 1);
    let load = player.begin_load().unwrap();
    player.complete_cached_load(load, returned.clone()).unwrap();
    player.play();
    let mut out = vec![0.375; 4097];
    player.process(&mut out);
    assert_eq!(
        out.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        expected_pcm,
        "refused cache LRU changed complete incoming PCM"
    );
    drop((player, returned, peer, c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn final_lru_metadata_waits_for_original_cpu_and_real_bucket_free_after_close() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 1 << 20);
    let held: Vec<_> = (0..1024)
        .map(|n| {
            c.insert(format!("held-{n}"), pcm(n as f32 / 1024.0))
                .unwrap()
        })
        .collect();
    let keys: Vec<_> = {
        let i = c.inner.lock().unwrap();
        assert!(i.lru_valid());
        i.lru_keys().cloned().collect()
    };
    let bytes = c.inner.lock().unwrap().bucket_bytes();
    let charge = fee(&s, bytes);
    let seen = Arc::new(Seen::default());
    let freed = c.inner.lock().unwrap().observe_buckets(hook(&seen));
    let gate = block(&s.audio_cpu());
    let used = l.used_bytes();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    drop(c);
    let after = migo_alloc_probe::thread_counts();
    assert!(
        after.bytes_freed - before.bytes_freed < 4096,
        "cache LRU metadata freed on caller before original CPU"
    );
    assert_eq!(
        after.allocations - before.allocations,
        0,
        "cache LRU drop allocated late handoff"
    );
    assert_eq!(
        l.used_bytes(),
        used,
        "cache LRU credit returned before original CPU destruction"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| seen.count.load(Ordering::Acquire) == 1);
    assert_eq!(
        freed.load(Ordering::Acquire),
        bytes,
        "cache LRU backing was not actually freed on original CPU"
    );
    assert!(!seen.wrong.load(Ordering::Acquire));
    wait(|| l.used_bytes() <= used - charge);
    drop((held, keys));
    wait(|| l.used_bytes() == HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn table_growth_preserves_all_recency_links_and_original_pcm_pointers() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 1 << 20);
    for n in 0..28 {
        c.insert(format!("row-{n}"), pcm(n as f32 / 32.0)).unwrap();
    }
    c.get("row-9").unwrap();
    c.get("row-0").unwrap();
    let old = state(&c);
    let old_order = order(&c);
    c.insert("grown".into(), pcm(-0.25)).unwrap();
    let new = state(&c);
    assert!(
        new.capacity > old.capacity,
        "cache LRU growth did not grow buckets"
    );
    assert_eq!(
        &order(&c)[..old_order.len()],
        old_order,
        "cache LRU growth changed recency order"
    );
    for e in old.entries {
        assert!(
            new.entries.contains(&e),
            "cache LRU growth moved or lost original PCM"
        );
    }
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn counter_wrap_preserves_true_lru_and_does_not_interrupt_hits() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 8);
    c.insert("a".into(), pcm(0.25)).unwrap();
    c.insert("b".into(), pcm(0.5)).unwrap();
    c.inner.lock().unwrap().access_counter = u64::MAX;
    let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.get("a")));
    assert!(hit.is_ok(), "cache LRU counter overflow interrupted hit");
    assert_eq!(hit.unwrap().map(|a| a.samples[0]), Some(0.25));
    c.insert("c".into(), pcm(0.75)).unwrap();
    {
        let i = c.inner.lock().unwrap();
        assert!(
            i.entries.contains_key("a")
                && !i.entries.contains_key("b")
                && i.entries.contains_key("c"),
            "cache LRU counter wrap evicted most recent entry"
        );
    }
    expected(&c, &["a", "c"]);
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn replacement_preserves_original_evict_before_replace_byte_policy_and_player_arc() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 8);
    let old = c.insert("a".into(), pcm(0.25)).unwrap();
    let ptr = old.samples.as_ptr();
    c.insert("b".into(), pcm(0.5)).unwrap();
    c.get("a").unwrap();
    c.insert("a".into(), pcm(-0.25)).unwrap();
    expected(&c, &["a"]);
    assert_eq!(old.samples.as_ptr(), ptr);
    assert_eq!(old.samples, [0.25]);
    assert_eq!(c.get("a").unwrap().samples, [-0.25]);
    assert_eq!(c.stats().unwrap().current_size, 4);
    drop((old, c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn links_share_original_large_url_allocation_across_hits_replace_and_clear() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 32);
    let mut url = String::with_capacity(8192);
    url.push_str("你好/é\0original");
    let lookup = url.clone();
    let pointer = url.as_ptr();
    c.insert(url, pcm(0.125)).unwrap();
    for _ in 0..16 {
        c.get(&lookup).unwrap();
        let i = c.inner.lock().unwrap();
        assert!(i.lru_valid());
        assert_eq!(
            i.entries
                .get_key_value(lookup.as_str())
                .unwrap()
                .0
                .as_str()
                .as_ptr(),
            pointer
        );
        assert_eq!(
            i.lru_keys().next().unwrap().as_str().as_ptr(),
            pointer,
            "cache LRU copied original URL backing"
        );
    }
    c.insert(lookup.clone(), pcm(0.5)).unwrap();
    assert_eq!(c.stats().unwrap().entry_count, 1);
    c.clear();
    assert!(c.inner.lock().unwrap().lru_empty());
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn missing_lookup_and_remove_leave_all_links_and_storage_unchanged_without_heap() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 32);
    c.insert("kept".into(), pcm(0.5)).unwrap();
    let old = state(&c);
    let before = migo_alloc_probe::thread_counts();
    assert!(c.get("missing").is_none());
    c.remove("missing");
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations,
            after.deallocations - before.deallocations
        ),
        (0, 0, 0),
        "missing cache LRU allocated or freed metadata"
    );
    assert_eq!(state(&c), old, "missing cache LRU changed existing state");
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn deterministic_operations_match_independent_weighted_vecdeque_lru_model() {
    let (s, h, l) = setup();
    let c = cache(&s, &h, &l, 28);
    let mut model: VecDeque<(String, Vec<u32>, usize)> = VecDeque::new();
    let mut seed = 0x49a3_4fd1u64;
    for step in 0..5000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let name = format!("key-{}", seed % 19);
        match (seed >> 8) % 11 {
            0 => {
                c.clear();
                model.clear();
            }
            1 | 2 => {
                c.remove(&name);
                model.retain(|v| v.0 != name);
            }
            3..=6 => {
                let found = model.iter().position(|v| v.0 == name);
                let expected = found.map(|p| {
                    let v = model.remove(p).unwrap();
                    let bits = v.1.clone();
                    model.push_back(v);
                    bits
                });
                assert_eq!(
                    c.get(&name)
                        .map(|a| a.samples.iter().map(|v| v.to_bits()).collect::<Vec<_>>()),
                    expected,
                    "cache LRU model hit mismatch"
                );
            }
            _ => {
                let capacity = [0, 1, 2, 4][(seed as usize >> 16) & 3];
                let mut samples = Vec::with_capacity(capacity);
                if capacity != 0 {
                    samples.push(step as f32 / 8192.0);
                }
                let bytes = samples.capacity() * 4;
                let bits = samples.iter().map(|v| v.to_bits()).collect();
                let mut used: usize = model.iter().map(|v| v.2).sum();
                while used + bytes > 28 && !model.is_empty() {
                    used -= model.pop_front().unwrap().2;
                }
                model.retain(|v| v.0 != name);
                model.push_back((name.clone(), bits, bytes));
                c.insert(
                    name,
                    DecodedAudio {
                        samples,
                        sample_rate: 48000,
                        channels: 1,
                    },
                )
                .unwrap();
            }
        }
        assert_eq!(
            order(&c),
            model.iter().map(|v| v.0.clone()).collect::<Vec<_>>(),
            "cache LRU independent order mismatch"
        );
        let i = c.inner.lock().unwrap();
        assert_eq!(i.current_size, model.iter().map(|v| v.2).sum::<usize>());
        assert_eq!(i.entries.len(), model.len());
        for (n, b, _) in &model {
            assert_eq!(
                i.entries
                    .get(n.as_str())
                    .unwrap()
                    .audio
                    .samples
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                *b
            );
        }
    }
    drop((c, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn complete_growth_lru_replacement_and_eof_pcm_trace() {
    let (s, h, l) = setup();
    for channels in [1, 2, 32] {
        let bytes = 8192 * channels as usize * 4;
        let c = cache(&s, &h, &l, bytes * 2);
        for n in 0..512 {
            c.insert(format!("warm-{n}"), pcm(0.0)).unwrap();
        }
        let a = format!("A{}", "x".repeat(8192));
        let b = format!("B{}", "x".repeat(8192));
        let d = format!("C{}", "x".repeat(8192));
        let make = |sentinel| DecodedAudio {
            samples: (0..8192 * channels as usize)
                .map(|n| {
                    if sentinel {
                        0.8125
                    } else {
                        (n % 127) as f32 / 256.0 - 0.25
                    }
                })
                .collect(),
            sample_rate: 48000,
            channels,
        };
        c.insert(a.clone(), make(false)).unwrap();
        c.insert(b.clone(), make(true)).unwrap();
        let mut player = crate::inner_audio::InnerAudioPlayer::new(7, channels);
        let load = player.begin_load().unwrap();
        player
            .complete_cached_load(load, c.get(&a).unwrap())
            .unwrap();
        player.play();
        let mut played = 0usize;
        for frames in [1, 127, 128, 129, 257, 4097, 8193] {
            let mut out = vec![0.375; frames * channels as usize + channels as usize - 1];
            player.process(&mut out);
            let expected: Vec<_> = (0..frames * channels as usize)
                .map(|n| {
                    let frame = played + n / channels as usize;
                    if frame < 8192 {
                        (0.375 + (((played * channels as usize + n) % 127) as f32 / 256.0 - 0.25))
                            .to_bits()
                    } else {
                        0.375f32.to_bits()
                    }
                })
                .collect();
            assert_eq!(
                out[..frames * channels as usize]
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                expected,
                "cache LRU complete PCM changed"
            );
            assert!(
                out[frames * channels as usize..]
                    .iter()
                    .all(|v| *v == 0.375)
            );
            played += frames;
            if frames == 127 {
                c.insert(d.clone(), make(true)).unwrap();
                assert!(c.get(&b).is_none());
            }
            if frames == 129 {
                c.insert(a.clone(), make(true)).unwrap();
            }
            if frames == 257 {
                c.clear();
            }
            let i = c.inner.lock().unwrap();
            assert!(i.lru_valid(), "cache LRU topology invalid");
            let order: Vec<_> = i
                .lru_keys()
                .map(|k| {
                    if k.as_str() == a {
                        1u64
                    } else if k.as_str() == b {
                        2
                    } else {
                        3
                    }
                })
                .collect();
            println!(
                "MIGO_CACHE_LRU_PCM {channels}:{frames} {:?} {:?} {:?}",
                out.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                [
                    player.shared.current_time().to_bits(),
                    player.is_active() as u64,
                    i.entries.len() as u64,
                    i.current_size as u64,
                    i.access_counter
                ],
                order
            );
        }
        drop((player, c));
        wait(|| l.used_bytes() == HANDOFF_BYTES);
    }
    drop(h);
    wait(|| l.used_bytes() == 0);
}
