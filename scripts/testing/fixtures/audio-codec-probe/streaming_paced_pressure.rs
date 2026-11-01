//! Playback uses the production consumer and an ordinary background queue.
//! The host timer is observable; it is not an audio-device callback clock.
use super::*;
use crate::inner_audio::{InnerAudioPlayer, PlaybackState};
use shared::protocol::audio_cmd::InnerAudioEventType;
use std::sync::atomic::AtomicUsize;

const QUANTUM: usize = 128;

fn mixed_reference(bits: &[u32], rate: u32) -> Vec<u32> {
    let mut player = InnerAudioPlayer::new(81, 2);
    player
        .load_audio(DecodedAudio {
            samples: bits.iter().copied().map(f32::from_bits).collect(),
            sample_rate: rate,
            channels: 2,
        })
        .unwrap();
    player.play();
    let mut result = Vec::with_capacity(bits.len() + QUANTUM * 2);
    let mut output = [0.; QUANTUM * 2];
    for _ in 0..(bits.len() / 2).div_ceil(QUANTUM) {
        output.fill(0.);
        player.process(&mut output);
        result.extend(output.iter().map(|value| value.to_bits()));
    }
    assert_eq!(
        player.shared.state(),
        PlaybackState::Stopped,
        "reference did not finish"
    );
    result.truncate(bits.len());
    assert!(
        result.iter().any(|&value| value & 0x7fff_ffff != 0),
        "silent render reference"
    );
    result
}

