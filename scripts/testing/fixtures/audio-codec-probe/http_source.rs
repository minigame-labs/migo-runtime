//! Decide before decoder/PCM admission; never retry a published stream as a file.
//! Known sequential containers keep streaming. M4A needs demonstrated byte-range
//! identity; otherwise consume the original full response once into bounded spool.
use crate::{
    format_source::{self, Format},
    isomp4_transport::IndexedHttp,
    local_source::FileReader,
    push_decoder,
    replay_body::{Body, Replayed},
    resampled_streaming::Batched,
    source_chunk::BodyChunk,
    spool_source,
    streaming::{HttpClient, RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{self as transport, Decoder, HttpSource, Sequential, while_active},
};
use http::{HeaderValue, header::ACCEPT_ENCODING};
use migo_io::scheduler::IoScheduler;
use migo_services::network::{
    gate::GateKind,
    http::{Response, range},
    url::Url,
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{collections::VecDeque, future::Future, sync::Arc};
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sequential,
    Range,
    Buffered,
    Spool,
}
enum Source {
    Sequential(HttpClient, Response, Option<u64>, VecDeque<BodyChunk>),
    Range(IndexedHttp, Option<u64>, VecDeque<BodyChunk>, bool),
    Spool(FileReader),
}
pub struct Prepared {
    source: Source,
    format: Option<Format>,
    deadline: Instant,
}
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
async fn in_domain<T>(
    owner: &IoScheduler,
    work: impl Future<Output = EngineResult<T>>,
) -> EngineResult<T> {
    let domain = owner.domain();
    tokio::select! { biased; _=domain.closed()=>Err(error("audio source domain closed")), result=work=>result }
}
impl Prepared {
    #[cfg(test)]
    pub(crate) fn range_requests(&self) -> Option<u32> {
        match &self.source {
            Source::Range(source, _, _, false) => Some(source.control_requests()),
            _ => None,
        }
    }
    pub fn kind(&self) -> Kind {
        match self.source {
            Source::Sequential(..) => Kind::Sequential,
            Source::Range(_, _, _, false) => Kind::Range,
            Source::Range(_, _, _, true) => Kind::Buffered,
            Source::Spool(..) => Kind::Spool,
        }
    }
    /// Consume the selected owner once. Failures here never re-enter preparation.
    pub async fn run(
        self,
        owner: &IoScheduler,
        target: u32,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<()> {
        in_domain(owner, async move {
            owner.ensure_open().map_err(error)?;
            match self.source {
                Source::Spool(reader) => match self.format {
                    Some(format) => {
                        format_source::run_identified(owner, reader, format, target, tx, state)
                            .await
                    }
                    None => {
                        format_source::run(owner, reader, self.deadline, target, tx, state).await
                    }
                },
                Source::Range(source, length, prefix, _) => {
                    transport::run_source(
                        owner.audio_cpu(),
                        source.with_prefix(prefix),
                        length,
                        tx,
                        state,
                        move |length| {
                            Batched::<16, push_decoder::M4a<true>>::with_object_len(target, length)
                        },
                    )
                    .await
                }
                Source::Sequential(client, response, length, prefix) => match self.format {
                    Some(Format::Mp3) => {
                        sequential(
                            owner,
                            client,
                            response,
                            length,
                            prefix,
                            tx,
                            state,
                            move |_| Batched::<16, push_decoder::Mp3>::new(target),
                        )
                        .await
                    }
                    Some(Format::Wave) => {
                        sequential(
                            owner,
                            client,
                            response,
                            length,
                            prefix,
                            tx,
                            state,
                            move |_| Batched::<16, push_decoder::Wave>::new(target),
                        )
                        .await
                    }
                    Some(Format::Adts) => {
                        sequential(
                            owner,
                            client,
                            response,
                            length,
                            prefix,
                            tx,
                            state,
                            move |_| Batched::<16, push_decoder::Adts>::new(target),
                        )
                        .await
                    }
                    Some(Format::Ogg) => {
                        sequential(
                            owner,
                            client,
                            response,
                            length,
                            prefix,
                            tx,
                            state,
                            move |_| Batched::<16, push_decoder::Vorbis>::new(target),
                        )
                        .await
                    }
                    _ => Err(error("invalid sequential container selection")),
                },
            }
        })
        .await
    }
}
async fn sequential<D: Decoder>(
    owner: &IoScheduler,
    client: HttpClient,
    response: Response,
    length: Option<u64>,
    prefix: VecDeque<BodyChunk>,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    make: impl FnOnce(Option<u64>) -> D + Send + 'static,
) -> EngineResult<()> {
    let source = Replayed {
        source: <Sequential as HttpSource<D>>::new(client, response)?,
        prefix,
    };
    transport::run_source(owner.audio_cpu(), source, length, tx, state, make).await
}

/// `format` is a trusted, previously identified container, never a filename/MIME
/// guess. Unknown containers use the bounded library prefix probe first.
pub async fn response(
    client: HttpClient,
    response: Response,
    owner: &IoScheduler,
    format: Option<Format>,
    limits: spool_source::Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<Prepared>> {
    in_domain(
        owner,
        prepare(client, response, owner, format, limits, tx, state),
    )
    .await
}
async fn prepare(
    client: HttpClient,
    response: Response,
    owner: &IoScheduler,
    format: Option<Format>,
    limits: spool_source::Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<Prepared>> {
    owner.ensure_open().map_err(error)?;
    let deadline = limits.deadline;
    if Instant::now() >= deadline {
        return Err(error("audio source preparation expired"));
    }
    if state.is_cancelled() || tx.is_closed() {
        return Ok(None);
    }
    let mut body = Body::new(response)?;
    let decoded = body.response().has_decoded_representation();
    let identity = if !decoded {
        Some(range::Identity::capture(body.response()))
    } else {
        None
    };
    let format = if format.is_none() {
        let Some((returned, found)) =
            crate::http_prefix::probe(body, owner, deadline, tx, state).await?
        else {
            return Ok(None);
        };
        body = returned;
        found
    } else {
        format
    };
    if matches!(
        format,
        Some(Format::Mp3 | Format::Wave | Format::Adts | Format::Ogg)
    ) {
        return Ok(Some(Prepared {
            source: {
                let (response, length, prefix) = body.into_parts();
                Source::Sequential(client, response, length, prefix)
            },
            format,
            deadline,
        }));
    }
    if format == Some(Format::M4a)
        && body.complete()
        && body.received() <= crate::http_prefix::MAX_PREFIX_BYTES
    {
        let (response, length, prefix) = body.into_parts();
        return Ok(Some(Prepared {
            source: Source::Range(
                IndexedHttp::cached(client, response, length),
                length,
                prefix,
                true,
            ),
            format,
            deadline,
        }));
    }
    if format == Some(Format::M4a) && !decoded && !body.complete() {
        match identity.expect("identity captured before prefix") {
            Ok(identity) => {
                let mut reader = range::Reader::new(
                    client.clone(),
                    GateKind::AudioStream,
                    identity,
                    range::Limits::default(),
                );
                let probe_deadline = deadline.min(Instant::now() + RESPONSE_HEADER_TIMEOUT);
                let Some(capability) =
                    while_active(tx, state, reader.read_exact_at(0, 1, probe_deadline)).await
                else {
                    return Ok(None);
                };
                if Instant::now() >= probe_deadline {
                    return Err(error("audio range capability deadline exceeded"));
                }
                match capability {
                    Ok(_) => {
                        return Ok(Some(Prepared {
                            source: {
                                let length = reader.length();
                                let (response, _, prefix) = body.into_parts();
                                Source::Range(
                                    IndexedHttp::prepared(client, response, reader),
                                    length,
                                    prefix,
                                    false,
                                )
                            },
                            format,
                            deadline,
                        }));
                    }
                    Err(range::Error::RangeUnsupported) => {}
                    Err(e) => return Err(error(e)),
                }
            }
            Err(range::Error::NoStrongValidator) => {}
            Err(e) => return Err(error(e)),
        }
    }
    let reader = spool_source::body(body, owner, limits, tx, state).await?;
    drop(client);
    Ok(reader.map(|reader| Prepared {
        source: Source::Spool(reader),
        format,
        deadline,
    }))
}

pub async fn http(
    client: HttpClient,
    url: String,
    owner: &IoScheduler,
    format: Option<Format>,
    limits: spool_source::Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<Prepared>> {
    in_domain(owner, async {
        owner.ensure_open().map_err(error)?;
        if state.is_cancelled() || tx.is_closed() {
            return Ok(None);
        }
        let deadline = limits
            .deadline
            .min(Instant::now() + RESPONSE_HEADER_TIMEOUT);
        if Instant::now() >= deadline {
            return Err(error("audio source preparation expired"));
        }
        let url = Url::parse(&url).map_err(error)?;
        let Some(response) = while_active(
            tx,
            state,
            tokio::time::timeout_at(
                deadline,
                client
                    .get(GateKind::AudioStream, url)
                    .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
                    .send(),
            ),
        )
        .await
        else {
            return Ok(None);
        };
        let response = response
            .map_err(|_| error("audio source header deadline exceeded"))?
            .map_err(error)?;
        prepare(client, response, owner, format, limits, tx, state).await
    })
    .await
}
