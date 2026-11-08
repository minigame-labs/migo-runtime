//! Charged stream backing could be freed inline before cache validation/locking.
//! Protect refusal without altering its error, old cache entry or lock ordering.
use super::retirement_tests::{Seen, block, finish, hook, host, managed, pcm, sentinel};
use super::*;
use std::sync::atomic::Ordering;
use std::time::Duration;
const BYTES: usize = 4 << 20;
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    bytes: usize,
    entries: usize,
    counter: u64,
    pointer: Option<usize>,
    bits: Vec<u32>,
    order: Vec<(u64, String)>,
}
fn snapshot(cache: &GlobalAudioCache) -> Snapshot {
    let inner = cache.inner.lock().unwrap_or_else(|e| e.into_inner());
    let old = inner.entries.get("existing");
    Snapshot {
        bytes: inner.current_size,
        entries: inner.entries.len(),
        counter: inner.access_counter,
        pointer: old.map(|v| v.audio.samples.as_ptr() as usize),
        bits: old
            .map(|v| v.audio.samples.iter().map(|v| v.to_bits()).collect())
            .unwrap_or_default(),
        order: inner.lru_iter().map(|(k, v)| (*k, v.to_string())).collect(),
    }
}
fn setup(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    legacy: bool,
) -> GlobalAudioCache {
    let cache = if legacy {
        let cache = GlobalAudioCache::new();
        {
            let mut inner = cache.inner.lock().unwrap();
            inner.max_size = BYTES * 2;
            inner.aggregate = ledger.clone();
        }
        cache
    } else {
        managed(cpu.clone(), BYTES * 2, ledger)
    };
    cache.insert("existing".into(), pcm(8)).unwrap();
    cache
}
fn poison(cache: &GlobalAudioCache) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = cache.inner.lock().unwrap();
        panic!("cache rejection fixture poisons metadata mutex");
    }));
    assert!(result.is_err() && cache.inner.is_poisoned());
}
fn input(mode: &str, bytes: usize) -> (DecodedAudio, Option<crate::streaming::StreamTimeline>) {
    let mut data = pcm(bytes);
    let (frames, rate, channels) = match mode {
        "rate" => {
            data.sample_rate = 44100;
            (64, 48000, 2)
        }
        "channels" => (128, 48000, 1),
        "extent" => (65, 48000, 2),
        "partial" => {
            data.samples.push(0.125);
            (64, 48000, 2)
        }
        "zero-channels" => {
            data.channels = 0;
            (64, 48000, 2)
        }
        "permit" => return (data, None),
        _ => (64, 48000, 2),
    };
    (
        data,
        Some(crate::streaming::StreamTimeline::new(frames, rate, 48000, channels).unwrap()),
    )
}
fn error_matches(result: &EngineResult<Arc<CachedAudio>>, mode: &str) -> bool {
    let Err(error) = result else {
        return false;
    };
    if matches!(mode, "valid" | "permit") {
        error.code == ErrorCode::Internal
            && error.detail.as_deref() == Some("audio cache lock poisoned")
    } else {
        error.code == ErrorCode::IoError
            && error.detail.as_deref() == Some("cache timeline does not describe owned PCM")
    }
}
pub(super) fn verify() {
    held_invalid(false);
    held_invalid(true);
    for closed in [false, true] {
        for poisoned in [false, true] {
            for mode in ["rate", "channels", "extent", "partial", "zero-channels"] {
                refusal(mode, closed, poisoned, false);
            }
        }
        for mode in ["valid", "permit"] {
            refusal(mode, closed, true, false);
        }
        success(closed);
    }
    refusal("extent", false, false, true);
    refusal("permit", false, true, true);
    small_refusal();
}
fn held_invalid(closed: bool) {
    let (owner, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES + 8));
    let seen = Arc::new(Seen::default());
    let cache = Arc::new(setup(&cpu, &ledger, false));
    let old = snapshot(&cache);
    let (data, timeline) = input("rate", BYTES);
    let permit = ledger.try_reserve(BYTES, "cache refusal").unwrap();
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    let guard = cache.inner.lock().unwrap();
    let work = cache.clone();
    let observer = hook(&seen);
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let before = migo_alloc_probe::thread_counts();
        let answer = retirement::observe(observer, || {
            work.insert_with_source("existing".into(), data, permit, timeline)
        });
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        send.send((answer, freed)).unwrap();
    });
    let first = receive.recv_timeout(Duration::from_secs(2));
    let prompt = first.is_ok();
    drop(guard);
    let (answer, freed) =
        first.unwrap_or_else(|_| receive.recv_timeout(Duration::from_secs(15)).unwrap());
    worker.join().unwrap();
    let semantic = error_matches(&answer, "rate") && snapshot(&cache) == old;
    drop(answer);
    drop(cache);
    sentinel();
    let held = seen.count.load(Ordering::Acquire) == 0;
    let credit = ledger.used_bytes();
    gate.open();
    assert!(prompt, "invalid cache input waited for metadata lock");
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "rejected cached PCM escaped CPU disposal"
    );
    assert_eq!(
        credit, BYTES,
        "rejected cached PCM credit returned before CPU disposal"
    );
    assert!(semantic, "cache refusal changed typed error or old entry");
    finish(&seen, &ledger, 1);
}
fn refusal(mode: &str, closed: bool, poisoned: bool, legacy: bool) {
    let (owner, cpu) = host();
    let peer = owner.peer_for_admission_probe(921);
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES + 8));
    let seen = Arc::new(Seen::default());
    let cache = setup(&cpu, &ledger, legacy);
    let old = snapshot(&cache);
    if poisoned {
        poison(&cache);
    }
    let (data, timeline) = input(mode, BYTES);
    let permit = ledger.try_reserve(BYTES, "cache refusal").unwrap();
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    let before = migo_alloc_probe::thread_counts();
    let answer = retirement::observe(hook(&seen), || {
        if mode == "permit" {
            cache.insert_with_permit("existing".into(), data, permit)
        } else {
            cache.insert_with_source("existing".into(), data, permit, timeline)
        }
    });
    let semantic = error_matches(&answer, mode) && snapshot(&cache) == old;
    drop(answer);
    drop(cache);
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let count = seen.count.load(Ordering::Acquire);
    let credit = ledger.used_bytes();
    gate.open();
    assert!(semantic, "cache refusal changed typed error or old entry");
    if legacy {
        assert!(
            freed >= BYTES as u64 && count == 1 && credit == 0,
            "legacy rejected cache input lost synchronous disposal"
        );
    } else {
        assert!(
            freed < (BYTES / 2) as u64 && count == 0,
            "rejected cached PCM escaped CPU disposal"
        );
        assert_eq!(
            credit, BYTES,
            "rejected cached PCM credit returned before CPU disposal"
        );
        finish(&seen, &ledger, 1);
    }
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42
    );
}
fn success(closed: bool) {
    let (owner, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES + 8));
    let cache = setup(&cpu, &ledger, false);
    let seen = Arc::new(Seen::default());
    let (data, timeline) = input("valid", BYTES);
    let pointer = data.samples.as_ptr();
    let bits: Vec<_> = data.samples.iter().map(|v| v.to_bits()).collect();
    let permit = ledger.try_reserve(BYTES, "cache success").unwrap();
    let cached = retirement::observe(hook(&seen), || {
        cache.insert_with_source("existing".into(), data, permit, timeline)
    })
    .unwrap();
    assert!(
        cached.samples.as_ptr() == pointer
            && cached.samples.capacity() * 4 == BYTES
            && cached
                .samples
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>()
                == bits
            && cached.source_timeline() == timeline
            && ledger.used_bytes() == BYTES,
        "cache success changed PCM, timeline or original permit"
    );
    assert!(Arc::ptr_eq(&cached, &cache.get("existing").unwrap()));
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    drop(cache);
    assert_eq!(ledger.used_bytes(), BYTES);
    drop(cached);
    sentinel();
    let held = seen.count.load(Ordering::Acquire) == 0;
    let credit = ledger.used_bytes();
    gate.open();
    assert!(
        held && credit == BYTES,
        "accepted prepared cache input escaped CPU disposal"
    );
    finish(&seen, &ledger, 1);
}
fn small_refusal() {
    let (_owner, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(4092 + 8));
    let cache = setup(&cpu, &ledger, false);
    poison(&cache);
    let seen = Arc::new(Seen::default());
    let data = pcm(4092);
    let permit = ledger.try_reserve(4092, "small refusal").unwrap();
    let gate = block(&cpu);
    let answer = retirement::observe(hook(&seen), || {
        cache.insert_with_permit("existing".into(), data, permit)
    });
    assert!(error_matches(&answer, "permit"));
    drop(cache);
    let count = seen.count.load(Ordering::Acquire);
    let credit = ledger.used_bytes();
    gate.open();
    assert!(
        count == 1 && credit == 0,
        "small rejected cache input lost synchronous disposal"
    );
}
