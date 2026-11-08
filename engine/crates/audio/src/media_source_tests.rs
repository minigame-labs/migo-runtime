use super::*;
use shared::audio_file::AudioFileRead;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use symphonia_core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadBytes};

/// An in-memory sealed object that counts its blocking reads.
pub(crate) struct MemoryFile {
    pub(crate) bytes: Vec<u8>,
    pub(crate) reads: AtomicUsize,
    closed: tokio::sync::Notify,
}

impl MemoryFile {
    pub(crate) fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes,
            reads: AtomicUsize::new(0),
            closed: tokio::sync::Notify::new(),
        })
    }
}

impl AudioFile for MemoryFile {
    fn length(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn prefix(&self) -> &[u8] {
        &self.bytes[..self.bytes.len().min(8)]
    }
    fn queued_bytes(&self) -> usize {
        0
    }
    fn is_closed(&self) -> bool {
        false
    }
    fn read_blocking(&self, offset: u64, output: &mut [u8]) -> EngineResult<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let start = offset as usize;
        let end = start
            .checked_add(output.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| EngineError::new(ErrorCode::IoError))?;
        output.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }
    fn closed_signal(&self) -> &tokio::sync::Notify {
        &self.closed
    }
    fn read_at(
        self: Arc<Self>,
        _offset: u64,
        _length: usize,
        _cancelled: Arc<dyn AudioFileCancel>,
        _deadline: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        Err(EngineError::new(ErrorCode::IoError))
    }
    fn read_control_at(
        self: Arc<Self>,
        _offset: u64,
        _length: usize,
        _cancelled: Arc<dyn AudioFileCancel>,
        _deadline: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        Err(EngineError::new(ErrorCode::IoError))
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

fn stream(file: Arc<MemoryFile>, cancel: Arc<AtomicBool>) -> MediaSourceStream<'static> {
    MediaSourceStream::new(
        Box::new(AudioFileSource::new(file, cancel)),
        MediaSourceStreamOptions::default(),
    )
}

#[test]
fn a_reader_sees_every_byte_and_the_exact_end() {
    let bytes = pattern(200_003);
    let file = MemoryFile::new(bytes.clone());
    let mut mss = stream(file.clone(), Arc::new(AtomicBool::new(false)));
    assert!(mss.is_seekable());
    assert_eq!(mss.byte_len(), Some(bytes.len() as u64));
    let mut out = Vec::new();
    mss.read_to_end(&mut out).unwrap();
    assert_eq!(out, bytes);
    // The reader's buffer turns byte reads into large blocks.
    assert!(file.reads.load(Ordering::Relaxed) < 20);
}

#[test]
fn seeks_land_on_the_requested_byte_from_every_origin() {
    let bytes = pattern(100_000);
    let file = MemoryFile::new(bytes.clone());
    let mut mss = stream(file, Arc::new(AtomicBool::new(false)));
    mss.seek(SeekFrom::Start(77_777)).unwrap();
    assert_eq!(mss.read_byte().unwrap(), bytes[77_777]);
    mss.seek(SeekFrom::End(-1)).unwrap();
    assert_eq!(mss.read_byte().unwrap(), bytes[99_999]);
    mss.seek(SeekFrom::Start(10)).unwrap();
    mss.seek(SeekFrom::Current(5)).unwrap();
    assert_eq!(mss.read_byte().unwrap(), bytes[15]);
    // Past the end reads as end of stream, before the start is refused.
    mss.seek(SeekFrom::Start(200_000)).unwrap();
    assert_eq!(mss.read(&mut [0; 4]).unwrap(), 0);
    let mut raw = AudioFileSource::new(MemoryFile::new(bytes), Arc::new(AtomicBool::new(false)));
    assert!(raw.seek(SeekFrom::Current(-1)).is_err());
}

#[test]
fn cancellation_stops_the_next_read_without_touching_the_file() {
    let file = MemoryFile::new(pattern(1000));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut source = AudioFileSource::new(file.clone(), cancel.clone());
    let mut buf = [0u8; 100];
    assert_eq!(source.read(&mut buf).unwrap(), 100);
    cancel.store(true, Ordering::Release);
    let reads = file.reads.load(Ordering::Relaxed);
    assert_eq!(
        source.read(&mut buf).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(file.reads.load(Ordering::Relaxed), reads);
}
