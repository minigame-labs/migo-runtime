//! One source / CPU / PCM publication path for streaming codecs.
//!
//! A decoder accepts a prefix of a source chunk and performs a bounded step.
//! Retrying its suffix and draining Yield before requesting another body chunk
//! preserves the four-slot channel's backpressure all the way to its IO source.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shared::error::{EngineError, EngineResult, ErrorCode};
use tokio::sync::mpsc;

use crate::off_worker::OffWorker;
use crate::streaming::{
    BODY_IDLE_TIMEOUT, HttpClient, NetworkWait, PcmPool, RESPONSE_HEADER_TIMEOUT, StreamMsg,
    StreamingState, publish_stream_chunk, wait_for_network,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    NeedInput,
    NeedSource,
    Yield,
    Finished,
    Cancelled,
}

pub(crate) struct Step {
    pub consumed: usize,
    pub progress: Progress,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Codec and resampling work belongs here, never on the shared IO worker.
/// Implementations bound retained input and each step's CPU/output, clear PCM
/// on failure, and check the supplied cancellation flag within long steps.
pub(crate) trait Decoder: Send + 'static {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step>;
}

/// A source may suspend the CPU decoder for an index or a media reposition.
/// Requests are captured in the same CPU step; resolving them never occupies
/// a CPU worker while waiting for IO. Sequential codecs use the default source.
pub(crate) trait Source<D: Decoder>: Send {
    type Request: Send + 'static;

    fn pending(decoder: &D) -> EngineResult<Self::Request>;
    fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> impl Future<Output = EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>>> + Send;
    fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<D>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> impl Future<Output = EngineResult<Option<SourceResume<D>>>> + Send;
}

/// HTTP opening is separate from the source-independent publication loop.
pub(crate) trait HttpSource<D: Decoder>: Source<D> {
    fn request(
        request: migo_services::network::http::RequestBuilder,
    ) -> migo_services::network::http::RequestBuilder {
        request
    }
    fn new(
        client: HttpClient,
        response: migo_services::network::http::Response,
    ) -> EngineResult<Self>
    where
        Self: Sized;
}

pub(crate) enum SourceRead<C> {
    Data(C),
    End,
    Cancelled,
}

pub(crate) struct SourceResume<D> {
    pub decoder: OffWorker<Option<D>>,
    /// A media reposition invalidates the old response's unaccepted suffix.
    /// Installing an index preserves it, including when HTTP EOF was reached.
    pub restart: bool,
}

struct Sequential {
    response: migo_services::network::http::Response,
    // Preserve the original client's pool/domain lifetime through body EOF.
    _client: HttpClient,
}
impl<D: Decoder> Source<D> for Sequential {
    type Request = ();
    fn pending(_decoder: &D) -> EngineResult<()> {
        Err(invalid("sequential decoder requested source reposition"))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.response
            .chunk()
            .await
            .map(|chunk| match chunk {
                Some(bytes) => SourceRead::Data(bytes),
                None => SourceRead::End,
            })
            .map_err(|e| invalid(&format!("Stream error: {e}")))
    }
    async fn resume(
        &mut self,
        _request: (),
        _decoder: OffWorker<Option<D>>,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<D>>> {
        Err(invalid("sequential source cannot reposition"))
    }
}

impl<D: Decoder> HttpSource<D> for Sequential {
    fn new(
        client: HttpClient,
        response: migo_services::network::http::Response,
    ) -> EngineResult<Self> {
        Ok(Self {
            response,
            _client: client,
        })
    }
}

/// Also used for Error and Done: a stopped consumer must never strand a task
/// waiting for a slot, including when the stream has already reached EOF.
pub(crate) async fn send(
    tx: &mpsc::Sender<StreamMsg>,
    state: &StreamingState,
    message: StreamMsg,
) -> bool {
    tokio::select! {
        biased;
        _ = state.cancelled() => false,
        result = tx.send(message) => result.is_ok(),
    }
}

pub(crate) async fn while_active<T>(
    tx: &mpsc::Sender<StreamMsg>,
    state: &StreamingState,
    operation: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = state.cancelled() => None,
        _ = tx.closed() => {
            state.cancel();
            None
        },
        result = operation => Some(result),
    }
}

pub(crate) async fn run<D: Decoder>(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    make_decoder: impl FnOnce() -> D + Send + 'static,
) {
    run_with_source::<D, Sequential>(cpu, client, url, tx, state, move |_| make_decoder()).await;
}

