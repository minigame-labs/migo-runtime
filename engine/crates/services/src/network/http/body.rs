//! A request's body on its way out, and a response's on its way in -- each measured as it passes.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use futures::Stream;
use http_body::{Frame, SizeHint};

pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

/// What a request sends.
pub struct Body {
    kind: Kind,
}

enum Kind {
    Empty,
    Full(Bytes),
    Stream {
        stream: ByteStream,
        length: Option<u64>,
    },
}

impl Body {
    pub fn empty() -> Self {
        Self { kind: Kind::Empty }
    }

    /// A body read from `stream` as it is sent. `length`, when known, is declared as `Content-Length`; otherwise the
    /// body is sent chunked.
    pub fn wrap_stream(
        stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
        length: Option<u64>,
    ) -> Self {
        Self {
            kind: Kind::Stream {
                stream: Box::pin(stream),
                length,
            },
        }
    }

    /// The length, where it is known before sending.
    pub fn length(&self) -> Option<u64> {
        match &self.kind {
            Kind::Empty => Some(0),
            Kind::Full(bytes) => Some(bytes.len() as u64),
            Kind::Stream { length, .. } => *length,
        }
    }

    /// The same body again, for a redirect that resends it -- possible only for one held whole.
    pub(crate) fn try_clone(&self) -> Option<Self> {
        match &self.kind {
            Kind::Empty => Some(Self::empty()),
            Kind::Full(bytes) => Some(Self::from(bytes.clone())),
            Kind::Stream { .. } => None,
        }
    }

    pub(crate) fn sending(self, progress: Arc<SendProgress>) -> SentBody {
        SentBody {
            kind: self.kind,
            progress,
        }
    }
}

impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        Self {
            kind: Kind::Full(bytes),
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from(Bytes::from(bytes))
    }
}

/// How far a request's body has gone out: bytes written, and when the last one was.
#[derive(Debug, Default)]
pub(crate) struct SendProgress {
    bytes: AtomicU64,
    ended: OnceLock<Instant>,
}

impl SendProgress {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn ended(&self) -> Option<Instant> {
        self.ended.get().copied()
    }

    fn end(&self) {
        let _ = self.ended.set(Instant::now());
    }
}

/// The body as hyper polls it.
pub(crate) struct SentBody {
    kind: Kind,
    progress: Arc<SendProgress>,
}

impl http_body::Body for SentBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = &mut *self;
        let next = match &mut this.kind {
            Kind::Empty => None,
            Kind::Full(bytes) => {
                let bytes = std::mem::take(bytes);
                this.kind = Kind::Empty;
                (!bytes.is_empty()).then_some(Ok(bytes))
            }
            Kind::Stream { stream, .. } => match stream.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(next) => next,
            },
        };
        match next {
            Some(Ok(bytes)) => {
                this.progress
                    .bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Some(Err(error)) => Poll::Ready(Some(Err(error))),
            None => {
                this.progress.end();
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        let ended = match &self.kind {
            Kind::Empty => true,
            Kind::Full(bytes) => bytes.is_empty(),
            Kind::Stream { .. } => false,
        };
        if ended {
            // hyper does not poll a body that says it has ended, so this is where an empty one ends.
            self.progress.end();
        }
        ended
    }

    fn size_hint(&self) -> SizeHint {
        match &self.kind {
            Kind::Empty => SizeHint::with_exact(0),
            Kind::Full(bytes) => SizeHint::with_exact(bytes.len() as u64),
            Kind::Stream {
                length: Some(length),
                ..
            } => SizeHint::with_exact(*length),
            Kind::Stream { length: None, .. } => SizeHint::default(),
        }
    }
}
