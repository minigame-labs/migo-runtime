//! Opening a stream's source and choosing its decoder, on the producer thread.
use crate::media_source::AudioFileSource;
use crate::media_source_http::HttpSource;
use crate::pcm_source::PcmSource;
use crate::stream_producer::Open;
use crate::streaming::{HttpClient, StreamingState};
use shared::audio_file::{AudioFile, AudioFileCancel, AudioHttpSpool, AudioSparseSpool};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// A file load the consumer answers once the stream is ready or failed; the
/// producer records a failure's typed cause in `error` first.
pub(crate) struct Load {
    pub reply: shared::protocol::audio_cmd::AudioResp<()>,
    pub error: crate::stream_producer::ErrorCell,
}

/// An HTTP body kept in memory when the session has no disk spool, or its
/// disk quota is in use by other streams.
const MEMORY_SPOOL_BYTES: u64 = 32 * 1024 * 1024;

/// A control queued while the producer waits on network bytes for its
/// current traversal: the wait is abandoned and the reader repositioned, as a
/// browser does on seek.
struct Preempted(Arc<StreamingState>);
impl AudioFileCancel for Preempted {
    fn is_cancelled(&self) -> bool {
        self.0.read_preempted()
    }
}

/// A sealed local file, package entry or spooled body.
pub(crate) fn file(file: Arc<dyn AudioFile>) -> Open {
    Box::new(move |state| {
        if file.is_closed() {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "audio file closed",
            ));
        }
        let prefix = file.prefix().to_vec();
        let source = AudioFileSource::new(file.clone(), state.clone());
        Ok(Box::new(Revalidated {
            inner: crate::pcm_source::open(Box::new(source), &prefix)?,
            file,
        }))
    })
}

/// A file capability can be revoked while its bytes still sit in a reader's
/// buffer (a short looping clip never reads again): check it at every read and
/// seek; a new traversal always reads before it publishes.
struct Revalidated {
    inner: Box<dyn PcmSource>,
    file: Arc<dyn AudioFile>,
}
impl Revalidated {
    fn check(&self) -> EngineResult<()> {
        if self.file.is_closed() {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "audio file closed",
            ));
        }
        Ok(())
    }
}
impl PcmSource for Revalidated {
    fn rate(&self) -> u32 {
        self.inner.rate()
    }
    fn channels(&self) -> u32 {
        self.inner.channels()
    }
    fn frames(&self) -> Option<u64> {
        self.inner.frames()
    }
    fn read(&mut self, out: &mut Vec<f32>) -> EngineResult<bool> {
        self.check()?;
        self.inner.read(out)
    }
    fn seek(&mut self, frame: u64) -> EngineResult<()> {
        self.check()?;
        self.inner.seek(frame)
    }
}

/// An HTTP URL, fetched into a sparse spool as the decoder reads it.
pub(crate) fn http(
    client: HttpClient,
    url: String,
    spool: Option<Arc<dyn AudioHttpSpool>>,
) -> Open {
    Box::new(move |state| {
        let runtime = crate::streaming::get_stream_runtime().handle().clone();
        let response = runtime.block_on(open_response(&client, &url, state))?;
        let Some(response) = response else {
            return Err(EngineError::from_detail(
                ErrorCode::Cancelled,
                "audio stream cancelled",
            ));
        };
        if let Some(total) = response.content_length() {
            state.bytes_total.store(total, Ordering::Relaxed);
        }
        let progress_state = state.clone();
        let progress: Box<dyn Fn(u64) + Send + Sync> = Box::new(move |bytes| {
            progress_state
                .bytes_downloaded
                .store(bytes, Ordering::Relaxed);
        });
        let spool = Fallback(spool);
        let mut source = HttpSource::open(
            &runtime,
            client,
            response,
            &spool,
            state.clone(),
            Some(progress),
        )?;
        source.interrupt_with(Arc::new(Preempted(state.clone())));
        let mut prefix = [0u8; crate::pcm_source::SNIFF_BYTES];
        let mut have = 0;
        while have < prefix.len() {
            match source.read(&mut prefix[have..]) {
                Ok(0) => break,
                Ok(n) => have += n,
                Err(error) => {
                    return Err(EngineError::from_detail(
                        ErrorCode::IoError,
                        error.to_string(),
                    ));
                }
            }
        }
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| EngineError::from_detail(ErrorCode::IoError, error.to_string()))?;
        crate::pcm_source::open(Box::new(source), &prefix[..have])
    })
}

