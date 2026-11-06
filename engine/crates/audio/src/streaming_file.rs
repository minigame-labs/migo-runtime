//! Sequential sealed-file playback on the original CPU/publication driver.
use crate::off_worker::OffWorker;
use crate::streaming::{self, StreamMsg, StreamingState};
use crate::streaming_transport::{
    self as transport, Decoder, Source, SourceRead, SourceResume, Step,
};
use shared::audio_file::AudioFile;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::{Arc, atomic::AtomicBool};
use tokio::sync::mpsc;

pub(crate) use crate::streaming_source_control::ErrorCell;
pub(crate) struct Load {
    pub reply: shared::protocol::audio_cmd::AudioResp<()>,
    pub error: ErrorCell,
}
struct FileSource {
    file: Arc<dyn AudioFile>,
    // Drop the caller's Arc before scheduling the final owner on the CPU.
    _owner: OffWorker<Arc<dyn AudioFile>>,
    position: u64,
    index: index::Index,
    pending: Option<shared::audio_file::AudioFileChunk>,
}
impl Source<FileDecoder> for FileSource {
    type Request = FileRequest;
    fn pending(decoder: &FileDecoder) -> EngineResult<Self::Request> {
        decoder
            .source_request()
            .ok_or_else(|| invalid("local file has no pending source request"))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        let closed = self.file.closed_signal().notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if self.file.is_closed() {
            return Err(io_error());
        }
        if let Some(chunk) = self.pending.take() {
            return Ok(SourceRead::Data(chunk));
        }
        if self.position == self.file.length() {
            return Ok(SourceRead::End);
        }
        let count = (self.file.length() - self.position)
            .min(migo_io::read_at_file::FILE_CHUNK_BYTES as u64) as usize;
        let deadline = std::time::Instant::now() + streaming::BODY_IDLE_TIMEOUT;
        let read = self
            .file
            .clone()
            .read_at(self.position, count, state.clone(), deadline)?;
        let result = tokio::select! { biased;
            _=&mut closed=>return Err(io_error()),
            result=read=>result,
        };
        let chunk = result.map_err(|_| io_error())?.map_err(|_| io_error())??;
        if chunk.as_ref().len() != count || self.file.is_closed() {
            return Err(io_error());
        }
        self.position += count as u64;
        Ok(SourceRead::Data(chunk))
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<FileDecoder>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<FileDecoder>>> {
        match request {
            FileRequest::M4a(request) => self.resolve(request, decoder, tx, state).await,
            request @ (FileRequest::Wave(_) | FileRequest::Mp3(_) | FileRequest::Vorbis(_)) => {
                self.resolve_request(
                    request,
                    decoder,
                    tx,
                    state,
                    tokio::time::Instant::now() + streaming::RESPONSE_HEADER_TIMEOUT,
                )
                .await
            }
        }
    }
}
#[derive(Clone, Copy)]
enum FileRequest {
    M4a(crate::decoder::aac::m4a::SourceRequest),
    Wave(u64),
    Mp3(u64),
    Vorbis(u64),
}
enum FileDecoder {
    Vorbis(
        Box<crate::streaming_codecs::packet::PacketStream<crate::streaming_codecs::vorbis::Vorbis>>,
        std::sync::Weak<dyn AudioFile>,
    ),
    Wave(
        Box<crate::streaming_codecs::packet::PacketStream<crate::streaming_codecs::wave::Wave>>,
        std::sync::Weak<dyn AudioFile>,
    ),
    Sequential(
        crate::streaming_codecs::AutoDecoder,
        std::sync::Weak<dyn AudioFile>,
    ),
    M4a(
        Box<crate::streaming_codecs::packet::PacketStream<crate::decoder::aac::m4a::M4a>>,
        std::sync::Weak<dyn AudioFile>,
    ),
}
impl FileDecoder {
    fn new(
        rate: u32,
        length: u64,
        m4a: bool,
        retained: bool,
        file: std::sync::Weak<dyn AudioFile>,
    ) -> Self {
        if m4a {
            let mut decoder = crate::streaming_codecs::packet::PacketStream::file(rate, length);
            if retained {
                decoder.retain();
            }
            Self::M4a(Box::new(decoder), file)
        } else if retained
            && file
                .upgrade()
                .is_some_and(|f| f.prefix().starts_with(b"RIFF"))
        {
            Self::Wave(
                Box::new(crate::streaming_codecs::packet::PacketStream::wave(
                    rate, length,
                )),
                file,
            )
        } else if retained
            && file
                .upgrade()
                .is_some_and(|f| f.prefix().starts_with(b"OggS"))
        {
            Self::Vorbis(
                Box::new(crate::streaming_codecs::packet::PacketStream::vorbis(
                    rate, length,
                )),
                file,
            )
        } else {
            Self::Sequential(
                if retained {
                    crate::streaming_codecs::AutoDecoder::file_retained(rate)
                } else {
                    crate::streaming_codecs::AutoDecoder::new(rate)
                },
                file,
            )
        }
    }
    fn source_request(&self) -> Option<FileRequest> {
        match self {
            Self::Vorbis(d, _) => d.vorbis_request().map(FileRequest::Vorbis),
            Self::M4a(d, _) => d.source_request().map(FileRequest::M4a),
            Self::Wave(d, _) => d.wave_request().map(FileRequest::Wave),
            Self::Sequential(d, _) => d.mp3_request().map(FileRequest::Mp3),
        }
    }
}
impl Decoder for FileDecoder {
    fn timeline(&self) -> EngineResult<Option<streaming::StreamTimeline>> {
        match self {
            Self::Vorbis(d, _) => d.timeline(),
            Self::Sequential(d, _) => d.timeline(),
            Self::M4a(d, _) => d.timeline(),
            Self::Wave(d, _) => d.timeline(),
        }
    }
    fn presentation(&self) -> EngineResult<Option<streaming::StreamPresentation>> {
        match self {
            Self::Vorbis(d, _) => d.presentation(),
            Self::Sequential(d, _) => d.presentation(),
            Self::M4a(d, _) => d.presentation(),
            Self::Wave(d, _) => d.presentation(),
        }
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        // Cached replay performs no source read. Revalidate the same pinned
        // capability on its original CPU without holding a new caller owner.
        let (Self::M4a(_, file)
        | Self::Wave(_, file)
        | Self::Sequential(_, file)
        | Self::Vorbis(_, file)) = self;
        let file = file.upgrade().ok_or_else(io_error)?;
        if file.is_closed() {
            return Err(io_error());
        }
        let result = match self {
            Self::Vorbis(d, _) => d.step(input, eof, pcm, cancel),
            Self::Sequential(d, _) => d.step(input, eof, pcm, cancel),
            Self::M4a(d, _) => d.step(input, eof, pcm, cancel),
            Self::Wave(d, _) => d.step(input, eof, pcm, cancel),
        };
        if file.is_closed() {
            pcm.clear();
            return Err(io_error());
        }
        let step = result.map_err(|mut error| {
            if error.code == ErrorCode::IoError {
                error.code = ErrorCode::InvalidArgument;
            }
            error
        })?;
        if step.progress == transport::Progress::Finished && step.sample_rate == 0 {
            return Err(invalid("local file contains no complete audio frames"));
        }
        Ok(step)
    }
}
#[path = "streaming_file_control.rs"]
mod control;
#[path = "streaming_file_index.rs"]
pub(crate) mod index;
#[path = "streaming_file_wave.rs"]
mod wave;
fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, message)
}
fn io_error() -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, "local audio source unavailable")
}

