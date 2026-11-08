use super::*;
use crate::streaming_http::tests::{Mode, server, server_with_initial_prefix};
use shared::audio_file::{
    AudioFile, AudioFileCancel, AudioHttpSpool, AudioSpoolBody, AudioSpoolFuture,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[test]
fn extents_merge_touching_ranges_and_report_gaps() {
    let mut e = Extents::default();
    e.insert(10, 20);
    e.insert(30, 40);
    e.insert(20, 25);
    assert_eq!(e, Extents(vec![(10, 25), (30, 40)]));
    e.insert(0, 5);
    e.insert(24, 31);
    assert_eq!(e, Extents(vec![(0, 5), (10, 40)]));
    assert_eq!(e.end_from(12), Some(40));
    assert_eq!(e.end_from(5), None);
    assert_eq!(e.gap_from(0, 50), Some(5));
    assert_eq!(e.gap_from(12, 50), Some(40));
    assert_eq!(e.gap_from(12, 40), None);
    assert!(!e.covers(40));
    e.insert(5, 10);
    assert!(e.covers(40) && !e.covers(41));
    e.insert(7, 7);
    assert_eq!(e, Extents(vec![(0, 40)]));
}

struct MemorySparse(std::sync::Mutex<Vec<u8>>);
impl AudioSparseSpool for MemorySparse {
    fn len(&self) -> u64 {
        self.0.lock().unwrap().len() as u64
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> EngineResult<()> {
        let mut data = self.0.lock().unwrap();
        let (start, end) = (offset as usize, offset as usize + bytes.len());
        data.get_mut(start..end)
            .ok_or_else(|| EngineError::new(ErrorCode::IoError))?
            .copy_from_slice(bytes);
        Ok(())
    }
    fn read_at(&self, offset: u64, output: &mut [u8]) -> EngineResult<()> {
        let data = self.0.lock().unwrap();
        let (start, end) = (offset as usize, offset as usize + output.len());
        output.copy_from_slice(
            data.get(start..end)
                .ok_or_else(|| EngineError::new(ErrorCode::IoError))?,
        );
        Ok(())
    }
}

struct MemorySpool;
impl AudioHttpSpool for MemorySpool {
    fn prepare(
        self: Arc<Self>,
        _body: Box<dyn AudioSpoolBody>,
        _cancel: Arc<dyn AudioFileCancel>,
        _deadline: Instant,
    ) -> AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
        Box::pin(async { Err(EngineError::new(ErrorCode::Unsupported)) })
    }
    fn sparse(&self, len: Option<u64>) -> EngineResult<Box<dyn AudioSparseSpool>> {
        Ok(Box::new(MemorySparse(std::sync::Mutex::new(
            vec![0; len.unwrap_or(1 << 22) as usize],
        ))))
    }
}

fn media(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 253) as u8).collect()
}

/// A tokio runtime for the server and the fetcher; reads run on this thread.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

struct Opened {
    source: HttpSource,
    server: crate::streaming_http::tests::Server,
    cancel: Arc<AtomicBool>,
    _host: migo_io::scheduler::IoScheduler,
}