/// The session's disk spool, else memory (no spool, or its quota is taken).
struct Fallback(Option<Arc<dyn AudioHttpSpool>>);
impl AudioHttpSpool for Fallback {
    fn prepare(
        self: Arc<Self>,
        _body: Box<dyn shared::audio_file::AudioSpoolBody>,
        _cancel: Arc<dyn AudioFileCancel>,
        _deadline: std::time::Instant,
    ) -> shared::audio_file::AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
        Box::pin(async { Err(EngineError::new(ErrorCode::Unsupported)) })
    }
    fn sparse(&self, len: Option<u64>) -> EngineResult<Box<dyn AudioSparseSpool>> {
        match &self.0 {
            Some(disk) => disk.sparse(len).or_else(|_| memory_sparse(len)),
            None => memory_sparse(len),
        }
    }
}

/// A bounded in-memory sparse store (sessions without a disk spool, tests).
pub(crate) fn memory_sparse(len: Option<u64>) -> EngineResult<Box<dyn AudioSparseSpool>> {
    MemorySpool.sparse(len)
}

/// Send the request; `None` when the stream was cancelled meanwhile.
async fn open_response(
    client: &HttpClient,
    url: &str,
    state: &StreamingState,
) -> EngineResult<Option<migo_services::network::http::Response>> {
    use crate::streaming::{NetworkWait, RESPONSE_HEADER_TIMEOUT, wait_for_network};
    let parsed = migo_services::network::url::Url::parse(url).map_err(|error| {
        EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("invalid audio URL: {error}"),
        )
    })?;
    // Range reads need the stored representation; audio is compressed anyway.
    let request = client
        .get(migo_services::network::gate::GateKind::AudioStream, parsed)
        .header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        );
    let response = match wait_for_network(state, RESPONSE_HEADER_TIMEOUT, request.send()).await {
        NetworkWait::Cancelled => return Ok(None),
        NetworkWait::TimedOut => {
            return Err(EngineError::from_detail(
                ErrorCode::Timeout,
                "audio response header timeout",
            ));
        }
        NetworkWait::Ready(Err(error)) => {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                format!("HTTP request failed: {error}"),
            ));
        }
        NetworkWait::Ready(Ok(response)) => response,
    };
    if !response.status().is_success() {
        return Err(EngineError::from_detail(
            ErrorCode::IoError,
            format!("HTTP error: {}", response.status()),
        ));
    }
    Ok(Some(response))
}

/// Sessions without a disk spool keep the body in memory, bounded.
struct MemorySpool;
impl AudioHttpSpool for MemorySpool {
    fn prepare(
        self: Arc<Self>,
        _body: Box<dyn shared::audio_file::AudioSpoolBody>,
        _cancel: Arc<dyn AudioFileCancel>,
        _deadline: std::time::Instant,
    ) -> shared::audio_file::AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
        Box::pin(async { Err(EngineError::new(ErrorCode::Unsupported)) })
    }
    fn sparse(&self, len: Option<u64>) -> EngineResult<Box<dyn AudioSparseSpool>> {
        let cap = len.unwrap_or(MEMORY_SPOOL_BYTES);
        if cap > MEMORY_SPOOL_BYTES {
            return Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "audio body exceeds the in-memory spool",
            ));
        }
        Ok(Box::new(MemorySparse {
            data: std::sync::Mutex::new(Vec::new()),
            cap,
        }))
    }
}

/// Grows as bytes arrive (an unknown-length body only ever appends).
struct MemorySparse {
    data: std::sync::Mutex<Vec<u8>>,
    cap: u64,
}
impl AudioSparseSpool for MemorySparse {
    fn size(&self) -> u64 {
        self.cap
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> EngineResult<()> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= self.cap)
            .ok_or_else(|| EngineError::new(ErrorCode::InputSaturated))?;
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        let have = data.len();
        if have < end as usize {
            data.try_reserve(end as usize - have)
                .map_err(|_| EngineError::new(ErrorCode::OutOfMemory))?;
            data.resize(end as usize, 0);
        }
        data[offset as usize..end as usize].copy_from_slice(bytes);
        Ok(())
    }
    fn read_at(&self, offset: u64, output: &mut [u8]) -> EngineResult<()> {
        let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        let start = offset as usize;
        output.copy_from_slice(
            data.get(start..start + output.len())
                .ok_or_else(|| EngineError::new(ErrorCode::IoError))?,
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "streaming_file_tests.rs"]
mod file_tests;
#[cfg(test)]
#[path = "streaming_http_tests.rs"]
pub(crate) mod http_tests;