struct Load {
    stop: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<(usize, u128)>>,
}
impl Load {
    fn new(hosts: Arc<Vec<migo_io::scheduler::IoScheduler>>, kind: &str, jobs: usize) -> Self {
        let data = Arc::new(match kind {
            "wave" => wav(48000, 2_000_000),
            "convert" => wav(32000, 32768),
            _ => panic!("unknown background format"),
        });
        let expected =
            crate::resampler::resample_if_needed(crate::decoder::decode(&data).unwrap(), 48000)
                .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicUsize::new(0));
        let stopped = stop.clone();
        let progress = completed.clone();
        let worker = thread::spawn(move || {
            let mut owners: Vec<_> = hosts.iter().map(Background::new).collect();
            assert!(
                owners.iter().all(|owner| owner.pool.step_hook.is_none()),
                "background entry hook installed"
            );
            let mut count = 0;
            let mut longest = 0;
            while !stopped.load(Ordering::Acquire) {
                let start = Instant::now();
                for job in 0..jobs {
                    owners[job % hosts.len()].submit(&data);
                }
                for owner in &mut owners {
                    // Unoptimized sinc under sustained foreground load still makes CPU
                    // progress, but a whole result can exceed the gated probe's
                    // five-second wait. Completion/progress assertions stay below.
                    let timeout = if cfg!(debug_assertions) {
                        Duration::from_secs(15)
                    } else {
                        WAIT
                    };
                    count += owner.complete_with_timeout(&expected, timeout);
                }
                longest = longest.max(start.elapsed().as_nanos());
                assert_eq!(
                    process_decode_in_flight_budget()
                        .bytes
                        .load(Ordering::Acquire),
                    0
                );
                assert_eq!(Arc::strong_count(&data), 1);
                progress.store(count, Ordering::Release);
            }
            (count, longest)
        });
        Self {
            stop,
            completed,
            worker: Some(worker),
        }
    }
    fn finish(&mut self) -> (usize, u128) {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("background worker failed")
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Consumer {
    stream: Stream,
    player: InnerAudioPlayer,
    expected: Vec<u32>,
    rendered: usize,
    ended: usize,
    waiting: usize,
    missing_frames: usize,
}
impl Consumer {
    fn new(host: &migo_io::scheduler::IoScheduler, rate: u32, expected: &[u32]) -> Self {
        let mut stream = Stream::new(host, rate);
        let mut player = InnerAudioPlayer::new(82, 2);
        player
            .start_streaming(
                stream.server.url.clone(),
                stream.rx.take().unwrap(),
                stream.state.clone(),
            )
            .unwrap();
        // The complete response is allowed before playback. There is no source
        // credit, CPU-entry gate, or test step hook during the measured interval.
        for _ in MUSIC.chunks(CHUNK) {
            stream.server.next();
        }
        let deadline = Instant::now() + WAIT;
        while !player.shared.is_loaded() {
            player.poll_stream();
            assert!(
                Instant::now() < deadline,
                "production player did not preload"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !player.is_stream_complete(),
            "whole stream decoded before paced consumption"
        );
        player.play();
        player.drain_events().for_each(drop);
        Self {
            stream,
            player,
            expected: expected.to_vec(),
            rendered: 0,
            ended: 0,
            waiting: 0,
            missing_frames: 0,
        }
    }
    fn tick(&mut self) {
        if self.ended != 0 {
            return;
        }
        self.player.poll_stream();
        let before = self.player.shared.position_frames() as usize;
        assert_eq!(
            before, self.rendered,
            "stream cursor changed between callbacks"
        );
        let mut output = [0.; QUANTUM * 2];
        let active = self.player.process(&mut output);
        let count = if active {
            let after = self.player.shared.position_frames() as usize;
            assert!(after >= before, "stream cursor moved backwards");
            after - before
        } else {
            self.expected.len() / 2 - before
        };
        assert!(count <= QUANTUM, "consumer skipped media frames");
        assert!(
            output[..count * 2]
                .iter()
                .map(|v| v.to_bits())
                .eq(self.expected[before * 2..(before + count) * 2]
                    .iter()
                    .copied()),
            "paced rendered PCM changed"
        );
        assert!(
            output[count * 2..].iter().all(|&value| value == 0.),
            "missing source overwrote output tail"
        );
        self.rendered += count;
        if active {
            self.missing_frames += QUANTUM - count;
        }
        for event in self.player.drain_events() {
            match event.event_type {
                InnerAudioEventType::Error => panic!("production player reported an error"),
                InnerAudioEventType::Waiting => self.waiting += 1,
                InnerAudioEventType::Ended => self.ended += 1,
                _ => {}
            }
        }
        if !active {
            assert_eq!(self.ended, 1, "player stopped without exact Ended");
            assert_eq!(
                self.rendered * 2,
                self.expected.len(),
                "player ended before complete PCM"
            );
        }
    }
}

#[test]
fn sample_clock_consumption_preserves_pcm_under_unbarriered_pressure() {
    let rate: u32 = std::env::var("MIGO_PACED_RATE")
        .unwrap_or_else(|_| "48000".into())
        .parse()
        .unwrap();
    let host_count: usize = std::env::var("MIGO_PACED_HOSTS")
        .unwrap_or_else(|_| "2".into())
        .parse()
        .unwrap();
    let stream_count: usize = std::env::var("MIGO_PACED_STREAMS")
        .unwrap_or_else(|_| "2".into())
        .parse()
        .unwrap();
    let pressure = std::env::var("MIGO_PACED_PRESSURE").unwrap_or_else(|_| "1".into()) == "1";
    let kind = std::env::var("MIGO_PACED_KIND").unwrap_or_else(|_| "wave".into());
    assert!([1, 2].contains(&host_count) && [1, 2].contains(&stream_count));
    let hosts = Arc::new(
        (0..host_count)
            .map(|id| migo_io::scheduler::IoScheduler::new(id as i32))
            .collect::<Vec<_>>(),
    );
    let mut reference = Stream::new(&hosts[0], rate);
    for _ in MUSIC.chunks(CHUNK) {
        reference.server.next();
    }
    reference.finish();
    let expected = mixed_reference(&reference.bits, rate);
    let mut consumers = (0..stream_count)
        .map(|id| Consumer::new(&hosts[id % host_count], rate, &expected))
        .collect::<Vec<_>>();
    let workers = thread::available_parallelism().unwrap().get().clamp(2, 6);
    until(|| io_workers() == workers);
    let actual_workers = io_workers();
    assert_eq!(actual_workers, workers);
    let cpu = hosts[0].audio_cpu();
    let initial_generation = cpu.refill_generation();
    let mut last_generation = initial_generation;
    let mut load = if pressure {
        Some(Load::new(hosts.clone(), &kind, (workers - 1).clamp(1, 2)))
    } else {
        None
    };
    println!("MIGO_PACED_PLAYING");
    let start = Instant::now();
    let mut anchor = start;
    let mut ticks = 0u64;
    let mut late = Vec::new();
    let mut resyncs = 0;
    let mut coexist = 0;
    let mut peak_reserved = 0;
    let mut next_progress = start + Duration::from_secs(1);
    let mut last_completed = 0;
    let mut progress_windows = 0;
    let media = Duration::from_nanos((expected.len() / 2) as u64 * 1_000_000_000 / u64::from(rate));
    let period = Duration::from_nanos(QUANTUM as u64 * 1_000_000_000 / u64::from(rate));
    while consumers.iter().any(|consumer| consumer.ended == 0) {
        assert!(
            start.elapsed() < media + Duration::from_secs(20),
            "paced playback did not complete"
        );
        ticks += 1;
        let deadline =
            anchor + Duration::from_nanos(ticks * QUANTUM as u64 * 1_000_000_000 / u64::from(rate));
        let now = Instant::now();
        if now < deadline {
            thread::sleep(deadline - now);
        }
        let delay = Instant::now().saturating_duration_since(deadline);
        late.push(delay.as_nanos());
        if delay >= period {
            anchor += delay;
            resyncs += 1;
        }
        for consumer in &mut consumers {
            consumer.tick();
        }
        let reserved = process_decode_in_flight_budget()
            .bytes
            .load(Ordering::Acquire);
        peak_reserved = peak_reserved.max(reserved);
        let generation = cpu.refill_generation();
        if generation != last_generation && reserved != 0 {
            coexist += 1;
        }
        last_generation = generation;
        if Instant::now() >= next_progress {
            let count = load
                .as_ref()
                .map_or(0, |load| load.completed.load(Ordering::Acquire));
            if count > last_completed {
                progress_windows += 1;
            }
            last_completed = count;
            next_progress = Instant::now() + Duration::from_secs(1);
        }
    }
    let elapsed = start.elapsed();

    assert!(elapsed >= media, "consumer ran ahead of sample clock");
    let (completed, longest) = load.as_mut().map_or((0, 0), Load::finish);
    if pressure {
        assert!(completed >= 2, "background work did not progress");
        assert!(progress_windows >= 2, "background stalled during playback");
        assert!(
            coexist >= 2,
            "no real foreground/background coexistence observed"
        );
    } else {
        assert_eq!(completed, 0);
    }
    assert!(
        peak_reserved <= MAX_DECODE_IN_FLIGHT_BYTES,
        "shared memory reservation exceeded"
    );
    assert_eq!(
        process_decode_in_flight_budget()
            .bytes
            .load(Ordering::Acquire),
        0
    );
    let missing: usize = consumers.iter().map(|c| c.missing_frames).sum();
    let waiting: usize = consumers.iter().map(|c| c.waiting).sum();
    for consumer in &consumers {
        assert_eq!(consumer.ended, 1);
        assert_eq!(
            consumer
                .stream
                .state
                .bytes_downloaded
                .load(Ordering::Acquire),
            MUSIC.len() as u64
        );
        assert!(consumer.player.is_stream_complete());
    }
    drop(consumers);
    late.sort_unstable();
    println!(
        "MIGO_PACED {{\"rate\":{rate},\"hosts\":{host_count},\"streams\":{stream_count},\"pressure\":{pressure},\"background_kind\":\"{kind}\",\"io_workers\":{actual_workers},\"background_completed\":{completed},\"background_progress_windows\":{progress_windows},\"longest_background_batch_ns\":{longest},\"coexist_observations\":{coexist},\"callbacks\":{ticks},\"callback_p99_late_ns\":{},\"callback_max_late_ns\":{},\"clock_resyncs\":{resyncs},\"player_missing_frames\":{missing},\"player_waiting_events\":{waiting},\"elapsed_ns\":{},\"media_frames\":{},\"foreground_requests\":{},\"peak_reserved_bytes\":{peak_reserved},\"complete_mixed_pcm_bits_checked\":true}}",
        late[(late.len() - 1) * 99 / 100],
        late.last().unwrap(),
        elapsed.as_nanos(),
        expected.len() / 2,
        cpu.refill_generation().wrapping_sub(initial_generation)
    );
}
