//! Bounded Content-Encoding streams. Codec polls run on the session's existing
//! fair CPU/IO executor, never on its JS thread. There is no decompression task
//! running ahead of the reader: at most one bounded poll is admitted at a time.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use async_compression::tokio::bufread::{BrotliDecoder, GzipDecoder};
use bytes::Bytes;
use futures::Stream;
use http::{
    HeaderMap, Method, StatusCode,
    header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE},
};
use http_body::Body;
use migo_io::{
    polling::Offloaded,
    pools::{ByteTicket, IoPools},
    task::PoolKind,
};
use parking_lot::Mutex;
use tokio_util::io::{ReaderStream, StreamReader};

use super::body::ByteStream;

const CHUNK: usize = 64 * 1024;
const MAX_LAYERS: usize = 2;
const MAX_BYTES: u64 = 100 * 1024 * 1024;
const MAX_RATIO: u64 = 256;
const RATIO_SLACK: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum Coding {
    Gzip,
    Brotli,
}

/// Unsupported encodings remain opaque, with both representation headers intact.
/// A Range request/answer always remains in its original byte coordinate system.
pub(super) fn codings(
    method: &Method,
    range_request: bool,
    status: StatusCode,
    headers: &HeaderMap,
) -> io::Result<Option<DecodePlan>> {
    if *method == Method::HEAD
        || range_request
        || status.is_informational()
        || matches!(
            status,
            StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED | StatusCode::PARTIAL_CONTENT
        )
        || headers.contains_key(CONTENT_RANGE)
        || headers.get(CONTENT_LENGTH).is_some_and(|v| v == "0")
    {
        return Ok(None);
    }
    let mut result = Vec::new();
    for value in headers.get_all(CONTENT_ENCODING) {
        let value = value
            .to_str()
            .map_err(|_| invalid("invalid Content-Encoding"))?;
        for name in value.split(',') {
            let coding = match name.trim().to_ascii_lowercase().as_str() {
                "gzip" | "x-gzip" => Coding::Gzip,
                "br" => Coding::Brotli,
                "identity" => continue,
                "" => return Err(invalid("empty Content-Encoding")),
                _ => return Ok(None),
            };
            result.push(coding);
        }
    }
    if result.len() > MAX_LAYERS {
        return Err(invalid("too many Content-Encoding layers"));
    }
    Ok((!result.is_empty()).then_some(DecodePlan(result)))
}

pub(super) struct DecodePlan(Vec<Coding>);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A budget for each layer is reset before a CPU job polls. In particular an
/// arbitrarily long run of empty gzip members cannot monopolize that worker.
struct WireStream {
    incoming: hyper::body::Incoming,
    pending: Bytes,
    budget: Arc<AtomicUsize>,
    count: Arc<AtomicU64>,
}

impl Stream for WireStream {
    type Item = io::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        for _ in 0..32 {
            let budget = self.budget.load(Ordering::Relaxed);
            if budget == 0 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if !self.pending.is_empty() {
                let length = budget.min(self.pending.len()).min(CHUNK);
                let total = self.count.load(Ordering::Relaxed) + length as u64;
                if total > MAX_BYTES {
                    return Poll::Ready(Some(Err(invalid("encoded HTTP body limit exceeded"))));
                }
                let bytes = self.pending.split_to(length);
                self.budget.fetch_sub(length, Ordering::Relaxed);
                self.count.store(total, Ordering::Relaxed);
                return Poll::Ready(Some(Ok(bytes)));
            }
            match Pin::new(&mut self.incoming).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Some(Err(io::Error::other(error))));
                }
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.pending = bytes;
                    }
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

struct LayerStream {
    inner: ByteStream,
    input: Arc<AtomicU64>,
    output: Arc<AtomicU64>,
    budget: Arc<AtomicUsize>,
}

impl Stream for LayerStream {
    type Item = io::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.budget.load(Ordering::Relaxed) == 0 {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let next = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(Some(Ok(bytes))) = &next {
            let total = self.output.load(Ordering::Relaxed) + bytes.len() as u64;
            if total > MAX_BYTES
                || total
                    > self
                        .input
                        .load(Ordering::Relaxed)
                        .saturating_mul(MAX_RATIO)
                        .saturating_add(RATIO_SLACK)
            {
                return Poll::Ready(Some(Err(invalid(
                    "decoded HTTP body expansion limit exceeded",
                ))));
            }
            self.output.store(total, Ordering::Relaxed);
            self.budget
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    Some(left.saturating_sub(bytes.len()))
                })
                .unwrap();
        }
        next
    }
}

struct DecodeState {
    stream: ByteStream,
    budgets: Vec<Arc<AtomicUsize>>,
    // Admission credit for codec workspace and bounded input/output buffers.
    // This is accounting for our codec lane, not a process-wide allocator cap.
    _workspace: ByteTicket,
}

struct ReadChunk {
    state: Arc<Mutex<DecodeState>>,
}

impl Future for ReadChunk {
    type Output = Option<io::Result<Bytes>>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock();
        for budget in &state.budgets {
            budget.store(CHUNK, Ordering::Relaxed);
        }
        state.stream.as_mut().poll_next(cx)
    }
}

pub(super) struct DecodedBody {
    state: Arc<Mutex<DecodeState>>,
    pools: IoPools,
    // Kept in the body, not the chunk future: cancelling a read must not lose
    // the chunk that an already admitted CPU poll is about to produce.
    flight: Option<Offloaded<ReadChunk>>,
    pub(super) wire_bytes: Arc<AtomicU64>,
}