fn open(
    rt: &tokio::runtime::Runtime,
    server: crate::streaming_http::tests::Server,
    path: &str,
) -> Opened {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let client = server.client(host.pools().clone());
    let sequential = path == "/sequential";
    let response = rt.block_on(async {
        let mut request = client.get(
            GateKind::AudioStream,
            migo_services::network::url::Url::parse(&format!("http://media.invalid{path}"))
                .unwrap(),
        );
        if !sequential {
            request = request.header(
                "accept-encoding".parse().unwrap(),
                "identity".parse().unwrap(),
            );
        }
        request.send().await.unwrap()
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let source =
        HttpSource::open(rt.handle(), client, response, &MemorySpool, cancel.clone()).unwrap();
    Opened {
        source,
        server,
        cancel,
        _host: host,
    }
}

fn read_all(source: &mut HttpSource) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    source.read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn a_ranged_object_reads_whole_and_reports_its_length_at_once() {
    let rt = runtime();
    let data = media(300_001);
    let server = rt.block_on(server(data.clone(), Mode::Normal, false, false));
    let mut o = open(&rt, server, "/clip");
    assert_eq!(o.source.byte_len(), Some(data.len() as u64));
    assert_eq!(read_all(&mut o.source).unwrap(), data);
    assert_eq!(
        o.server.range_requests(),
        0,
        "a sequential read needs no range"
    );
}

#[test]
fn the_tail_arrives_by_range_while_the_initial_body_is_still_stalled() {
    let rt = runtime();
    let data = media(2_000_000);
    // The initial body sends 4 KiB and then never another byte.
    let server = rt.block_on(server_with_initial_prefix(
        data.clone(),
        Mode::Normal,
        Some(4096),
        false,
    ));
    let mut o = open(&rt, server, "/clip");
    let started = Instant::now();
    o.source.seek(SeekFrom::End(-10_000)).unwrap();
    let mut tail = vec![0; 10_000];
    o.source.read_exact(&mut tail).unwrap();
    assert_eq!(tail, data[data.len() - 10_000..]);
    o.source.seek(SeekFrom::Start(100)).unwrap();
    let mut head = [0; 64];
    o.source.read_exact(&mut head).unwrap();
    assert_eq!(head, data[100..164]);
    // The middle comes from a range too: the stalled body never delivers it.
    o.source.seek(SeekFrom::Start(1_000_000)).unwrap();
    let mut middle = vec![0; 50_000];
    o.source.read_exact(&mut middle).unwrap();
    assert_eq!(middle, data[1_000_000..1_050_000]);
    assert!(o.server.range_requests() >= 1);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn without_a_strong_validator_the_object_reads_on_and_hides_its_length() {
    for mode in [Mode::NoTag, Mode::Weak] {
        let rt = runtime();
        let data = media(200_000);
        let server = rt.block_on(server(data.clone(), mode, false, false));
        let mut o = open(&rt, server, "/clip");
        assert_eq!(o.source.byte_len(), None, "a read-on object hides its tail");
        // Forward seeks wait for the one response instead of asking for a range.
        o.source.seek(SeekFrom::Start(150_000)).unwrap();
        let mut late = [0; 100];
        o.source.read_exact(&mut late).unwrap();
        assert_eq!(late, data[150_000..150_100]);
        o.source.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(read_all(&mut o.source).unwrap(), data);
        assert_eq!(
            o.source.byte_len(),
            Some(data.len() as u64),
            "complete objects report length"
        );
        assert_eq!(o.server.range_requests(), 0);
    }
}

#[test]
fn compressed_and_unknown_length_bodies_learn_their_length_at_the_end() {
    for (mode, path) in [(Mode::Gzip, "/sequential"), (Mode::Chunked, "/clip")] {
        let rt = runtime();
        let data = media(40_000);
        let server = rt.block_on(server(data.clone(), mode, false, false));
        let mut o = open(&rt, server, path);
        assert_eq!(o.source.byte_len(), None);
        assert!(o.source.seek(SeekFrom::End(0)).is_err());
        assert_eq!(read_all(&mut o.source).unwrap(), data);
        assert_eq!(o.source.byte_len(), Some(data.len() as u64));
    }
}

#[test]
fn a_changed_representation_fails_the_read_instead_of_mixing_objects() {
    let rt = runtime();
    let data = media(2_000_000);
    let server = rt.block_on(server_with_initial_prefix(
        data.clone(),
        Mode::Changed,
        Some(4096),
        false,
    ));
    let mut o = open(&rt, server, "/clip");
    o.source.seek(SeekFrom::End(-1000)).unwrap();
    let mut tail = vec![0; 1000];
    assert!(o.source.read_exact(&mut tail).is_err());
}

#[test]
fn closing_releases_a_reader_blocked_on_a_stalled_body() {
    let rt = runtime();
    let data = media(100_000);
    // Without a validator the stalled body is the only way to the bytes.
    let server = rt.block_on(server_with_initial_prefix(
        data,
        Mode::NoTag,
        Some(4096),
        false,
    ));
    let mut o = open(&rt, server, "/clip");
    o.source.seek(SeekFrom::Start(50_000)).unwrap();
    let shared = o.source.shared.clone();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        close(&shared);
    });
    let started = Instant::now();
    let error = o.source.read(&mut [0; 16]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(2));
    closer.join().unwrap();
    // Cancellation alone (no close) is noticed within one wait slice.
    let rt2 = runtime();
    let server = rt2.block_on(server_with_initial_prefix(
        media(100_000),
        Mode::NoTag,
        Some(4096),
        false,
    ));
    let mut o2 = open(&rt2, server, "/clip");
    o2.source.seek(SeekFrom::Start(50_000)).unwrap();
    let cancel = o2.cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel.store(true, Ordering::Release);
    });
    let started = Instant::now();
    assert!(o2.source.read(&mut [0; 16]).is_err());
    assert!(started.elapsed() < WAIT_SLICE * 4);
    drop(o);
}
