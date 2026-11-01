//! Exercise the production HTTP/MP3/PCM path against whole-file owners.
//! Gates establish overlap; their wall times are not device deadline evidence.
use super::super::tests::{audio_buffer_decode_job, wav};
use super::*;
use crate::streaming::{HttpClient, StreamMsg, StreamingState};
use std::{
    hash::{Hash, Hasher},
    io::{Read, Write},
    net::TcpListener,
    sync::{Mutex, Weak, mpsc},
};

const WAIT: Duration = Duration::from_secs(5);
const CHUNK: usize = 4096;
const MUSIC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/decoder/mp3/fixtures/music.mp3"
));

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "actual pipeline made no progress"
        );
        thread::yield_now();
    }
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

struct Server {
    url: String,
    credit: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/audio.mp3", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (credit, permits) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + WAIT;
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return,
                }
            };
            socket.set_read_timeout(Some(WAIT)).unwrap();
            socket.set_write_timeout(Some(WAIT)).unwrap();
            socket.set_nodelay(true).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if socket.read_exact(&mut byte).is_err() {
                    return;
                }
                header.push(byte[0]);
                assert!(header.len() <= 8192);
            }
            if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").is_err() { return; }
            for chunk in MUSIC.chunks(CHUNK) {
                if permits.recv_timeout(WAIT).is_err() {
                    return;
                }
                if write!(socket, "{:x}\r\n", chunk.len()).is_err()
                    || socket.write_all(chunk).is_err()
                    || socket.write_all(b"\r\n").is_err()
                {
                    return;
                }
            }
            let _ = socket.write_all(b"0\r\n\r\n");
        });
        Self {
            url,
            credit: Some(credit),
            worker: Some(worker),
        }
    }
    fn next(&self) {
        self.credit.as_ref().unwrap().send(()).unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.credit.take();
        self.worker.take().unwrap().join().unwrap();
    }
}

struct Stream {
    server: Server,
    state: Arc<StreamingState>,
    rx: Option<tokio::sync::mpsc::Receiver<StreamMsg>>,
    ready: bool,
    bits: Vec<u32>,
    rate: u32,
    publications: usize,
}
impl Stream {
    fn new(host: &migo_io::scheduler::IoScheduler, rate: u32) -> Self {
        let server = Server::new();
        let state = StreamingState::new();
        let client: HttpClient = migo_services::network::client::create_policy_http_client(
            "migo-stream-contention-test",
            false,
            &shared::network_policy::NetworkPolicy::default(),
            "audio",
            host.pools().clone(),
        )
        .unwrap();
        let rx = crate::streaming::start_streaming_download(
            host.audio_cpu(),
            client,
            server.url.clone(),
            state.clone(),
            rate,
        );
        Self {
            server,
            state,
            rx: Some(rx),
            ready: false,
            bits: Vec::new(),
            rate,
            publications: 0,
        }
    }
    fn next_message(&mut self) -> Option<StreamMsg> {
        crate::streaming::get_stream_runtime().block_on(async {
            tokio::time::timeout(WAIT, self.rx.as_mut().unwrap().recv())
                .await
                .unwrap()
        })
    }
    fn accept(&mut self, message: StreamMsg) -> bool {
        assert_eq!(
            message.revision(),
            crate::streaming::StreamRevision::INITIAL
        );
        match message {
            StreamMsg::Ready {
                sample_rate,
                channels,
                ..
            } => {
                assert!(!self.ready, "duplicate Ready");
                assert_eq!((sample_rate, channels), (self.rate, 2));
                self.ready = true;
            }
            StreamMsg::Samples { pcm, .. } => {
                assert!(self.ready && !pcm.is_empty());
                assert_eq!(pcm.len() % 2, 0);
                self.bits.extend(pcm.iter().map(|value| value.to_bits()));
                self.publications += 1;
                return true;
            }
            StreamMsg::Error { message, .. } => panic!("real streaming error: {message}"),
            StreamMsg::Done { .. } => panic!("unexpected early Done"),
        }
        false
    }
    fn sample(&mut self) {
        loop {
            let message = self.next_message().expect("source closed before PCM");
            if self.accept(message) {
                return;
            }
        }
    }
    fn finish(&mut self) {
        loop {
            let message = self.next_message().expect("source closed without Done");
            if matches!(message, StreamMsg::Done { .. }) {
                assert_eq!(
                    message.revision(),
                    crate::streaming::StreamRevision::INITIAL
                );
                break;
            }
            self.accept(message);
        }
        assert!(self.next_message().is_none(), "message after Done");
        assert!(self.ready && self.publications >= 16);
        assert!(
            self.bits.iter().any(|&value| value & 0x7fff_ffff != 0),
            "silent reference/workload"
        );
        assert_eq!(
            self.state.bytes_downloaded.load(Ordering::Acquire),
            MUSIC.len() as u64
        );
        assert!(!self.state.is_cancelled());
        until(|| Arc::strong_count(&self.state) == 1);
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.state.cancel();
        self.rx.take();
    }
}