pub(crate) async fn run_with_source<D: Decoder, S: HttpSource<D>>(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    make_decoder: impl FnOnce(Option<u64>) -> D + Send + 'static,
) {
    let result = download::<D, S>(cpu, client, url, &tx, &state, make_decoder).await;
    publish_result(&tx, &state, result).await;
}

fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message)
}

async fn download<D: Decoder, S: HttpSource<D>>(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    make_decoder: impl FnOnce(Option<u64>) -> D + Send + 'static,
) -> EngineResult<()> {
    if state.is_cancelled() || tx.is_closed() {
        return Ok(());
    }
    let parsed = migo_services::network::url::Url::parse(&url).map_err(|error| {
        EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("invalid audio URL: {error}"),
        )
    })?;
    let Some(response) = while_active(
        tx,
        state,
        wait_for_network(
            state,
            RESPONSE_HEADER_TIMEOUT,
            S::request(client.get(migo_services::network::gate::GateKind::AudioStream, parsed))
                .send(),
        ),
    )
    .await
    else {
        return Ok(());
    };
    let response = match response {
        NetworkWait::Cancelled => return Ok(()),
        NetworkWait::TimedOut => {
            return Err(EngineError::from_detail(
                ErrorCode::Timeout,
                "audio response header timeout",
            ));
        }
        NetworkWait::Ready(Err(error)) => {
            return Err(invalid(&format!("HTTP request failed: {error}")));
        }
        NetworkWait::Ready(Ok(response)) => response,
    };
    if !response.status().is_success() {
        return Err(invalid(&format!("HTTP error: {}", response.status())));
    }
    let object_len = response.content_length();
    if let Some(length) = object_len {
        state.bytes_total.store(length, Ordering::Relaxed);
    }

    let source = S::new(client, response)?;
    run_source(cpu, source, object_len, tx, state, make_decoder).await
}

pub(crate) async fn publish_result(
    tx: &mpsc::Sender<StreamMsg>,
    state: &StreamingState,
    result: EngineResult<()>,
) {
    if let Err(error) = result {
        send(tx, state, StreamMsg::Error(error.to_string())).await;
    }
}