impl DecodedBody {
    pub(super) fn new(
        incoming: hyper::body::Incoming,
        plan: DecodePlan,
        pools: IoPools,
    ) -> io::Result<Self> {
        let workspace = plan
            .0
            .iter()
            .map(|coding| match coding {
                Coding::Gzip => 1024 * 1024,
                Coding::Brotli => 32 * 1024 * 1024,
            })
            .sum::<u64>()
            + ((plan.0.len() + 1) * CHUNK * 2) as u64;
        let workspace = pools.reserve_bytes(workspace).map_err(io::Error::other)?;
        let wire_bytes = Arc::new(AtomicU64::new(0));
        let budget = Arc::new(AtomicUsize::new(CHUNK));
        let mut budgets = vec![budget.clone()];
        let mut stream: ByteStream = Box::pin(WireStream {
            incoming,
            pending: Bytes::new(),
            budget,
            count: wire_bytes.clone(),
        });
        let mut input = wire_bytes.clone();
        for coding in plan.0.into_iter().rev() {
            let reader = StreamReader::new(stream);
            stream = match coding {
                Coding::Gzip => {
                    let mut decoder = GzipDecoder::new(reader);
                    decoder.multiple_members(true);
                    Box::pin(ReaderStream::with_capacity(decoder, CHUNK))
                }
                Coding::Brotli => {
                    let mut decoder = BrotliDecoder::new(reader);
                    decoder.multiple_members(true);
                    Box::pin(ReaderStream::with_capacity(decoder, CHUNK))
                }
            };
            let output = Arc::new(AtomicU64::new(0));
            let budget = Arc::new(AtomicUsize::new(CHUNK));
            budgets.push(budget.clone());
            stream = Box::pin(LayerStream {
                inner: stream,
                input,
                output: output.clone(),
                budget,
            });
            input = output;
        }
        Ok(Self {
            state: Arc::new(Mutex::new(DecodeState {
                stream,
                budgets,
                _workspace: workspace,
            })),
            pools,
            flight: None,
            wire_bytes,
        })
    }

    pub(super) async fn chunk(&mut self) -> io::Result<Option<Bytes>> {
        if self.flight.is_none() {
            self.flight = Some(
                Offloaded::new(
                    ReadChunk {
                        state: self.state.clone(),
                    },
                    self.pools.clone(),
                    PoolKind::Archive,
                    CHUNK as u64,
                )
                .map_err(io::Error::other)?,
            );
        }
        let result = self.flight.as_mut().unwrap().wait().await;
        self.flight = None;
        result?.transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::task::Waker;

    fn body(stream: ByteStream) -> DecodedBody {
        let pools = IoPools::new(7091);
        let workspace = pools.reserve_bytes(1024).unwrap();
        DecodedBody {
            state: Arc::new(Mutex::new(DecodeState {
                stream,
                budgets: Vec::new(),
                _workspace: workspace,
            })),
            pools,
            flight: None,
            wire_bytes: Arc::new(AtomicU64::new(0)),
        }
    }

    #[tokio::test]
    async fn cancelling_a_read_keeps_the_admitted_chunk_and_polls_off_the_caller_thread() {
        let caller = std::thread::current().id();
        let (release, gate) = std::sync::mpsc::channel();
        let stream = futures::stream::once(async move {
            assert_ne!(
                std::thread::current().id(),
                caller,
                "codec polled on foreground thread"
            );
            gate.recv().unwrap();
            Ok(Bytes::from_static(b"retained chunk"))
        });
        let mut body = body(Box::pin(stream));
        let state = Arc::downgrade(&body.state);
        {
            let mut read = Box::pin(body.chunk());
            assert!(futures::poll!(read.as_mut()).is_pending());
        }
        release.send(()).unwrap();
        assert_eq!(body.chunk().await.unwrap().unwrap(), "retained chunk");
        assert_eq!(body.chunk().await.unwrap(), None);
        drop(body);
        assert!(
            state.upgrade().is_none(),
            "decoder workspace retained after closing"
        );
    }

    #[tokio::test]
    async fn pending_input_waits_for_wake_and_a_cancelled_future_can_resume() {
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = polls.clone();
        let ready = Arc::new(AtomicBool::new(false));
        let stream_ready = ready.clone();
        let wake = Arc::new(Mutex::new(None::<Waker>));
        let saved = wake.clone();
        let stream = futures::stream::poll_fn(move |cx| {
            *saved.lock() = Some(cx.waker().clone());
            seen.fetch_add(1, Ordering::Relaxed);
            if stream_ready.load(Ordering::Acquire) {
                Poll::Ready(Some(Ok(Bytes::from_static(b"awake"))))
            } else {
                Poll::Pending
            }
        });
        let mut body = body(Box::pin(stream));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), body.chunk())
                .await
                .is_err()
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            1,
            "pending input caused a busy loop"
        );
        ready.store(true, Ordering::Release);
        wake.lock().take().unwrap().wake();
        assert_eq!(body.chunk().await.unwrap().unwrap(), "awake");
        drop(body);
    }

    #[test]
    fn decoding_policy_keeps_range_bodyless_and_unknown_representations() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, http::HeaderValue::from_static("gzip, br"));
        for status in [
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
            StatusCode::PARTIAL_CONTENT,
        ] {
            assert!(
                codings(&Method::GET, false, status, &headers)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            codings(&Method::HEAD, false, StatusCode::OK, &headers)
                .unwrap()
                .is_none()
        );
        assert!(
            codings(&Method::GET, true, StatusCode::OK, &headers)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            codings(&Method::GET, false, StatusCode::OK, &headers)
                .unwrap()
                .unwrap()
                .0
                .len(),
            2
        );
        headers.insert(
            CONTENT_ENCODING,
            http::HeaderValue::from_static("gzip, future"),
        );
        assert!(
            codings(&Method::GET, false, StatusCode::OK, &headers)
                .unwrap()
                .is_none()
        );
    }
}