struct Background {
    pool: DecodePool,
    results: DecodeQueueReceiver<DecodeResult>,
    replies: Vec<tokio::sync::oneshot::Receiver<EngineResult<DecodedPcm>>>,
}
impl Background {
    fn new(host: &migo_io::scheduler::IoScheduler) -> Self {
        let (tx, results) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
        Self {
            pool: DecodePool::new(2, tx, ThreadWakeup::new(), host.audio_cpu()),
            results,
            replies: Vec::new(),
        }
    }
    fn submit(&mut self, data: &Arc<Vec<u8>>) {
        let (job, reply) = audio_buffer_decode_job(1, data.clone());
        self.replies.push(reply);
        self.pool.submit(job);
    }
    fn complete(&mut self, expected: &DecodedAudio) -> usize {
        self.complete_with_timeout(expected, WAIT)
    }
    fn complete_with_timeout(&mut self, expected: &DecodedAudio, timeout: Duration) -> usize {
        let count = self.replies.len();
        for _ in 0..count {
            let DecodeResult::AudioBuffer { result, .. } =
                self.results.recv_timeout(timeout).unwrap()
            else {
                panic!("wrong whole result");
            };
            let audio = result.unwrap();
            assert_eq!(
                (audio.sample_rate, audio.channels),
                (expected.sample_rate, expected.channels)
            );
            assert_eq!(audio.samples.len(), expected.samples.len());
            assert!(
                audio
                    .samples
                    .iter()
                    .zip(&expected.samples)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "whole PCM changed"
            );
        }
        self.replies.clear();
        count
    }
}

