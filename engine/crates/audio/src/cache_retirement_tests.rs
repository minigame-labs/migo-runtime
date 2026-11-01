//! Eviction releases cache accounting, but final shared backing and its physical
//! credit must survive until the CPU really destroys the original allocation.
use super::*;
use std::sync::{
    Condvar,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
const BYTES: usize = 4 << 20;
#[derive(Default)]
pub(super) struct Seen {
    pub(super) count: AtomicUsize,
    pub(super) wrong: AtomicBool,
}
pub(super) fn hook(seen: &Arc<Seen>) -> Arc<dyn Fn() + Send + Sync> {
    let seen = seen.clone();
    Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            seen.wrong.store(true, Ordering::Release);
        }
        seen.count.fetch_add(1, Ordering::Release);
    })
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "cached PCM retirement did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
pub(super) struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    pub(super) fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}
struct Blocked {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}
impl Drop for Blocked {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        let mut open = self.gate.0.lock().unwrap();
        while !*open {
            open = self.gate.1.wait(open).unwrap();
        }
    }
}
pub(super) fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Blocked {
        gate: gate.0.clone(),
        entered: entered.clone(),
    };
    let retired = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        retired.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    gate
}
pub(super) fn sentinel() {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}
pub(super) fn host() -> (
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(911, 1, u64::MAX);
    let cpu = host.audio_cpu();
    (host, cpu)
}
pub(super) fn pcm(bytes: usize) -> DecodedAudio {
    let mut samples = Vec::with_capacity(bytes / 4);
    samples.extend((0..128.min(bytes / 4)).map(|i| {
        if i == 0 {
            -0.0
        } else {
            (i % 31) as f32 / 32.0 - 0.5
        }
    }));
    DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    }
}
pub(super) fn managed(
    cpu: migo_io::audio_cpu::AudioCpu,
    limit: usize,
    ledger: &Arc<AudioAggregateLedger>,
) -> GlobalAudioCache {
    let cache = GlobalAudioCache::with_cpu(cpu);
    {
        let mut inner = cache.inner.lock().unwrap();
        inner.max_size = limit;
        inner.aggregate = ledger.clone();
    }
    cache
}
pub(super) fn finish(seen: &Arc<Seen>, ledger: &Arc<AudioAggregateLedger>, count: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == count && ledger.used_bytes() == 0);
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "cached PCM destructor escaped CPU workers"
    );
}
pub(super) fn verify() {
    for closed in [false, true] {
        for mode in [
            "remove",
            "clear",
            "lru",
            "replace",
            "cache-drop",
            "oversized",
            "late-owner",
            "player",
        ] {
            for supplied in [false, true] {
                lifecycle(mode, closed, supplied);
            }
        }
        production_service(closed);
    }
    capacity_bound();
    small_and_legacy();
}
fn lifecycle(mode: &str, closed: bool, supplied: bool) {
    let (host, cpu) = host();
    let peer = host.peer_for_admission_probe(912);
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES * 2));
    let cache = managed(
        cpu.clone(),
        if mode == "oversized" {
            BYTES / 2
        } else {
            BYTES
        },
        &ledger,
    );
    let data = pcm(BYTES);
    let address = data.samples.as_ptr();
    let bits: Vec<_> = data.samples.iter().map(|v| v.to_bits()).collect();
    let seen = Arc::new(Seen::default());
    let timeline = crate::streaming::StreamTimeline::new(64, 48000, 48000, 2).unwrap();
    let cached = retirement::observe(hook(&seen), || {
        if supplied {
            let permit = ledger.try_reserve(BYTES, "cache probe").unwrap();
            cache
                .insert_with_source("original".into(), data, permit, Some(timeline))
                .unwrap()
        } else {
            cache.insert("original".into(), data).unwrap()
        }
    });
    assert!(
        cached.samples.as_ptr() == address
            && cached.samples.capacity() * 4 == BYTES
            && cached
                .samples
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>()
                == bits,
        "cache adoption copied or changed original PCM allocation"
    );
    assert_eq!(
        cached.source_timeline(),
        supplied.then_some(timeline),
        "cached PCM lost source timeline"
    );
    assert_eq!(
        ledger.used_bytes(),
        BYTES,
        "cache adopted PCM without original physical credit"
    );
    let mut cache = Some(cache);
    let mut cached = Some(cached);
    let mut player = None;
    if mode == "player" {
        let mut p =
            crate::inner_audio::InnerAudioPlayer::new_with_aggregate(913, 2, ledger.clone());
        p.attach_cached_backing(cached.as_ref().unwrap().clone());
        player = Some(p);
    }
    if !matches!(mode, "late-owner" | "player" | "oversized") {
        drop(cached.take());
    }
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    match mode {
        "remove" | "late-owner" | "player" => cache.as_ref().unwrap().remove("original"),
        "clear" => cache.as_ref().unwrap().clear(),
        "lru" => {
            cache
                .as_ref()
                .unwrap()
                .insert("new".into(), pcm(8))
                .unwrap();
            assert!(
                cache.as_ref().unwrap().get("original").is_none(),
                "cache eviction changed original LRU policy"
            );
        }
        "replace" => {
            cache
                .as_ref()
                .unwrap()
                .insert("original".into(), pcm(8))
                .unwrap();
            assert_eq!(
                cache
                    .as_ref()
                    .unwrap()
                    .get("original")
                    .unwrap()
                    .samples
                    .capacity(),
                2
            );
        }
        "cache-drop" | "oversized" => {}
        _ => unreachable!(),
    }
    drop(cache.take());
    if matches!(mode, "late-owner" | "player") {
        assert!(
            seen.count.load(Ordering::Acquire) == 0 && ledger.used_bytes() == BYTES,
            "cache eviction released a live external backing"
        );
        if let Some(value) = &cached {
            assert_eq!(value.samples.as_ptr(), address);
        }
    }
    drop(cached.take());
    drop(player.take());
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = seen.count.load(Ordering::Acquire) == 0;
    let credit = ledger.used_bytes();
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "cached PCM destructor escaped CPU workers"
    );
    assert_eq!(
        credit, BYTES,
        "cached PCM physical credit returned before actual CPU disposal"
    );
    finish(&seen, &ledger, 1);
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42
    );
}
fn production_service(closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES * 2));
    let seen = Arc::new(Seen::default());
    let (cache, players) = retirement::observe(hook(&seen), || {
        crate::audio_thread::cache_retirement_service_probe(cpu.clone(), ledger.clone())
    });
    let charged = ledger.used_bytes();
    assert!(charged >= BYTES);
    cache.clear();
    assert_eq!(
        ledger.used_bytes(),
        charged,
        "production cache released player-owned PCM"
    );
    drop(cache);
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    drop(players);
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = seen.count.load(Ordering::Acquire) == 0;
    let credit = ledger.used_bytes();
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "production cache backing bypassed CPU disposal"
    );
    assert_eq!(
        credit, charged,
        "production cache returned transferred stream credit early"
    );
    finish(&seen, &ledger, 1);
}
fn capacity_bound() {
    let (_host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES * 2));
    let seen = Arc::new(Seen::default());
    let mut caches = Vec::new();
    for i in 0..2 {
        let cache = managed(cpu.clone(), BYTES, &ledger);
        retirement::observe(hook(&seen), || cache.insert(format!("{i}"), pcm(BYTES))).unwrap();
        caches.push(cache);
    }
    let gate = block(&cpu);
    drop(caches);
    sentinel();
    let credit = ledger.used_bytes();
    let refused = ledger.try_reserve(1, "pending cache").is_err();
    gate.open();
    assert!(
        credit == BYTES * 2 && refused,
        "cached PCM retirement bypassed original shared physical limit"
    );
    finish(&seen, &ledger, 2);
    assert!(ledger.try_reserve(BYTES * 2, "reclaimed cache").is_ok());
}
fn small_and_legacy() {
    for bytes in [0, 4092, 4096, 8192, BYTES] {
        for legacy in [false, true] {
            let (_host, cpu) = host();
            let ledger = Arc::new(AudioAggregateLedger::new(BYTES * 2));
            let seen = Arc::new(Seen::default());
            let cache = if legacy {
                GlobalAudioCache {
                    inner: Mutex::new(AudioCache::with_max_size_and_aggregate(
                        BYTES,
                        ledger.clone(),
                    )),
                    cpu: None,
                }
            } else {
                managed(cpu.clone(), BYTES, &ledger)
            };
            let value =
                retirement::observe(hook(&seen), || cache.insert("boundary".into(), pcm(bytes)))
                    .unwrap();
            let gate = block(&cpu);
            drop(cache);
            drop(value);
            sentinel();
            let count = seen.count.load(Ordering::Acquire);
            let credit = ledger.used_bytes();
            gate.open();
            if !legacy && bytes >= 4096 {
                assert!(
                    count == 0 && credit == bytes,
                    "managed cached PCM ignored allocation capacity boundary"
                );
                finish(&seen, &ledger, 1);
            } else {
                assert!(
                    count == 1 && credit == 0,
                    "small or standalone cached PCM lost synchronous disposal"
                );
            }
        }
    }
}
