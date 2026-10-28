//! Library-native container identification followed by the existing transport.
//! The adapter supplies bounded positional IO; scoring, metadata and container
//! markers stay in Symphonia. No container reader or codec is built while probing.
use crate::{
    local_source::{Backing, FileReader},
    streaming::{StreamMsg, StreamingState},
    streaming_transport,
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use symphonia::core::{
    common::Limit,
    formats::{probe::Hint, well_known::*},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
};
use tokio::{sync::mpsc, time::Instant};

const PROBE_MEMORY_BYTES: u64 = 128 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mp3,
    Wave,
    Adts,
    Ogg,
    M4a,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub max_read_bytes: u64,
    pub max_reads: u32,
    pub max_seek_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_read_bytes: 2 * 1024 * 1024,
            max_reads: 128,
            max_seek_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Identified {
    pub format: Format,
    pub bytes: u64,
    pub reads: u32,
    pub seek_bytes: u64,
}
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
fn check(
    domain: &migo_io::domain::IoDomain,
    active: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    if domain.is_closed() || !active.load(Ordering::Acquire) {
        return Err(io::Error::other("format probe cancelled or closed"));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "format probe deadline exceeded",
        ));
    }
    Ok(())
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
struct Input {
    source: Arc<Backing>,
    domain: Arc<migo_io::domain::IoDomain>,
    active: Arc<AtomicBool>,
    deadline: Instant,
    limits: Limits,
    position: u64,
    bytes: u64,
    reads: u32,
    seek_bytes: u64,
}
impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        check(&self.domain, &self.active, self.deadline)?;
        let len = (self.source.len() - self.position).min(out.len() as u64) as usize;
        if len == 0 {
            return Ok(0);
        }
        let bytes = self
            .bytes
            .checked_add(len as u64)
            .filter(|n| *n <= self.limits.max_read_bytes)
            .ok_or_else(|| io::Error::other("format probe byte limit"))?;
        let reads = self
            .reads
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_reads)
            .ok_or_else(|| io::Error::other("format probe request limit"))?;
        self.source.read_into_at(self.position, &mut out[..len])?;
        self.position += len as u64;
        self.bytes = bytes;
        self.reads = reads;
        check(&self.domain, &self.active, self.deadline)?;
        Ok(len)
    }
}
impl Seek for Input {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        check(&self.domain, &self.active, self.deadline)?;
        let target = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.source.len()) + i128::from(n),
        };
        let target = u64::try_from(target)
            .ok()
            .filter(|n| *n <= self.source.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "format probe seek extent")
            })?;
        self.seek_bytes = self
            .seek_bytes
            .checked_add(target.abs_diff(self.position))
            .filter(|n| *n <= self.limits.max_seek_bytes)
            .ok_or_else(|| io::Error::other("format probe seek budget"))?;
        self.position = target;
        Ok(target)
    }
}
impl MediaSource for Input {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.source.len())
    }
}
impl MediaSource for &mut Input {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.source.len())
    }
}
fn identify(mut input: Input) -> EngineResult<Identified> {
    let stream = MediaSourceStream::new(Box::new(&mut input), Default::default());
    let metadata = MetadataOptions::default()
        .limit_total_bytes(Limit::Maximum(0))
        .limit_tag_bytes(Limit::Maximum(64 * 1024))
        .limit_visual_bytes(Limit::Maximum(0))
        .limit_frame_bytes(Limit::Maximum(64 * 1024))
        .limit_nesting_depth(Limit::Maximum(2));
    let identified = symphonia::default::get_probe()
        .probe_format(&Hint::new(), stream, metadata)
        .map_err(error)?;
    let id = identified.info().format;
    let format = match id {
        FORMAT_ID_MP3 => Format::Mp3,
        FORMAT_ID_WAVE => Format::Wave,
        FORMAT_ID_ADTS => Format::Adts,
        FORMAT_ID_OGG => Format::Ogg,
        FORMAT_ID_ISOMP4 => Format::M4a,
        _ => return Err(error("unsupported audio container")),
    };
    drop(identified);
    check(&input.domain, &input.active, input.deadline).map_err(error)?;
    Ok(Identified {
        format,
        bytes: input.bytes,
        reads: input.reads,
        seek_bytes: input.seek_bytes,
    })
}

/// A failed/cancelled probe permanently retires this reader. It uses positional
/// reads, so successful identification leaves sequential playback at byte zero.
pub async fn probe(
    reader: &mut FileReader,
    deadline: Instant,
    limits: Limits,
) -> EngineResult<Identified> {
    if reader.failed {
        return Err(error("format source previously failed"));
    }
    reader.failed = true;
    if limits.max_read_bytes == 0 || limits.max_reads == 0 {
        return Err(error("invalid format probe limits"));
    }
    let active = Arc::new(AtomicBool::new(true));
    let _guard = Active(active.clone());
    check(&reader.domain, &active, deadline).map_err(error)?;
    let lease = reader
        .pools
        .reserve_bytes(PROBE_MEMORY_BYTES)
        .map_err(error)?;
    let source = Input {
        source: reader.file.clone(),
        domain: reader.domain.clone(),
        active,
        deadline,
        limits,
        position: 0,
        bytes: 0,
        reads: 0,
        seek_bytes: 0,
    };
    let job = reader
        .pools
        .submit_async(
            reader.file.kind(),
            migo_io::task::PriorityClass::ForegroundAsync,
            move || {
                let _lease = lease;
                identify(source)
            },
        )
        .map_err(error)?;
    let result = tokio::time::timeout_at(deadline, job)
        .await
        .map_err(|_| error("format probe timeout"))?
        .map_err(error)?
        .map_err(|_| error("format probe worker panicked"))??;
    check(&reader.domain, &_guard.0, deadline).map_err(error)?;
    reader.failed = false;
    Ok(result)
}

/// Select the typed codec once, then use the common CPU/DSP/publication loop.
pub async fn run(
    owner: &migo_io::scheduler::IoScheduler,
    mut reader: FileReader,
    deadline: Instant,
    target: u32,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<()> {
    let format = probe(&mut reader, deadline, Limits::default())
        .await?
        .format;
    let length = reader.length();
    use crate::{push_decoder, resampled_streaming::Batched};
    match format {
        Format::Mp3 => {
            streaming_transport::run_source(
                owner.audio_cpu(),
                reader,
                length,
                tx,
                state,
                move |_| Batched::<16, push_decoder::Mp3>::new(target),
            )
            .await
        }
        Format::Wave => {
            streaming_transport::run_source(
                owner.audio_cpu(),
                reader,
                length,
                tx,
                state,
                move |_| Batched::<16, push_decoder::Wave>::new(target),
            )
            .await
        }
        Format::Adts => {
            streaming_transport::run_source(
                owner.audio_cpu(),
                reader,
                length,
                tx,
                state,
                move |_| Batched::<16, push_decoder::Adts>::new(target),
            )
            .await
        }
        Format::Ogg => {
            streaming_transport::run_source(
                owner.audio_cpu(),
                reader,
                length,
                tx,
                state,
                move |_| Batched::<16, push_decoder::Vorbis>::new(target),
            )
            .await
        }
        Format::M4a => {
            streaming_transport::run_source(
                owner.audio_cpu(),
                crate::isomp4_file::IndexedFile::from_reader(reader),
                length,
                tx,
                state,
                move |_| Batched::<16, push_decoder::M4a<true>>::with_object_len(target, length),
            )
            .await
        }
    }
}
