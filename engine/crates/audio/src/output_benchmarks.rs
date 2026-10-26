//! Explicit host callback-to-refill experiment, using the production callback,
//! SPSC ring, AudioSync and wakeup. No device, DSP or speaker latency claim.
use super::*;
use ringbuf::HeapRb;
use ringbuf::traits::{Producer, Split};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
#[ignore = "explicit fresh-process Release ABBA callback-to-refill experiment"]
fn audio_wake_refill_latency() {
    let mode = std::env::var("MIGO_AUDIO_WAKE_BENCH").unwrap();
    let wake = match mode.as_str() {
        "watchdog" => ThreadWakeup::new(),
        "callback" => ThreadWakeup::new_audio(),
        _ => panic!("unknown audio wake experiment arm"),
    };
    const WARMUP: usize = 20;
    const MEASURED: usize = 200;
    const SAMPLES: usize = 128 * 2;
    let count = WARMUP + MEASURED;
    let sync = AudioSync::new();
    sync.bind_wakeup(&wake);
    sync.set_callback_wakeup_enabled(true);
    let (mut producer, consumer) = HeapRb::<f32>::new(SAMPLES).split();
    let pcm = [0.25f32; SAMPLES];
    assert_eq!(producer.push_slice(&pcm), SAMPLES);
    let mut callback = OutputCallback::new(consumer, sync.clone(), SAMPLES / 4);
    let started = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let published = Arc::new(AtomicUsize::new(0));
    let (native_tx, native_rx) = mpsc::channel::<std::thread::Thread>();
    let (ready_tx, ready_rx) = mpsc::channel();
    let origin = Instant::now();
    let owner_completed = completed.clone();
    let owner_started = started.clone();
    let owner_published = published.clone();
    let owner = std::thread::spawn(move || {
        wake.bind_audio_thread();
        let native = native_rx.recv().unwrap();
        let mut elapsed = Vec::with_capacity(MEASURED);
        ready_tx.send(()).unwrap();
        for expected in 1..=count {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !sync.check_and_clear() {
                assert!(
                    Instant::now() < deadline,
                    "producer did not observe a refill request"
                );
                wake.wait_timeout(Duration::from_millis(5));
            }
            assert_eq!(owner_published.load(Ordering::Acquire), expected);
            assert_eq!(
                producer.push_slice(&pcm),
                SAMPLES,
                "one refill per callback"
            );
            let refill_ns = origin.elapsed().as_nanos() as u64;
            let callback_ns = owner_started.load(Ordering::Relaxed);
            if expected > WARMUP {
                elapsed.push(refill_ns - callback_ns);
            }
            owner_completed.store(expected, Ordering::Release);
            native.unpark(); // Ack outside the device callback.
        }
        elapsed
    });
    migo_alloc_probe::on_native_thread(move || {
        // Unlike the cold correctness gate, the steady-state experiment
        // initializes its pacing/ack thread token before the first callback.
        native_tx.send(std::thread::current()).unwrap();
        ready_rx.recv().unwrap();
        let mut out = [0.0f32; SAMPLES];
        for sequence in 1..=count {
            // A synthetic 128-frame/stereo/48kHz callback interval. If refill
            // is late, wait for it; do not pretend this models hardware xruns.
            std::thread::sleep(Duration::from_nanos(2_666_667));
            started.store(origin.elapsed().as_nanos() as u64, Ordering::Relaxed);
            published.store(sequence, Ordering::Release);
            callback.render_native(&mut out);
            assert!(out.iter().all(|sample| *sample == 0.25));
            let deadline = Instant::now() + Duration::from_secs(5);
            while completed.load(Ordering::Acquire) < sequence {
                assert!(Instant::now() < deadline, "refill acknowledgement missing");
                std::thread::park_timeout(Duration::from_millis(5));
            }
        }
    })
    .unwrap();
    let elapsed = owner.join().unwrap();
    println!(
        "MIGO_AUDIO_WAKE_BENCH {{\"mode\":\"{}\",\"warmup\":{},\"measured\":{},\"frames\":128,\"channels\":2,\"sample_rate\":48000,\"watchdog_ms\":5,\"latencies_ns\":{:?}}}",
        mode, WARMUP, MEASURED, elapsed
    );
}