pub(crate) fn run(
    cpu: migo_io::audio_cpu::AudioCpu,
    file: Arc<dyn AudioFile>,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    rate: u32,
    error: ErrorCell,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let source = FileSource {
        _owner: OffWorker::new(file.clone(), cpu.clone()),
        file,
        position: 0,
        index: index::Index::new(),
        pending: None,
    };
    let controls = state.accepts_controls().then(|| {
        if !crate::decoder::is_m4a(source.file.prefix()) {
            Ok(control::Controls::deferred(Some(error.clone())))
        } else {
            control::Controls::for_source(&state, Some(error.clone()))
        }
    });
    let file = Arc::downgrade(&source.file);
    async move {
        let length = source.file.length();
        let m4a = crate::decoder::is_m4a(source.file.prefix());
        let controlled = controls.is_some();
        let result = match controls {
            Some(Ok(controls)) => {
                transport::run_source_controlled(
                    cpu,
                    source,
                    Some(length),
                    tx.clone(),
                    state.clone(),
                    move |_| FileDecoder::new(rate, length, m4a, true, file),
                    controls,
                )
                .await
            }
            Some(Err(cause)) => {
                // Install failure also precedes the first observable message.
                *error.lock().unwrap() = Some(cause.clone());
                transport::publish_result(&tx, &state, Err(cause.clone())).await;
                Err(cause)
            }
            None => {
                transport::run_source(cpu, source, Some(length), &tx, &state, move |_| {
                    FileDecoder::new(rate, length, m4a, false, file)
                })
                .await
            }
        };
        if !controlled && let Err(cause) = &result {
            *error.lock().unwrap() = Some(cause.clone());
        }
        if !controlled {
            transport::publish_result(&tx, &state, result).await;
        }
    }
}

impl shared::audio_file::AudioFileCancel for StreamingState {
    fn is_cancelled(&self) -> bool {
        StreamingState::is_cancelled(self)
    }
}

#[cfg(test)]
#[path = "streaming_file_tests.rs"]
pub(crate) mod tests;
