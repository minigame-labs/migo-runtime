//! The original source quota must survive replacing or dropping its last Vec.
use super::*;
use crate::cache::GlobalAudioCache;
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize},
};
use std::time::{Duration, Instant};
const BYTES: usize = 4 << 20;
#[derive(Default)]
struct Seen {
    count: AtomicUsize,
    wrong: AtomicBool,
}
fn hook(seen: &Arc<Seen>) -> Arc<dyn Fn() + Send + Sync> {
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
            "owned source retirement did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}
struct Block(Arc<(Mutex<bool>, Condvar)>, Arc<AtomicBool>);
impl Drop for Block {
    fn drop(&mut self) {
        self.1.store(true, Ordering::Release);
        let mut open = self.0.0.lock().unwrap();
        while !*open {
            open = self.0.1.wait(open).unwrap();
        }
    }
}
fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Block(gate.0.clone(), entered.clone());
    let retired = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        retired.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    gate
}
fn host() -> (
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(961, 1, u64::MAX);
    let cpu = host.audio_cpu();
    (host, cpu)
}
fn player(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
) -> InnerAudioPlayer {
    let mut player = crate::audio_thread::new_inner_audio_player(1, 2, cpu);
    player.aggregate = ledger.clone();
    player
}
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn pcm(capacity: usize) -> DecodedAudio {
    let mut samples = Vec::with_capacity(capacity);
    samples.extend((0..128).map(sample));
    DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    }
}
fn load(p: &mut InnerAudioPlayer, seen: &Arc<Seen>, capacity: usize) -> InnerAudioLoadToken {
    let load = p.begin_load_for(1).unwrap();
    let mut data = pcm(capacity);
    let ptr = data.samples.as_ptr();
    source_retirement::observe(hook(seen), || p.try_complete_load(&load, &mut data)).unwrap();
    assert_eq!(
        p.source.samples().as_ptr(),
        ptr,
        "owned source installation copied PCM"
    );
    assert!(data.samples.is_empty());
    assert!(
        p.source
            .samples()
            .iter()
            .enumerate()
            .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
        "owned source changed complete PCM"
    );
    load
}
fn done(seen: &Arc<Seen>, ledger: &Arc<AudioAggregateLedger>, count: usize, bytes: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == count);
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "owned source destructor escaped CPU workers"
    );
    assert_eq!(
        ledger.used_bytes(),
        bytes,
        "owned source CPU disposal leaked physical credit"
    );
}
pub(super) fn lifetime() {
    for closed in [false, true] {
        for mode in [
            "drop",
            "begin",
            "fail",
            "attach",
            "cached",
            "empty-transfer",
        ] {
            disposal(mode, closed);
        }
        for mode in ["stale-begin", "timeline", "windowed"] {
            retained(mode, closed);
        }
        burst(closed);
        for mode in ["drop", "fail"] {
            growth(mode, closed);
        }
        for refused in [false, true] {
            cache_transfer(refused, closed);
        }
        for (managed, capacity) in [(false, BYTES / 4), (true, 1023), (true, 1024)] {
            boundary(managed, capacity, closed);
        }
    }
}
fn disposal(mode: &str, closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let seen = Arc::new(Seen::default());
    let mut p = player(&cpu, &ledger);
    let token = load(&mut p, &seen, BYTES / 4);
    p.drain_events().for_each(drop);
    let shared = p.shared.clone();
    let cache = GlobalAudioCache::with_cpu(cpu.clone());
    let cached = if matches!(mode, "attach" | "cached") {
        Some(cache.insert("new".into(), pcm(128)).unwrap())
    } else {
        None
    };
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    let before = migo_alloc_probe::thread_counts();
    match mode {
        "drop" => drop(p),
        "begin" => {
            p.begin_load_for(2).unwrap();
            assert!(!p.shared.is_loaded());
            assert_eq!(p.shared.state(), PlaybackState::Stopped);
            assert_eq!(p.shared.duration_frames(), 0);
            drop(p);
        }
        "fail" => {
            p.fail_stream();
            assert!(!p.shared.is_loaded());
            assert!(
                p.drain_events()
                    .any(|e| e.event_type == InnerAudioEventType::Error)
            );
            drop(p);
        }
        "attach" => {
            p.position = 20;
            p.position_phase = 234;
            p.frames_since_time_update = 17;
            p.shared.set_state(PlaybackState::Playing);
            p.attach_cached_backing(cached.unwrap());
            assert_eq!(
                (p.position, p.position_phase, p.frames_since_time_update),
                (20, 234, 17),
                "owned source cached attachment changed playback cursor"
            );
            assert_eq!(p.shared.state(), PlaybackState::Playing);
            drop(p);
        }
        "cached" => {
            p.shared.set_loaded(false);
            p.complete_cached_load(token, cached.unwrap()).unwrap();
            assert!(p.shared.is_loaded());
            drop(p);
        }
        "empty-transfer" => {
            if let AudioSource::Owned(samples) = &mut p.source {
                samples.clear();
            }
            assert!(p.take_streamed_audio_with_source().is_none());
            drop(p);
        }
        _ => unreachable!(),
    }
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    assert!(
        freed < (BYTES / 2) as u64,
        "owned source destructor escaped CPU workers"
    );
    assert_eq!(
        seen.count.load(Ordering::Acquire),
        0,
        "owned source destructor escaped CPU workers"
    );
    assert_eq!(
        ledger.used_bytes(),
        BYTES,
        "owned source credit returned before CPU disposal"
    );
    assert!(
        ledger.try_reserve(BYTES + 1, "probe").is_err(),
        "owned source reopened physical admission before disposal"
    );
    assert_eq!(
        shared.source_generation.load(Ordering::Acquire),
        0,
        "owned source Drop delayed generation retirement"
    );
    gate.open();
    done(&seen, &ledger, 1, 0);
}
fn retained(mode: &str, closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
    let seen = Arc::new(Seen::default());
    let mut p = player(&cpu, &ledger);
    let _token = load(&mut p, &seen, BYTES / 4);
    let ptr = p.source.samples().as_ptr();
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    match mode {
        "stale-begin" => {
            assert!(p.begin_load_for(1).is_err());
        }
        "timeline" => {
            p.stream_timeline =
                Some(crate::streaming::StreamTimeline::new(64, 48000, 48000, 2).unwrap());
            assert!(p.take_streamed_audio().is_none());
        }
        "windowed" => {
            p.stream_windowed = true;
            assert!(p.take_streamed_audio_with_source().is_none());
        }
        _ => unreachable!(),
    }
    assert_eq!(
        p.source.samples().as_ptr(),
        ptr,
        "owned source rejected operation replaced current PCM"
    );
    assert_eq!(ledger.used_bytes(), BYTES);
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    assert!(
        p.source_retirement.is_some(),
        "owned source retained PCM lost disposal authority"
    );
    gate.open();
    drop(p);
    done(&seen, &ledger, 1, 0);
}
fn burst(closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let seen = Arc::new(Seen::default());
    let mut p = player(&cpu, &ledger);
    let _token = load(&mut p, &seen, BYTES / 4);
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    p.begin_load_for(2).unwrap();
    let second = p.begin_load_for(3).unwrap();
    let mut data = pcm(BYTES / 4);
    source_retirement::observe(hook(&seen), || p.try_complete_load(&second, &mut data)).unwrap();
    p.begin_load_for(4).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        2 * BYTES,
        "owned source replacement burst returned pending credit early"
    );
    let final_load = p.begin_load_for(5).unwrap();
    let mut third = pcm(BYTES / 4);
    assert_eq!(
        p.try_complete_load(&final_load, &mut third)
            .unwrap_err()
            .code,
        ErrorCode::InputSaturated,
        "owned source replacement burst bypassed original aggregate cap"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    done(&seen, &ledger, 2, 0);
    drop(third);
    drop(p);
}
fn growth(mode: &str, closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let seen = Arc::new(Seen::default());
    let mut p = player(&cpu, &ledger);
    let state = StreamingState::new();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    source_retirement::observe(hook(&seen), || {
        p.start_streaming("growth".into(), rx, state)
    })
    .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 48000,
            channels: 2,
        })
        .is_ok()
    );
    let mut pool = crate::streaming::PcmPool::new();
    let mut data = pool.take();
    data.buffer_mut().extend((0..500_000).map(sample));
    assert!(
        tx.try_send(StreamMsg::Samples {
            revision: StreamRevision::INITIAL,
            pcm: data,
        })
        .is_ok()
    );
    p.poll_stream();
    let bytes = p.physical_pcm_bytes();
    assert!(bytes >= 2_000_000);
    assert_eq!(
        ledger.used_bytes(),
        bytes,
        "owned source growth changed capacity admission"
    );
    assert!(
        p.source
            .samples()
            .iter()
            .enumerate()
            .all(|(i, v)| v.to_bits() == sample(i).to_bits())
    );
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    let before = migo_alloc_probe::thread_counts();
    if mode == "fail" {
        p.fail_stream();
    }
    drop(p);
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (bytes / 2) as u64,
        "owned source destructor escaped CPU workers"
    );
    assert_eq!(
        ledger.used_bytes(),
        bytes,
        "owned source credit returned before CPU disposal"
    );
    gate.open();
    done(&seen, &ledger, 1, 0);
}
fn cache_transfer(refused: bool, closed: bool) {
    let (host, cpu) = host();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
    let seen = Arc::new(Seen::default());
    let cached_seen = Arc::new(Seen::default());
    let mut p = player(&cpu, &ledger);
    let _token = load(&mut p, &seen, BYTES / 4);
    let ptr = p.source.samples().as_ptr();
    let cache = GlobalAudioCache::with_cpu(cpu.clone());
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    if refused {
        let (audio, permit, _) = p.take_streamed_audio_with_source().unwrap();
        assert!(
            p.source_retirement.is_none(),
            "owned source extraction kept a stale disposal slot"
        );
        let wrong = crate::streaming::StreamTimeline::new(65, 48000, 48000, 2).unwrap();
        let before = migo_alloc_probe::thread_counts();
        let result = crate::cache::observe_retirement(hook(&cached_seen), || {
            cache.insert_with_source("refused".into(), audio, permit, Some(wrong))
        });
        assert!(result.is_err());
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (BYTES / 2) as u64,
            "owned source cache transfer destroyed PCM inline"
        );
        drop(p);
    } else {
        p.loading_url = Some("transfer".into());
        let mut players = std::collections::HashMap::from([(1, p)]);
        crate::cache::observe_retirement(hook(&cached_seen), || {
            crate::audio_thread::inner_source_cache_service(&mut players, &cache)
        });
        let retained = cache
            .get("transfer")
            .expect("owned source cache transfer lost completed backing");
        assert_eq!(
            retained.samples.as_ptr(),
            ptr,
            "owned source cache handoff copied PCM"
        );
        assert!(
            retained
                .samples
                .iter()
                .enumerate()
                .all(|(i, v)| v.to_bits() == sample(i).to_bits())
        );
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "owned source cache handoff double charged storage"
        );
        cache.clear();
        drop(players);
        assert_eq!(ledger.used_bytes(), BYTES);
        assert_eq!(
            seen.count.load(Ordering::Acquire),
            0,
            "owned source cache handoff retired transferred PCM"
        );
        let before = migo_alloc_probe::thread_counts();
        drop(retained);
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (BYTES / 2) as u64,
            "owned source cache transfer destroyed PCM inline"
        );
    }
    assert_eq!(
        ledger.used_bytes(),
        BYTES,
        "owned source cache handoff returned physical credit early"
    );
    assert_eq!(cached_seen.count.load(Ordering::Acquire), 0);
    gate.open();
    done(&cached_seen, &ledger, 1, 0);
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
}
fn boundary(managed: bool, capacity: usize, closed: bool) {
    let (host, cpu) = host();
    let bytes = capacity * 4;
    let ledger = Arc::new(AudioAggregateLedger::new(bytes));
    let seen = Arc::new(Seen::default());
    let mut p = if managed {
        player(&cpu, &ledger)
    } else {
        InnerAudioPlayer::new_with_aggregate(1, 2, ledger.clone())
    };
    let _token = load(&mut p, &seen, capacity);
    let gate = block(&cpu);
    if closed {
        drop(host);
    }
    let before = migo_alloc_probe::thread_counts();
    drop(p);
    let deferred = managed && bytes >= 4096;
    assert_eq!(
        ledger.used_bytes(),
        if deferred { bytes } else { 0 },
        "owned source small or legacy boundary changed"
    );
    if !deferred {
        assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes as u64);
    }
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    if deferred {
        done(&seen, &ledger, 1, 0);
    }
}
