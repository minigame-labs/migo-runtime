//! Choose the original HTTP source before creating a decoder or publishing PCM.
use crate::{
    streaming::{
        BODY_IDLE_TIMEOUT, HttpClient, RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState,
    },
    streaming_codecs::AutoDecoder,
    streaming_http::IndexedHttp,
    streaming_transport::{self as transport, HttpSource},
};
use migo_services::network::{
    gate::GateKind,
    http::{Bytes, Response, range},
};
use shared::{
    audio_file::{AudioFileChunk, AudioHttpSpool, AudioSpoolBody, AudioSpoolFuture},
    error::{EngineError, EngineResult, ErrorCode},
};
use std::{collections::VecDeque, sync::Arc};
use tokio::{sync::mpsc, time::Instant};

fn error(detail: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail.to_string())
}
struct Body {
    response: Response,
    client: HttpClient,
    prefix: VecDeque<Bytes>,
    pending: Option<Bytes>,
    length: Option<u64>,
}
// Charge the actual owned Box layout, not Bytes' opaque backing capacity.
struct Chunk {
    data: Box<[u8]>,
    _ticket: migo_io::pools::ByteTicket,
    _client: HttpClient,
}
impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl AudioSpoolBody for Body {
    fn length(&self) -> Option<u64> {
        self.length
    }
    fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<AudioFileChunk>> {
        Box::pin(async move {
            loop {
                let bytes = if let Some(bytes) = self.pending.take() {
                    Some(bytes)
                } else if let Some(bytes) = self.prefix.pop_front() {
                    Some(bytes)
                } else {
                    self.response.chunk().await.map_err(error)?
                };
                let Some(mut bytes) = bytes else {
                    return Ok(None);
                };
                if bytes.is_empty() {
                    continue;
                }
                let n = bytes.len().min(migo_io::read_at_file::FILE_CHUNK_BYTES);
                let part = bytes.split_to(n);
                if !bytes.is_empty() {
                    self.pending = Some(bytes);
                }
                let ticket = self
                    .client
                    .reserve_response_bytes(n as u64)
                    .map_err(error)?;
                let data = part.as_ref().to_vec().into_boxed_slice();
                return Ok(Some(AudioFileChunk::new(Chunk {
                    data,
                    _ticket: ticket,
                    _client: self.client.clone(),
                })));
            }
        })
    }
}
pub(crate) fn body_for_spool(
    client: HttpClient,
    response: Response,
    prefix: VecDeque<Bytes>,
    length: Option<u64>,
) -> Box<dyn AudioSpoolBody> {
    Box::new(Body {
        response,
        client,
        prefix,
        pending: None,
        length,
    })
}
pub(crate) async fn run(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    rate: u32,
    spool: Arc<dyn AudioHttpSpool>,
) {
    let result = prepare(cpu, client, &url, &tx, &state, rate, spool).await;
    transport::publish_result(&tx, &state, result).await;
}
async fn prepare(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: &str,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    rate: u32,
    spool: Arc<dyn AudioHttpSpool>,
) -> EngineResult<()> {
    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    let Some(mut response) =
        transport::open_response(&client, url, tx, state, IndexedHttp::request).await?
    else {
        return Ok(());
    };
    let length = response.content_length();
    if let Some(n) = length {
        state
            .bytes_total
            .store(n, std::sync::atomic::Ordering::Relaxed);
    }
    let identity = range::Identity::capture(&response);
    let decoded = response.has_decoded_representation();
    let mut prefix = VecDeque::new();
    let mut signature = [0; 8];
    let mut have = 0;
    for _ in 0..128 {
        if have == 8 {
            break;
        }
        let Some(chunk) = transport::while_active(
            tx,
            state,
            tokio::time::timeout_at(
                deadline.min(Instant::now() + BODY_IDLE_TIMEOUT),
                response.chunk(),
            ),
        )
        .await
        else {
            return Ok(());
        };
        let Some(chunk) = chunk
            .map_err(|_| EngineError::new(ErrorCode::Timeout))?
            .map_err(error)?
        else {
            break;
        };
        let n = (8 - have).min(chunk.len());
        signature[have..have + n].copy_from_slice(&chunk[..n]);
        have += n;
        if !chunk.is_empty() {
            prefix.push_back(chunk);
        }
    }
    let m4a = crate::decoder::is_m4a(&signature[..have]);
    let wave = signature[..have].starts_with(b"RIFF");
    let mp3 = state.accepts_controls()
        && crate::decoder::is_mp3(&signature[..have])
        && !crate::decoder::is_adts(&signature[..have]);
    let vorbis = state.accepts_controls() && signature[..have].starts_with(b"OggS");
    if !m4a && !wave && !mp3 && !vorbis {
        let source = IndexedHttp::prepared(client, response, identity, prefix, None)?;
        return transport::run_source(cpu, source, length, tx, state, move |_| {
            AutoDecoder::new(rate)
        })
        .await;
    }
    let full =
        response.status().as_u16() == 200 && !response.headers().contains_key("content-range");
    if m4a && !full {
        return Err(error("M4A preparation requires a full HTTP representation"));
    }
    if !decoded && full {
        match identity {
            Ok(identity) => {
                let mut reader = range::Reader::new(
                    client.clone(),
                    GateKind::AudioStream,
                    identity,
                    range::Limits::default(),
                );
                let mut probe = [0];
                let Some(result) = transport::while_active(
                    tx,
                    state,
                    reader.read_exact_into(
                        0,
                        &mut probe,
                        deadline.min(Instant::now() + RESPONSE_HEADER_TIMEOUT),
                    ),
                )
                .await
                else {
                    return Ok(());
                };
                match result {
                    Ok(()) if reader.length().is_some() => {
                        let source = IndexedHttp::prepared(
                            client,
                            response,
                            Err(range::Error::PreviouslyFailed),
                            prefix,
                            Some(reader),
                        )?;
                        return source.run_selected(cpu, length, tx, state, rate).await;
                    }
                    Ok(()) | Err(range::Error::RangeUnsupported) => {}
                    Err(cause) => return Err(error(cause)),
                }
            }
            Err(range::Error::NoStrongValidator) => {}
            Err(_) if wave || mp3 || vorbis => {}
            Err(cause) => return Err(error(cause)),
        }
    }
    if wave || mp3 || vorbis {
        // An unavailable Range capability keeps the original progressive body.
        let identity = range::Identity::capture(&response);
        let source = IndexedHttp::prepared(client, response, identity, prefix, None)?;
        return transport::run_source(cpu, source, length, tx, state, move |_| {
            AutoDecoder::new(rate)
        })
        .await;
    }
    let body = body_for_spool(client, response, prefix, length);
    let Some(source) = transport::while_active(
        tx,
        state,
        spool.prepare(body, state.clone(), deadline.into_std()),
    )
    .await
    else {
        return Ok(());
    };
    let source = source?;
    // FileSource creates its original CPU owner before this future's first poll.
    crate::streaming_file::run(
        cpu,
        source,
        tx.clone(),
        state.clone(),
        rate,
        Arc::new(std::sync::Mutex::new(None)),
    )
    .await;
    Ok(())
}
