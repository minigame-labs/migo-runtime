//! Read-only audio capability crossing the service/audio dependency boundary.
//! Implementations own pinned bytes, original IO admission and domain lifetime.
use crate::error::{EngineError, EngineResult};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;
use tokio::sync::{Notify, oneshot};

pub type AudioSpoolFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<T>> + Send + 'a>>;

/// One already-open full HTTP response, including every consumed prefix byte.
/// The producer owns network policy and the original response's byte admission.
pub trait AudioSpoolBody: Send {
    fn length(&self) -> Option<u64>;
    fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<AudioFileChunk>>;
}

/// Session-private Fs authority. No host pathname crosses into the audio queue.
/// Success exposes only the existing sealed, read-only AudioFile capability.
pub trait AudioHttpSpool: Send + Sync {
    fn prepare(
        self: Arc<Self>,
        body: Box<dyn AudioSpoolBody>,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: Instant,
    ) -> AudioSpoolFuture<'static, Arc<dyn AudioFile>>;
}

/// A single immutable loan. Its owner may retain the file and physical credit;
/// moving the loan into a CPU step never copies or releases its backing bytes.
pub struct AudioFileChunk(Box<dyn AsRef<[u8]> + Send>);
impl AudioFileChunk {
    pub fn new(owner: impl AsRef<[u8]> + Send + 'static) -> Self {
        Self(Box::new(owner))
    }
}
impl AsRef<[u8]> for AudioFileChunk {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().as_ref()
    }
}
pub type AudioFileRead = oneshot::Receiver<std::thread::Result<EngineResult<AudioFileChunk>>>;

pub trait AudioFileCancel: Send + Sync {
    fn is_cancelled(&self) -> bool;
}
impl AudioFileCancel for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub trait AudioFile: Send + Sync {
    fn length(&self) -> u64;
    /// Captured on Fs from the same immutable object, never a pathname probe.
    fn prefix(&self) -> &[u8];
    /// Bounded source storage retained by a queued command. IO owns its actual
    /// physical credit separately; this is the transport's backlog allowance.
    fn queued_bytes(&self) -> usize;
    fn is_closed(&self) -> bool;
    /// Register and enable before checking is_closed(), as for AudioLoadWatch.
    fn closed_signal(&self) -> &Notify;
    /// Enqueue one bounded read. The original source and its credit must survive
    /// a dropped receiver until the queued/running IO job really returns.
    fn read_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn AudioFileCancel>,
        deadline: Instant,
    ) -> Result<AudioFileRead, EngineError>;
    /// One bounded container index. Admission precedes allocation; the returned
    /// owner retains its exact storage credit through the final CPU borrow.
    fn read_control_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn AudioFileCancel>,
        deadline: Instant,
    ) -> Result<AudioFileRead, EngineError>;
}
