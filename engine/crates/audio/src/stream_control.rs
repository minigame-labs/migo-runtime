//! Bounded source repositions shared by codecs and the audio-thread consumer.
//! A public seek must not clear PCM until its source has prepared a new revision.
use crate::streaming::{
    STREAM_CHANNEL_CAPACITY, StreamRevision, StreamTimeline, StreamTraversal, StreamingState,
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

pub(crate) struct Command {
    pub target: i64,
    pub cutover: Option<LoopRequest>,
    pub deadline: Instant,
    pub reply: oneshot::Sender<Reply>,
}
pub(crate) enum Reply {
    Rejected(ErrorCode),
    NotStarted(ErrorCode),
    Prepared(Prepared),
}
pub(crate) struct Prepared {
    pub revision: StreamRevision,
    pub target: i64,
    pub traversal: Option<StreamTraversal>,
    pub source: Arc<StreamingState>,
    pub deadline: Instant,
    pub accept: oneshot::Sender<()>,
}
impl Prepared {
    pub(crate) fn mapped_output(&self) -> EngineResult<u64> {
        if let Some(traversal) = self.traversal {
            if self.source.timeline() != Some(traversal.presentation().timeline()) {
                return Err(invalid("traversal changes its source clock"));
            }
            return Ok(traversal.output_start());
        }
        let timeline = self
            .source
            .timeline()
            .ok_or_else(|| invalid("source has no output clock"))?;
        let target =
            u64::try_from(self.target).map_err(|_| invalid("negative accepted source position"))?;
        let position = timeline.position(target)?;
        Ok(position.frame + u64::from(position.phase != 0))
    }
}
fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message)
}
pub(crate) fn channel() -> (mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    mpsc::channel(STREAM_CHANNEL_CAPACITY)
}
#[cfg(test)]
pub(crate) fn request(
    tx: &mpsc::Sender<Command>,
    target: i64,
    deadline: Instant,
) -> EngineResult<oneshot::Receiver<Reply>> {
    request_as(tx, target, None, deadline)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoopRequest {
    pub output_start: u64,
    pub enabled: bool,
    pub seek_target: Option<u64>,
}
impl LoopRequest {
    pub(crate) fn traversal(self, clock: StreamTimeline) -> EngineResult<StreamTraversal> {
        let traversal = if let Some(target) = self.seek_target {
            StreamTraversal::seek(clock, target, self.enabled)?
        } else {
            StreamTraversal::cutover(clock, self.output_start, self.enabled)?
        };
        if traversal.output_start() != self.output_start {
            return Err(invalid("seek request changes its source output mapping"));
        }
        Ok(traversal)
    }
}
pub(crate) fn request_as(
    tx: &mpsc::Sender<Command>,
    target: i64,
    cutover: Option<LoopRequest>,
    deadline: Instant,
) -> EngineResult<oneshot::Receiver<Reply>> {
    let (reply, result) = oneshot::channel();
    tx.try_send(Command {
        target,
        cutover,
        deadline,
        reply,
    })
    .map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => {
            EngineError::from_detail(ErrorCode::InputSaturated, "source control queue full")
        }
        mpsc::error::TrySendError::Closed(_) => invalid("source control queue closed"),
    })?;
    Ok(result)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Seek,
    /// A fresh playback cannot fall back to PCM from a different position.
    Start,
    Restart,
    Loop(bool),
}
pub(crate) struct Pending {
    pub reply: oneshot::Receiver<Reply>,
    pub kind: Kind,
    pub deadline: Instant,
    pub target: i64,
    pub cutover: Option<LoopRequest>,
}
/// Fixed storage: the mixer never grows a control backlog or allocates a queue.
pub(crate) struct PendingControls {
    slots: [Option<Pending>; STREAM_CHANNEL_CAPACITY],
    first: usize,
    len: usize,
}
impl PendingControls {
    pub(crate) fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            first: 0,
            len: 0,
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(crate) fn is_full(&self) -> bool {
        self.len == self.slots.len()
    }
    pub(crate) fn has_restart(&self) -> bool {
        self.slots.iter().flatten().any(|p| p.kind == Kind::Restart)
    }
    pub(crate) fn desired_loop(&self, fallback: bool) -> bool {
        (0..self.len)
            .rev()
            .find_map(|i| {
                self.slots[(self.first + i) % self.slots.len()]
                    .as_ref()
                    .and_then(|p| p.cutover)
                    .map(|r| r.enabled)
            })
            .unwrap_or(fallback)
    }
    pub(crate) fn has_cutover(&self) -> bool {
        self.slots.iter().flatten().any(|p| p.cutover.is_some())
    }
    pub(crate) fn push(&mut self, pending: Pending) {
        assert!(!self.is_full());
        self.slots[(self.first + self.len) % STREAM_CHANNEL_CAPACITY] = Some(pending);
        self.len += 1;
    }
    pub(crate) fn front(&mut self) -> Option<&mut Pending> {
        if self.is_empty() {
            None
        } else {
            self.slots[self.first].as_mut()
        }
    }
    pub(crate) fn pop(&mut self) -> Option<Pending> {
        if self.is_empty() {
            return None;
        }
        let pending = self.slots[self.first].take();
        self.first = (self.first + 1) % STREAM_CHANNEL_CAPACITY;
        self.len -= 1;
        pending
    }
    pub(crate) fn clear(&mut self) {
        while self.pop().is_some() {}
    }
}
