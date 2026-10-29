//! M4A source adapter for the shared HTTP/CPU/PCM publication loop.
//! Format parsing remains in the library and IndexSource; no second decode loop.
use crate::{
    indexed_seek::{SeekContext, SeekSource},
    isomp4_source::{IndexSource, Limits, MediaRead},
    off_worker::OffWorker,
    push_decoder::{M4aSourceDecoder, M4aSourceRequest},
    streaming::{HttpClient, RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{Decoder, HttpSource, Source, SourceRead, SourceResume},
};
use bytes::Bytes;
use http::{HeaderValue, header::ACCEPT_ENCODING};
use migo_services::network::{
    gate::GateKind,
    http::{RequestBuilder, Response, range},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use tokio::{sync::mpsc, time::Instant};

pub struct IndexedHttp {
    client: HttpClient,
    identity: Option<Result<range::Identity, range::Error>>,
    initial: Option<Response>,
    source: Option<IndexSource>,
    pending: Option<Bytes>,
    limits: Limits,
    replay: std::collections::VecDeque<crate::source_chunk::BodyChunk>,
}
impl IndexedHttp {
    /// The complete bounded prefix is replayed by the outer source. All bytes
    /// are already owned; the buffered library decoder must not issue a GET.
    pub(crate) fn cached(client: HttpClient, response: Response, length: Option<u64>) -> Self {
        let mut limits = Limits::default();
        limits.index.object_len = length;
        Self {
            client,
            identity: Some(Err(range::Error::NoStrongValidator)),
            initial: Some(response),
            source: None,
            pending: None,
            limits,
            replay: Default::default(),
        }
    }
    /// Retain the actual capability probe's reader, counters and learned length.
    /// The original full response is still unread and remains the initial body.
    pub(crate) fn prepared(client: HttpClient, response: Response, reader: range::Reader) -> Self {
        let mut limits = Limits::default();
        limits.index.object_len = reader.length();
        let source = IndexSource::new(reader, limits);
        Self {
            client,
            identity: None,
            initial: Some(response),
            source: Some(source),
            pending: None,
            limits,
            replay: Default::default(),
        }
    }
    pub(crate) fn with_prefix(
        mut self,
        prefix: std::collections::VecDeque<crate::source_chunk::BodyChunk>,
    ) -> Self {
        self.replay = prefix;
        self
    }
    #[cfg(test)]
    pub(crate) fn control_requests(&self) -> u32 {
        self.source.as_ref().expect("prepared source").requests()
    }
}
fn error(cause: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.to_string())
}
impl<D: Decoder + M4aSourceDecoder> Source<D> for IndexedHttp {
    type Request = M4aSourceRequest;
    fn pending(decoder: &D) -> EngineResult<Self::Request> {
        decoder
            .source_request()
            .ok_or_else(|| error("M4A source command is not ready"))
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        use crate::source_chunk::Chunk;
        if let Some(bytes) = self.replay.pop_front() {
            return Ok(SourceRead::Data(Chunk::Prefix(bytes)));
        }
        if let Some(bytes) = self.pending.take() {
            return Ok(SourceRead::Data(Chunk::Tail(bytes)));
        }
        if let Some(response) = &mut self.initial {
            return response
                .chunk()
                .await
                .map(|chunk| match chunk {
                    Some(bytes) => SourceRead::Data(Chunk::Tail(bytes)),
                    None => SourceRead::End,
                })
                .map_err(error);
        }
        match self
            .source
            .as_mut()
            .ok_or_else(|| error("M4A media source is absent"))?
            .media_chunk(tx, state)
            .await
            .map_err(error)?
        {
            MediaRead::Data(bytes) => Ok(SourceRead::Data(Chunk::Tail(bytes))),
            MediaRead::End => Ok(SourceRead::End),
            MediaRead::Cancelled => Ok(SourceRead::Cancelled),
        }
    }
    fn resume<'a, 'b, 'c>(
        &'a mut self,
        request: Self::Request,
        decoder: OffWorker<Option<D>>,
        tx: &'b mpsc::Sender<StreamMsg>,
        state: &'c Arc<StreamingState>,
    ) -> impl std::future::Future<Output = EngineResult<Option<SourceResume<D>>>>
    + Send
    + use<'a, 'b, 'c, D> {
        self.resume_before(
            request,
            decoder,
            SeekContext {
                tx,
                state,
                deadline: Instant::now() + RESPONSE_HEADER_TIMEOUT,
            },
        )
    }
}

impl<D: Decoder + M4aSourceDecoder> SeekSource<D> for IndexedHttp {
    async fn resume_before(
        &mut self,
        request: M4aSourceRequest,
        decoder: OffWorker<Option<D>>,
        context: SeekContext<'_, '_>,
    ) -> EngineResult<Option<SourceResume<D>>> {
        let SeekContext {
            tx,
            state,
            deadline,
        } = context;
        if self.source.is_none() {
            let identity = self
                .identity
                .take()
                .ok_or_else(|| error("M4A representation was already consumed"))?
                .map_err(error)?;
            self.source = Some(IndexSource::new(
                range::Reader::new(
                    self.client.clone(),
                    GateKind::AudioStream,
                    identity,
                    range::Limits::default(),
                ),
                self.limits,
            ));
        }
        let source = self.source.as_mut().expect("source initialized");
        match request {
            M4aSourceRequest::Index(request) => {
                let Some(resolved) = source
                    .resolve(request, decoder, tx, state, deadline)
                    .await
                    .map_err(error)?
                else {
                    return Ok(None);
                };
                Ok(Some(SourceResume {
                    decoder: resolved.decoder,
                    restart: false,
                }))
            }
            M4aSourceRequest::ReadAt(packet) => {
                let Some(resumed) = source
                    .read_at(packet, decoder, tx, state, deadline)
                    .await
                    .map_err(error)?
                else {
                    return Ok(None);
                };
                self.initial = None;
                self.replay.clear();
                self.pending = Some(resumed.first);
                Ok(Some(SourceResume {
                    decoder: resumed.decoder,
                    restart: true,
                }))
            }
        }
    }
}

impl<D: Decoder + M4aSourceDecoder> HttpSource<D> for IndexedHttp {
    fn request(request: RequestBuilder) -> RequestBuilder {
        request.header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
    }
    fn new(client: HttpClient, response: Response) -> EngineResult<Self> {
        if response.status() != http::StatusCode::OK {
            return Err(error("indexed source requires a complete initial response"));
        }
        let mut limits = Limits::default();
        limits.index.object_len = response.content_length();
        Ok(Self {
            client,
            identity: Some(range::Identity::capture(&response)),
            initial: Some(response),
            source: None,
            pending: None,
            limits,
            replay: Default::default(),
        })
    }
}