pub(crate) async fn run_source<D: Decoder, S: Source<D>>(
    cpu: migo_io::audio_cpu::AudioCpu,
    mut source: S,
    object_len: Option<u64>,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    make_decoder: impl FnOnce(Option<u64>) -> D + Send + 'static,
) -> EngineResult<()> {
    if state.is_cancelled() || tx.is_closed() {
        return Ok(());
    }
    if let Some(length) = object_len {
        state.bytes_total.store(length, Ordering::Relaxed);
    }
    // Construction can initialize codec tables and allocate scratch. Include it
    // in the first off-worker step, with no extra thread-pool round trip.
    let mut decoder = OffWorker::new(None::<D>, cpu);
    let mut make_decoder = Some(make_decoder);
    let mut pool = PcmPool::new();
    let mut format = None;
    let mut ready_sent = false;
    let mut total_samples = 0;
    'download: loop {
        let Some(next) = while_active(
            tx,
            state,
            wait_for_network(state, BODY_IDLE_TIMEOUT, source.read(tx, state)),
        )
        .await
        else {
            return Ok(());
        };
        let next = match next {
            NetworkWait::Cancelled => SourceRead::Cancelled,
            NetworkWait::TimedOut => {
                return Err(EngineError::from_detail(
                    ErrorCode::Timeout,
                    "audio response body idle timeout",
                ));
            }
            NetworkWait::Ready(result) => result?,
        };
        let mut input = match next {
            SourceRead::Data(chunk) => Some(chunk),
            SourceRead::End => None,
            SourceRead::Cancelled => return Ok(()),
        };
        if let Some(chunk) = &input {
            state
                .bytes_downloaded
                .fetch_add(chunk.as_ref().len() as u64, Ordering::Relaxed);
        }
        let eof = input.is_none();
        let mut offset = 0;
        loop {
            let mut pcm = pool.take();
            let cancel = state.clone();
            let initialize = make_decoder.take();
            let operation = decoder.with(move |slot| {
                let decoder =
                    slot.get_or_insert_with(|| initialize.expect("first decoder step")(object_len));
                let offered = input
                    .as_ref()
                    .map_or(&[][..], |chunk| &chunk.as_ref()[offset..]);
                let step = decoder.step(offered, eof, pcm.buffer_mut(), &cancel.cancelled);
                let request = match &step {
                    Ok(step) if step.progress == Progress::NeedSource => Some(S::pending(decoder)),
                    _ => None,
                };
                (input, pcm, step, request)
            });
            let Some(result) = while_active(tx, state, operation).await else {
                return Ok(());
            };
            let (rest, (returned_input, pcm, step, request)) = result?;
            decoder = rest;
            input = returned_input;
            let step = step?;
            let request = request.transpose()?;
            let remaining = input.as_ref().map_or(0, |chunk| chunk.as_ref().len()) - offset;
            if step.consumed > remaining {
                return Err(invalid("stream decoder consumed beyond supplied input"));
            }
            offset += step.consumed;
            if step.progress == Progress::Cancelled {
                return Ok(());
            }
            if step.sample_rate != 0 || step.channels != 0 {
                if step.sample_rate == 0
                    || step.channels == 0
                    || !pcm.len().is_multiple_of(step.channels as usize)
                {
                    return Err(invalid("stream decoder returned an invalid PCM format"));
                }
                let current = (step.sample_rate, step.channels);
                if format.is_some_and(|format| format != current) {
                    return Err(invalid("stream PCM format changed"));
                }
                format = Some(current);
            }
            if !publish_stream_chunk(
                tx,
                state,
                pcm,
                step.sample_rate,
                step.channels,
                &mut ready_sent,
                &mut total_samples,
            )
            .await?
            {
                return Ok(());
            }
            match step.progress {
                Progress::Cancelled => unreachable!(),
                Progress::Finished => {
                    if !eof || remaining != 0 || !ready_sent || total_samples == 0 {
                        return Err(invalid(
                            "stream decoder finished without complete audio input",
                        ));
                    }
                    if send(tx, state, StreamMsg::Done).await {
                        state.download_complete.store(true, Ordering::Release);
                    }
                    return Ok(());
                }
                Progress::NeedSource => {
                    let request = request.ok_or_else(|| invalid("missing source request"))?;
                    let Some(resumed) =
                        while_active(tx, state, source.resume(request, decoder, tx, state)).await
                    else {
                        return Ok(());
                    };
                    let Some(resumed) = resumed? else {
                        return Ok(());
                    };
                    decoder = resumed.decoder;
                    if resumed.restart {
                        continue 'download;
                    }
                }
                Progress::NeedInput => {
                    if eof || (remaining != 0 && step.consumed == 0) {
                        return Err(invalid("stream decoder made no input progress"));
                    }
                    if step.consumed == remaining {
                        break;
                    }
                }
                Progress::Yield => tokio::task::yield_now().await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::{STREAM_CHANNEL_CAPACITY, stream_channel};
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_stream_releases_terminal_send_without_draining_the_queue() {
        for terminal in [StreamMsg::Done, StreamMsg::Error("decode failed".into())] {
            let (tx, _rx) = stream_channel();
            for _ in 0..STREAM_CHANNEL_CAPACITY {
                assert!(tx.try_send(StreamMsg::Done).is_ok());
            }
            let state = StreamingState::new();
            let waiting_state = state.clone();
            let waiting = tokio::spawn(async move { send(&tx, &waiting_state, terminal).await });
            tokio::task::yield_now().await;
            assert!(!waiting.is_finished());
            state.cancel();
            assert!(
                !tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .unwrap()
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn receiver_drop_releases_pending_work_and_latches_decoder_cancellation() {
        let (tx, rx) = stream_channel();
        let state = StreamingState::new();
        let operation = while_active(&tx, &state, std::future::pending::<()>());
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut operation)
                .await
                .is_err()
        );
        drop(rx);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), operation)
                .await
                .unwrap()
                .is_none()
        );
        assert!(state.is_cancelled());
        // Cancellation takes precedence even when the operation is ready.
        assert!(
            while_active(&tx, &state, std::future::ready(7))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn pcm_requires_its_own_complete_format_even_after_ready() {
        for (rate, channels, samples) in [(0, 0, 2), (48_000, 0, 2), (48_000, 2, 1)] {
            let (tx, mut rx) = stream_channel();
            let state = StreamingState::new();
            let mut pool = PcmPool::new();
            let mut pcm = pool.take();
            pcm.buffer_mut().resize(samples, 0.0);
            assert!(
                publish_stream_chunk(&tx, &state, pcm, rate, channels, &mut true, &mut 0)
                    .await
                    .is_err()
            );
            assert!(rx.try_recv().is_err());
        }
    }
}
