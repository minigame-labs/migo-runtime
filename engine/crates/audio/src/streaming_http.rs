//! Indexed HTTP packet codecs on the original source / CPU / PCM driver.
//! MP4 framing/index and RIFF/PCM positioning stay in their mature libraries.
//! Control reads and media transitions retain the original admitted CPU owner.
use crate::decoder::aac::m4a::SourceRequest;
use crate::off_worker::OffWorker;
use crate::streaming::{HttpClient, RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState};
use crate::streaming_codecs::{AutoDecoder, HttpRequest};
use crate::streaming_file::index::Index;
use crate::streaming_transport::{HttpSource, Sequential, Source, SourceRead, SourceResume};
use migo_services::network::{
    gate::GateKind,
    http::{Bytes, RequestBuilder, Response, range},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexedPacket,
};
use tokio::{sync::mpsc, time::Instant};

enum Owner {
    Initial(Result<range::Identity, range::Error>),
    Control(range::Reader),
    Media(Box<range::OwnedStream>),
    Failed,
}
// Field order is intentional: storage is destroyed before its credit returns.
struct Input {
    data: Box<[u8]>,
    _ticket: migo_io::pools::ByteTicket,
}
impl Input {
    fn new(client: &HttpClient, length: usize) -> EngineResult<Self> {
        let ticket = client
            .reserve_response_bytes(length as u64)
            .map_err(error)?;
        Ok(Self {
            data: vec![0; length].into_boxed_slice(),
            _ticket: ticket,
        })
    }
}
pub(crate) struct IndexedHttp {
    owner: Owner,
    initial: Option<Sequential>,
    pending: Option<Bytes>,
    index: Index,
    failed: bool,
    interrupted: bool,
    terminal: bool,
    client: HttpClient,
    prefix: std::collections::VecDeque<Bytes>,
    #[cfg(test)]
    input_observer: Option<tokio::sync::oneshot::Sender<migo_alloc_probe::DeallocationWatch>>,
}
fn error(cause: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.to_string())
}
fn check(state: &StreamingState, deadline: Instant) -> EngineResult<()> {
    if state.is_cancelled() {
        return Err(EngineError::new(ErrorCode::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(EngineError::new(ErrorCode::Timeout));
    }
    Ok(())
}
impl IndexedHttp {
    pub(crate) fn prepared(
        client: HttpClient,
        response: Response,
        identity: Result<range::Identity, range::Error>,
        prefix: std::collections::VecDeque<Bytes>,
        reader: Option<range::Reader>,
    ) -> EngineResult<Self> {
        let mut value = Self::new(client, response)?;
        value.owner = reader.map_or_else(|| Owner::Initial(identity), Owner::Control);
        value.prefix = prefix;
        Ok(value)
    }
    fn control(&mut self) -> EngineResult<&mut range::Reader> {
        self.owner = match std::mem::replace(&mut self.owner, Owner::Failed) {
            Owner::Initial(identity) => Owner::Control(range::Reader::new(
                self.client.clone(),
                GateKind::AudioStream,
                identity.map_err(error)?,
                range::Limits::default(),
            )),
            Owner::Media(media) => Owner::Control((*media).into_reader()),
            other => other,
        };
        match &mut self.owner {
            Owner::Control(reader) => Ok(reader),
            _ => Err(error("HTTP M4A range reader retired")),
        }
    }
    async fn atom(&mut self, position: u64, deadline: Instant) -> EngineResult<AtomSpan> {
        let reader = self.control()?;
        let mut header = [0; 16];
        reader
            .read_exact_into(position, &mut header[..8], deadline)
            .await
            .map_err(error)?;
        let span = AtomSpan::read(&header[..8], position, reader.length()).map_err(error)?;
        let span = if let Some(span) = span {
            span
        } else {
            reader
                .read_exact_into(position, &mut header, deadline)
                .await
                .map_err(error)?;
            AtomSpan::read(&header, position, reader.length())
                .map_err(error)?
                .ok_or_else(|| error("HTTP M4A truncated atom"))?
        };
        if span.kind == AtomKind::Fragmented {
            return Err(error("fragmented MP4 is outside the HTTP M4A profile"));
        }
        if span
            .end
            .is_none_or(|end| end <= position || reader.length().is_none_or(|n| end > n))
        {
            return Err(error("HTTP M4A atom extent outside source"));
        }
        Ok(span)
    }
    async fn media(&mut self, packet: IndexedPacket, deadline: Instant) -> EngineResult<AtomSpan> {
        let length = self
            .control()?
            .length()
            .ok_or_else(|| error("HTTP M4A missing object length"))?;
        let end = packet
            .offset
            .checked_add(u64::from(packet.size))
            .filter(|n| *n > packet.offset && *n <= length)
            .ok_or_else(|| error("HTTP M4A packet extent outside source"))?;
        if let Some(span) = self.index.media_for(packet) {
            return Ok(span);
        }
        let mut position = 0;
        let mut skipped = 0;
        for _ in 0..32 {
            let span = self.atom(position, deadline).await?;
            let limit = span.end.unwrap();
            if span.kind == AtomKind::MediaData {
                self.index.remember(span).map_err(error)?;
            } else {
                skipped += limit - position;
                if skipped > 4 * 1024 * 1024 {
                    return Err(error("HTTP M4A metadata budget exceeded"));
                }
            }
            if span.offset <= packet.offset && packet.offset < limit {
                if span.kind != AtomKind::MediaData
                    || packet.offset < span.body_offset
                    || end > limit
                {
                    return Err(error("HTTP M4A packet outside actual mdat"));
                }
                return Ok(span);
            }
            position = limit;
        }
        Err(error("HTTP M4A media discovery atom budget exceeded"))
    }
    async fn resolve(
        &mut self,
        request: SourceRequest,
        decoder: OffWorker<Option<AutoDecoder>>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> EngineResult<SourceResume<AutoDecoder>> {
        check(state, deadline)?;
        match request {
            SourceRequest::Index(request) => {
                if request.media.kind != AtomKind::MediaData
                    || request.next_atom_offset != request.media.end
                {
                    return Err(error("HTTP M4A invalid index request"));
                }
                let actual = self.atom(request.media.offset, deadline).await?;
                if actual != request.media {
                    return Err(error("HTTP M4A requested mdat changed"));
                }
                self.index.remember(actual).map_err(error)?;
                let mut position = request
                    .next_atom_offset
                    .ok_or_else(|| error("HTTP M4A missing tail index extent"))?;
                let mut skipped = 0;
                for _ in 0..32 {
                    let span = self.atom(position, deadline).await?;
                    let end = span.end.unwrap();
                    if span.kind == AtomKind::Movie {
                        let length = usize::try_from(end - position).map_err(error)?;
                        if length > IndexOptions::default().max_index_bytes {
                            return Err(error("HTTP M4A index budget exceeded before body read"));
                        }
                        let mut input = Input::new(&self.client, length)?;
                        self.control()?
                            .read_exact_into(position, &mut input.data, deadline)
                            .await
                            .map_err(error)?;
                        #[cfg(test)]
                        if let Some(observer) = self.input_observer.take() {
                            let _ = observer.send(
                                migo_alloc_probe::DeallocationWatch::new(input.data.as_ptr())
                                    .unwrap(),
                            );
                        }
                        let length = self
                            .control()?
                            .length()
                            .ok_or_else(|| error("HTTP M4A missing object length"))?;
                        let active = state.clone();
                        let operation = decoder.with(move |value| {
                            // Capture the complete owner, including its ticket;
                            // field-only capture would refund on the IO caller.
                            let bytes = input;
                            check(&active, deadline)?;
                            let decoder = value
                                .as_mut()
                                .ok_or_else(|| error("HTTP M4A decoder retired"))?;
                            if decoder.source_request() != Some(SourceRequest::Index(request)) {
                                return Err(error("HTTP M4A index request changed"));
                            }
                            let index = AudioIndex::from_moov(
                                &bytes.data,
                                IndexOptions {
                                    object_len: Some(length),
                                    ..Default::default()
                                },
                            )
                            .map_err(error)?;
                            check(&active, deadline)?;
                            decoder.m4a()?.install_http_index(index, length)
                        });
                        let (decoder, result) = tokio::time::timeout_at(deadline, operation)
                            .await
                            .map_err(|_| EngineError::new(ErrorCode::Timeout))??;
                        result?;
                        return Ok(SourceResume {
                            decoder,
                            restart: false,
                        });
                    }
                    if span.kind == AtomKind::MediaData {
                        self.index.remember(span).map_err(error)?;
                    } else {
                        skipped += end - position;
                        if skipped > 4 * 1024 * 1024 {
                            return Err(error("HTTP M4A tail metadata budget exceeded"));
                        }
                    }
                    position = end;
                }
                Err(error("HTTP M4A index discovery atom budget exceeded"))
            }
            SourceRequest::ReadAt(packet) => {
                self.index.charge(state).map_err(error)?;
                let media = self.media(packet, deadline).await?;
                self.restart_at(packet.offset, decoder, state, deadline, move |decoder| {
                    if decoder.source_request() != Some(SourceRequest::ReadAt(packet)) {
                        return Err(error("HTTP M4A packet request changed"));
                    }
                    decoder.m4a()?.resume_at(media, packet.offset)
                })
                .await
            }
        }
    }
    // One media transition for indexed M4A and independently framed PCM.
    // Preserve the same Reader, counters, original deadline and CPU handoff.
    async fn restart_at(
        &mut self,
        offset: u64,
        decoder: OffWorker<Option<AutoDecoder>>,
        state: &Arc<StreamingState>,
        deadline: Instant,
        resume: impl FnOnce(&mut AutoDecoder) -> EngineResult<()> + Send + 'static,
    ) -> EngineResult<SourceResume<AutoDecoder>> {
        let length = self
            .control()?
            .length()
            .ok_or_else(|| error("HTTP M4A missing object length"))?;
        let Owner::Control(reader) = std::mem::replace(&mut self.owner, Owner::Failed) else {
            return Err(error("HTTP M4A reader unavailable"));
        };
        self.owner = Owner::Media(Box::new(
            reader
                .into_stream_at(offset, length - offset, range::StreamLimits::default())
                .map_err(error)?,
        ));
        let Owner::Media(stream) = &mut self.owner else {
            unreachable!()
        };
        let first = tokio::time::timeout_at(deadline, stream.chunk())
            .await
            .map_err(|_| EngineError::new(ErrorCode::Timeout))?
            .map_err(error)?
            .ok_or_else(|| error("HTTP M4A missing media bytes"))?;
        let active = state.clone();
        let operation = decoder.with(move |value| {
            let result = (|| {
                check(&active, deadline)?;
                let decoder = value
                    .as_mut()
                    .ok_or_else(|| error("HTTP decoder retired"))?;
                resume(decoder)
            })();
            (result, first)
        });
        let (decoder, (result, first)) = tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| EngineError::new(ErrorCode::Timeout))??;
        result?;
        self.terminal = false;
        self.initial = None;
        self.prefix.clear();
        self.pending = Some(first);
        Ok(SourceResume {
            decoder,
            restart: true,
        })
    }
}
impl Source<AutoDecoder> for IndexedHttp {
    type Request = HttpRequest;
    fn pending(decoder: &AutoDecoder) -> EngineResult<HttpRequest> {
        decoder
            .http_request()
            .ok_or_else(|| error("HTTP M4A source command missing"))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        if self.failed {
            return Err(error("indexed HTTP source retired"));
        }
        if self.terminal {
            return Ok(SourceRead::End);
        }
        self.failed = true;
        self.interrupted = true;
        let result = async {
            let bytes = if let Some(prefix) = self.prefix.pop_front() {
                Some(prefix)
            } else if let Some(first) = self.pending.take() {
                Some(first)
            } else if let Some(initial) = &mut self.initial {
                initial.chunk().await.map_err(error)?
            } else if let Owner::Media(stream) = &mut self.owner {
                stream.chunk().await.map_err(error)?
            } else {
                return Err(error("HTTP M4A media source missing"));
            };
            Ok(bytes.map_or(SourceRead::End, SourceRead::Data))
        }
        .await;
        self.interrupted = false;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
    async fn resume(
        &mut self,
        request: HttpRequest,
        decoder: OffWorker<Option<AutoDecoder>>,
        _tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<AutoDecoder>>> {
        if self.failed {
            return Err(error("indexed HTTP source retired"));
        }
        self.failed = true;
        let result = self
            .resolve_request(
                request,
                decoder,
                state,
                Instant::now() + RESPONSE_HEADER_TIMEOUT,
            )
            .await?;
        self.failed = false;
        Ok(Some(result))
    }
}
impl HttpSource<AutoDecoder> for IndexedHttp {
    fn request(request: RequestBuilder) -> RequestBuilder {
        request.header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        )
    }
    fn new(client: HttpClient, response: Response) -> EngineResult<Self> {
        let identity = range::Identity::capture(&response);
        let initial = <Sequential as HttpSource<AutoDecoder>>::new(client.clone(), response)?;
        Ok(Self {
            owner: Owner::Initial(identity),
            initial: Some(initial),
            pending: None,
            index: Index::new(),
            failed: false,
            interrupted: false,
            terminal: false,
            client,
            prefix: Default::default(),
            #[cfg(test)]
            input_observer: None,
        })
    }
}

#[path = "streaming_http_control.rs"]
mod control;

#[cfg(test)]
#[path = "streaming_http_tests.rs"]
pub(crate) mod tests;

#[path = "streaming_http_wave.rs"]
mod wave;
