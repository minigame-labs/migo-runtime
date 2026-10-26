//! One HTTP / CPU / PCM publication path for streaming codecs.
//!
//! A decoder accepts a prefix of a network chunk and performs a bounded step.
//! Retrying its suffix and draining Yield before requesting another body chunk
//! preserves the four-slot channel's backpressure all the way to the socket.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
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

async fn while_active<T>(
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
    if let Err(error) = download(cpu, client, url, &tx, &state, make_decoder).await {
        send(&tx, &state, StreamMsg::Error(error.to_string())).await;
    }
}

fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message)
}

async fn download<D: Decoder>(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    make_decoder: impl FnOnce() -> D + Send + 'static,
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
            client
                .get(migo_services::network::gate::GateKind::AudioStream, parsed)
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
    if let Some(length) = response.content_length() {
        state.bytes_total.store(length, Ordering::Relaxed);
    }

    let mut body = response.bytes_stream();
    // Construction can initialize codec tables and allocate scratch. Include it
    // in the first off-worker step, with no extra thread-pool round trip.
    let mut decoder = OffWorker::new(None::<D>, cpu);
    let mut make_decoder = Some(make_decoder);
    let mut pool = PcmPool::new();
    let mut format = None;
    let mut ready_sent = false;
    let mut total_samples = 0;
    loop {
        let Some(next) = while_active(
            tx,
            state,
            wait_for_network(state, BODY_IDLE_TIMEOUT, body.next()),
        )
        .await
        else {
            return Ok(());
        };
        let mut input = match next {
            NetworkWait::Cancelled => return Ok(()),
            NetworkWait::TimedOut => {
                return Err(EngineError::from_detail(
                    ErrorCode::Timeout,
                    "audio response body idle timeout",
                ));
            }
            NetworkWait::Ready(Some(Err(error))) => {
                return Err(invalid(&format!("Stream error: {error}")));
            }
            NetworkWait::Ready(Some(Ok(chunk))) => Some(chunk),
            NetworkWait::Ready(None) => None,
        };
        if let Some(chunk) = &input {
            state
                .bytes_downloaded
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        let eof = input.is_none();
        let mut offset = 0;
        loop {
            let mut pcm = pool.take();
            let cancel = state.clone();
            let initialize = make_decoder.take();
            let operation = decoder.with(move |slot| {
                let decoder = slot.get_or_insert_with(|| initialize.expect("first decoder step")());
                let offered = input.as_ref().map_or(&[][..], |chunk| &chunk[offset..]);
                let step = decoder.step(offered, eof, pcm.buffer_mut(), &cancel.cancelled);
                (input, pcm, step)
            });
            let Some(result) = while_active(tx, state, operation).await else {
                return Ok(());
            };
            let (rest, (returned_input, pcm, step)) = result?;
            decoder = rest;
            input = returned_input;
            let step = step?;
            let remaining = input.as_ref().map_or(0, |chunk| chunk.len()) - offset;
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
