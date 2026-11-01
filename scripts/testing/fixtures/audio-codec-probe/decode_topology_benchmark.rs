//! Fixed complete-PCM work across executor topologies and one/two owners.
use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static CONTENDED: AtomicBool = AtomicBool::new(false);
static BACKGROUND_ENTERED: AtomicUsize = AtomicUsize::new(0);
static FOREGROUND_POLLED: AtomicBool = AtomicBool::new(false);

pub(super) fn background_started() {
    if !CONTENDED.load(Ordering::Acquire) {
        return;
    }
    BACKGROUND_ENTERED.fetch_add(1, Ordering::AcqRel);
    let end = Instant::now() + Duration::from_secs(5);
    while !FOREGROUND_POLLED.load(Ordering::Acquire) {
        assert!(Instant::now() < end, "foreground probe was not polled");
        thread::yield_now();
    }
}

fn cpu_ns() -> u128 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut now) },
        0
    );
    now.tv_sec as u128 * 1_000_000_000 + now.tv_nsec as u128
}
fn io_workers() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .unwrap_or_default()
                .starts_with("Migo-IO-")
        })
        .count()
}
struct Owner {
    host: migo_io::scheduler::IoScheduler,
    pool: LazyDecodePool,
    results: DecodeQueueReceiver<DecodeResult>,
}
fn execute(
    owners: &mut [Owner],
    data: &Arc<Vec<u8>>,
    rate: u32,
    refill: bool,
) -> (Vec<DecodedAudio>, Vec<u128>) {
    CONTENDED.store(refill, Ordering::Release);
    BACKGROUND_ENTERED.store(0, Ordering::Release);
    FOREGROUND_POLLED.store(false, Ordering::Release);
    let expected = owners.len();
    let start = Arc::new(AtomicBool::new(false));
    let probe = refill.then(|| {
        let cpu = owners[0].host.audio_cpu();
        let start = start.clone();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            while !start.load(Ordering::Acquire) {
                thread::yield_now();
            }
            let end = Instant::now() + Duration::from_secs(5);
            while BACKGROUND_ENTERED.load(Ordering::Acquire) != expected {
                assert!(Instant::now() < end, "background jobs did not enter CPU");
                thread::yield_now();
            }
            runtime.block_on(async {
                let mut times = Vec::with_capacity(16);
                for _ in 0..16 {
                    let at = Instant::now();
                    let operation = cpu.run(|| 17);
                    tokio::pin!(operation);
                    let result = std::future::poll_fn(|cx| {
                        let ready = operation.as_mut().poll(cx);
                        FOREGROUND_POLLED.store(true, Ordering::Release);
                        ready
                    })
                    .await;
                    assert_eq!(result.unwrap(), 17);
                    times.push(at.elapsed().as_nanos());
                }
                times
            })
        })
    });
    let mut replies = Vec::new();
    for (id, owner) in owners.iter_mut().enumerate() {
        let (resp, reply) = tokio::sync::oneshot::channel();
        replies.push(reply);
        owner.pool.submit(DecodeJob::AudioBuffer {
            ctx_id: id as u32,
            target_rate: rate,
            data: data.clone(),
            resp,
        });
    }
    start.store(true, Ordering::Release);
    let decoded = owners
        .iter()
        .map(|owner| {
            let DecodeResult::AudioBuffer { result, .. } =
                owner.results.recv_timeout(Duration::from_secs(30)).unwrap()
            else {
                panic!("buffer");
            };
            result.unwrap()
        })
        .collect();
    let latencies = probe.map_or_else(Vec::new, |task| task.join().unwrap());
    // The old pool publishes before its in-flight permit drops. Include that
    // retirement in the timed round, rather than admitting the next two jobs
    // against the previous round's still-live budget.
    let end = Instant::now() + Duration::from_secs(5);
    while owners.iter().any(|owner| {
        owner
            .pool
            .pool
            .as_ref()
            .unwrap()
            .in_flight
            .bytes
            .load(Ordering::Acquire)
            != 0
    }) {
        assert!(Instant::now() < end, "decode budget did not retire");
        thread::yield_now();
    }

    if refill {
        assert_eq!(BACKGROUND_ENTERED.load(Ordering::Acquire), expected);
        assert!(FOREGROUND_POLLED.load(Ordering::Acquire));
    }
    (decoded, latencies)
}
#[test]
#[ignore = "isolated topology CPU/wall/RSS and foreground probe"]
fn complete_pcm_across_topologies() {
    let case = std::env::var("MIGO_TOPOLOGY_CASE").unwrap();
    let loops: usize = std::env::var("MIGO_TOPOLOGY_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let host_count: usize = std::env::var("MIGO_TOPOLOGY_HOSTS")
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&host_count) && loops > 0);
    let refill = std::env::var("MIGO_TOPOLOGY_REFILL").unwrap() == "1";
    let bytes = match case.as_str() {
        "wave" => tests::wav(48000, 384000),
        "tiny" => tests::wav(48000, 32),
        "mp3" | "convert" => include_bytes!("fixtures/music.mp3").to_vec(),
        "vorbis" => include_bytes!("fixtures/cross.ogg").to_vec(),
        _ => panic!("unknown case"),
    };
    let native = crate::decoder::decode(&bytes).unwrap();
    let rate = if case == "convert" {
        48000
    } else {
        native.sample_rate
    };
    let reference = crate::resampler::resample_if_needed(native, rate).unwrap();
    let data = Arc::new(bytes);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut owners = Vec::new();
    for id in 0..host_count {
        let host = migo_io::scheduler::IoScheduler::new(id as i32);
        // Both arms model a process whose common IO executor is already live.
        assert_eq!(runtime.block_on(host.audio_cpu().run(|| 23)).unwrap(), 23);
        let (tx, results) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
        owners.push(Owner {
            pool: super::benchmark_pool(tx, rate, &host),
            host,
            results,
        });
    }
    let expected_workers = std::thread::available_parallelism()
        .unwrap()
        .get()
        .clamp(2, 6);
    let end = Instant::now() + Duration::from_secs(5);
    while io_workers() != expected_workers {
        assert!(
            Instant::now() < end,
            "executor worker names did not initialize"
        );
        thread::yield_now();
    }
    assert_eq!(owners.len(), host_count);
    let workers = io_workers();
    assert_eq!(
        workers,
        std::thread::available_parallelism()
            .unwrap()
            .get()
            .clamp(2, 6)
    );
    let check = |audio: &DecodedAudio| {
        assert_eq!(
            (audio.channels, audio.sample_rate, audio.samples.len()),
            (
                reference.channels,
                reference.sample_rate,
                reference.samples.len()
            )
        );
        assert!(
            audio
                .samples
                .iter()
                .zip(&reference.samples)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "complete PCM differs"
        );
    };
    let at = Instant::now();
    let (cold, _) = execute(&mut owners, &data, rate, refill);
    let cold_ns = at.elapsed().as_nanos();
    assert_eq!(cold.len(), host_count);
    for audio in &cold {
        check(audio);
    }
    drop(cold);
    let mut cpu = 0;
    let mut wall = 0;
    let mut bits = 0_u64;
    let mut latencies = Vec::new();
    for _ in 0..loops {
        let start_cpu = cpu_ns();
        let start = Instant::now();
        let (decoded, foreground) = execute(&mut owners, &data, rate, refill);
        wall += start.elapsed().as_nanos();
        cpu += cpu_ns() - start_cpu;
        assert_eq!(decoded.len(), host_count);
        for audio in decoded {
            check(&audio);
            for sample in audio.samples {
                bits = bits
                    .wrapping_mul(31)
                    .wrapping_add(u64::from(sample.to_bits()));
            }
        }
        latencies.extend(foreground);
    }
    latencies.sort_unstable();
    let p99 = latencies
        .get((latencies.len() * 99 / 100).min(latencies.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0);
    println!(
        "MIGO_TOPOLOGY {{\"case\":\"{case}\",\"loops\":{loops},\"hosts\":{host_count},\"refill\":{refill},\"io_workers\":{workers},\"cpu_ns\":{cpu},\"wall_ns\":{wall},\"cold_ns\":{cold_ns},\"pcm_bits\":{bits},\"samples\":{},\"rate\":{rate},\"pcm_checked\":true,\"foreground_count\":{},\"foreground_p99_ns\":{p99}}}",
        reference.samples.len(),
        latencies.len()
    );
}