#[test]
fn real_http_refills_overlap_whole_jobs_and_preserve_every_sample() {
    let rate: u32 = std::env::var("MIGO_REAL_RATE")
        .unwrap_or_else(|_| "48000".into())
        .parse()
        .unwrap();
    let host_count: usize = std::env::var("MIGO_REAL_HOSTS")
        .unwrap_or_else(|_| "2".into())
        .parse()
        .unwrap();
    let stream_count: usize = std::env::var("MIGO_REAL_STREAMS")
        .unwrap_or_else(|_| "2".into())
        .parse()
        .unwrap();
    assert!([1, 2].contains(&host_count) && [1, 2].contains(&stream_count));
    let hosts: Vec<_> = (0..host_count)
        .map(|id| migo_io::scheduler::IoScheduler::new(id as i32))
        .collect();
    let mut reference = Stream::new(&hosts[0], rate);
    for _ in MUSIC.chunks(CHUNK) {
        reference.server.next();
    }
    reference.finish();
    let golden = &reference.bits;
    let mut streams: Vec<_> = (0..stream_count)
        .map(|id| Stream::new(&hosts[id % host_count], rate))
        .collect();
    for stream in &mut streams {
        stream.server.next();
        stream.sample();
    }
    println!("MIGO_REAL_STREAM_INITIAL_PCM");
    let expected_workers = thread::available_parallelism().unwrap().get().clamp(2, 6);
    until(|| io_workers() == expected_workers);
    let workers = io_workers();
    assert_eq!(workers, expected_workers, "actual IO worker topology");
    let jobs = (workers - 1).clamp(1, 2);
    let cpu = hosts[0].audio_cpu();
    until(|| !cpu.refill_pending());
    let data = Arc::new(wav(48000, 2_000_000));
    let expected = crate::decoder::decode(&data).unwrap();
    let mut owners: Vec<_> = hosts.iter().map(Background::new).collect();
    let seen = Arc::new(Mutex::new(Vec::<Weak<AtomicBool>>::new()));
    let (entered, starts) = mpsc::channel();
    let (release, permits) = mpsc::channel();
    let permits = Arc::new(Mutex::new(permits));
    for owner in &mut owners {
        let seen = seen.clone();
        let entered = entered.clone();
        let permits = permits.clone();
        owner.pool.step_hook = Some(Arc::new(move |cancel| {
            let first = {
                let mut seen = seen.lock().unwrap();
                let key = Arc::downgrade(cancel);
                if seen.iter().any(|old| old.ptr_eq(&key)) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            };
            if first {
                entered.send(()).unwrap();
                let _ = permits.lock().unwrap().recv_timeout(WAIT);
            }
        }));
    }
    let mut waits = Vec::new();
    let mut completed = 0;
    let mut overlaps = 0;
    for _ in 1..MUSIC.len().div_ceil(CHUNK) {
        seen.lock().unwrap().clear();
        for job in 0..jobs {
            owners[job % host_count].submit(&data);
        }
        for _ in 0..jobs {
            starts.recv_timeout(WAIT).unwrap();
        }
        let start = Instant::now();
        for stream in &streams {
            stream.server.next();
        }
        // One network chunk may now produce several bounded PCM batches.
        // Free publication slots while the whole jobs hold every CPU worker;
        // the queued foreground request is the actual overlap witness.
        until(|| {
            for stream in &mut streams {
                while let Ok(message) = stream.rx.as_mut().unwrap().try_recv() {
                    stream.accept(message);
                }
            }
            cpu.refill_pending()
        });
        assert_eq!(
            process_decode_in_flight_budget()
                .bytes
                .load(Ordering::Acquire),
            jobs * DECODE_IN_FLIGHT_RESERVATION_BYTES,
            "shared whole memory reservation"
        );
        overlaps += 1;
        for _ in 0..jobs {
            release.send(()).unwrap();
        }
        for stream in &mut streams {
            stream.sample();
            waits.push(start.elapsed().as_nanos());
        }
        for owner in &mut owners {
            completed += owner.complete(&expected);
        }
        assert_eq!(
            process_decode_in_flight_budget()
                .bytes
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(Arc::strong_count(&data), 1);
    }
    for stream in &mut streams {
        stream.finish();
        assert_eq!(&stream.bits, golden, "contended stream PCM differs");
    }
    until(|| !cpu.refill_pending());
    assert_eq!(completed, jobs * (MUSIC.len().div_ceil(CHUNK) - 1));
    assert!(overlaps >= 16);
    waits.sort_unstable();
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    golden.hash(&mut fingerprint);
    let fingerprint = fingerprint.finish();
    println!(
        "MIGO_REAL_STREAM {{\"rate\":{rate},\"hosts\":{host_count},\"streams\":{stream_count},\"io_workers\":{workers},\"background_jobs\":{completed},\"overlap_rounds\":{overlaps},\"stream_samples\":{},\"observed_p99_ns\":{},\"pcm_fingerprint\":{fingerprint},\"complete_pcm_bits_checked\":true}}",
        golden.len(),
        waits[(waits.len() - 1) * 99 / 100]
    );
}

#[test]
fn full_real_pcm_channel_stops_cpu_and_cancel_retires_the_producer() {
    for drop_consumer in [false, true] {
        let host = migo_io::scheduler::IoScheduler::new(31);
        let peer = migo_io::scheduler::IoScheduler::new(32);
        let cpu = host.audio_cpu();
        let generation_before_body = cpu.refill_generation();
        let mut stream = Stream::new(&host, 44100);
        for _ in MUSIC.chunks(CHUNK) {
            stream.server.next();
        }
        until(|| {
            stream.state.bytes_downloaded.load(Ordering::Acquire) > 0
                && cpu.refill_generation() != generation_before_body
        });
        println!("MIGO_REAL_HTTP_BODY");
        until(|| {
            stream.state.publication_waiting.load(Ordering::Acquire)
                && stream.rx.as_ref().unwrap().len() == 4
        });
        let bytes = stream.state.bytes_downloaded.load(Ordering::Acquire);
        let generation = cpu.refill_generation();
        let data = Arc::new(wav(48000, 2_000_000));
        let expected = crate::decoder::decode(&data).unwrap();
        let mut background = Background::new(&peer);
        background.submit(&data);
        assert_eq!(background.complete(&expected), 1);
        assert_eq!(stream.rx.as_ref().unwrap().len(), 4);
        assert_eq!(
            process_decode_in_flight_budget()
                .bytes
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(
            cpu.refill_generation(),
            generation,
            "full channel kept submitting CPU work"
        );
        assert_eq!(
            stream.state.bytes_downloaded.load(Ordering::Acquire),
            bytes,
            "full channel kept reading source data"
        );
        if drop_consumer {
            stream.rx.take();
        } else {
            stream.state.cancel();
        }
        until(|| stream.state.is_cancelled() && Arc::strong_count(&stream.state) == 1);
        if let Some(rx) = &stream.rx {
            assert!(rx.is_closed());
        }
        assert!(!cpu.refill_pending());
        let value = crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 23))
            .unwrap();
        assert_eq!(value, 23, "other host stopped with the stream");
    }
}
