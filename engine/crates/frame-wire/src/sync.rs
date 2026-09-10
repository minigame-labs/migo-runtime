//! The synchronous barrier: the few calls that cannot be answered locally.
//!
//! Almost everything a WebGL or Canvas2D producer asks for can be answered on
//! its own side. Object names are allocated by the producer and written into
//! the command stream without waiting; `getError` is a shadow of state the
//! producer already knows; limits and extensions are fetched once and cached.
//! What is left is the handful of calls whose *result* is the pixels:
//! `readPixels`, `getImageData`, `toDataURL`. Those have to cross, and they
//! have to block, because their return value is the answer.
//!
//! # Shape
//!
//! One request in flight per session, in a fixed-layout record the producer and
//! the host both read. On Apple the record lives in a small `SharedArrayBuffer`
//! that the producer's agent and the relay share -- same process, so the
//! atomics work -- and the reply travels back over the selected transport and
//! is copied into the mailbox by the relay. This module does not know which
//! transport that is, and deliberately: the record and its state machine are
//! the same whichever one G0 selects.
//!
//! # What the rules are for
//!
//! A blocked producer is a producer that will wait exactly as long as it is
//! told to. Every failure mode here ends with the waiter woken and told what
//! happened:
//!
//! - a reply that does not match the request it claims to answer is refused,
//!   because accepting one would hand the producer another call's pixels;
//! - a reply larger than the producer reserved is refused, because the producer
//!   sized the buffer it will read from;
//! - a deadline that passes wakes the waiter with a timeout rather than leaving
//!   it blocked on a reply that is not coming;
//! - teardown wakes every waiter with a failure, because a session that goes
//!   away while a producer is inside `Atomics.wait` leaves that agent blocked
//!   until the process ends.
//!
//! Returning zeros, stale bytes, or a partial buffer is not on that list. A
//! `readPixels` that silently answers with the previous frame's contents is
//! indistinguishable from a correct one until someone screenshots it.

use core::fmt;

/// Fixed, and validated rather than trusted.
pub const SYNC_RECORD_BYTES: u32 = 64;

/// The most a single reply may carry, before the session's own lower cap.
///
/// Large enough for a full-screen `readPixels` at 4× scale (a 1290×2796 phone
/// screen is about 14 MiB of RGBA), and no larger: the reply is copied through
/// a mailbox, and a producer that can name an arbitrary size can name one that
/// does not fit in the memory this lane exists to save.
pub const MAX_REPLY_BYTES: u32 = 16 * 1024 * 1024;

/// How many synchronous requests a session may have outstanding.
///
/// One. A second would need a second mailbox and a second waiter, and the
/// producer is a single agent that is blocked while it waits -- it cannot issue
/// a second request without first returning from the first. Making this a
/// constant rather than a parameter is the point: if it ever becomes two, the
/// protocol needs a request queue, and that is a change worth noticing.
pub const MAX_IN_FLIGHT: u32 = 1;

// Record field offsets. One list, checked against the wire document.
pub(crate) const SYNC_OFF_STATE: usize = 0;
pub(crate) const SYNC_OFF_REQUEST_ID: usize = 4;
pub(crate) const SYNC_OFF_RUNTIME_GENERATION: usize = 8;
pub(crate) const SYNC_OFF_SURFACE_GENERATION: usize = 16;
pub(crate) const SYNC_OFF_RESOURCE_EPOCH: usize = 24;
pub(crate) const SYNC_OFF_TRIGGERING_SEQUENCE: usize = 32;
pub(crate) const SYNC_OFF_OPERATION: usize = 40;
pub(crate) const SYNC_OFF_MAX_REPLY_BYTES: usize = 44;
pub(crate) const SYNC_OFF_REPLY_BYTES: usize = 48;
pub(crate) const SYNC_OFF_ERROR: usize = 52;
pub(crate) const SYNC_OFF_DEADLINE_NANOS: usize = 56;

/// The record, in order, with no gaps. Exported so the wire document and this
/// file can be compared field by field instead of by eye.
pub const SYNC_LAYOUT: &[crate::HeaderField] = &[
    crate::HeaderField {
        offset: SYNC_OFF_STATE as u32,
        size: 4,
        name: "state",
    },
    crate::HeaderField {
        offset: SYNC_OFF_REQUEST_ID as u32,
        size: 4,
        name: "request_id",
    },
    crate::HeaderField {
        offset: SYNC_OFF_RUNTIME_GENERATION as u32,
        size: 8,
        name: "runtime_generation",
    },
    crate::HeaderField {
        offset: SYNC_OFF_SURFACE_GENERATION as u32,
        size: 8,
        name: "surface_generation",
    },
    crate::HeaderField {
        offset: SYNC_OFF_RESOURCE_EPOCH as u32,
        size: 8,
        name: "resource_epoch",
    },
    crate::HeaderField {
        offset: SYNC_OFF_TRIGGERING_SEQUENCE as u32,
        size: 8,
        name: "triggering_sequence",
    },
    crate::HeaderField {
        offset: SYNC_OFF_OPERATION as u32,
        size: 4,
        name: "operation",
    },
    crate::HeaderField {
        offset: SYNC_OFF_MAX_REPLY_BYTES as u32,
        size: 4,
        name: "max_reply_bytes",
    },
    crate::HeaderField {
        offset: SYNC_OFF_REPLY_BYTES as u32,
        size: 4,
        name: "reply_bytes",
    },
    crate::HeaderField {
        offset: SYNC_OFF_ERROR as u32,
        size: 4,
        name: "error",
    },
    crate::HeaderField {
        offset: SYNC_OFF_DEADLINE_NANOS as u32,
        size: 8,
        name: "deadline_nanos",
    },
];

/// Which call the producer blocked in.
///
/// Numbered, not named, and stable: the producer writes one of these into a
/// shared cell and the host dispatches on it. A host that does not implement an
/// operation fails the request with [`SyncError::UnsupportedOperation`] rather
/// than answering it with zeros -- the whole point of this barrier is that the
/// return value IS the answer, so there is no safe default to return.
pub const SYNC_OP_READ_PIXELS: u32 = 1;

/// `readPixels`' arguments, as the producer sends them.
///
/// They are not in the mailbox record. That record is the rendezvous -- a small
/// cell the producer polls with atomics -- and putting per-operation arguments
/// in it would size it by the largest operation anyone ever adds. The arguments
/// travel over the transport beside the request and are decoded here.
///
/// Every field is four bytes, so the record is 32 bytes with no interior
/// padding on LP64 and ILP32 alike: one layout, rather than two that happen to
/// agree today.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadPixelsParams {
    pub canvas_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub type_: u32,
}

/// Serialised size of [`ReadPixelsParams`]. Validated, never trusted.
pub const READ_PIXELS_PARAMS_BYTES: usize = 32;

/// `GL_RGBA`, the only format this host reads back today.
pub const GL_RGBA: u32 = 0x1908;
/// `GL_UNSIGNED_BYTE`, the only type this host reads back today.
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;

impl ReadPixelsParams {
    /// Decode and validate. Refuses rather than clamps, for the reason
    /// [`SyncMailbox::complete`] refuses an oversized reply: a `readPixels`
    /// answered over a rectangle the producer did not ask for is a wrong answer
    /// that looks like a right one.
    pub fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
        if bytes.len() != READ_PIXELS_PARAMS_BYTES {
            return Err(SyncError::UnsupportedOperation);
        }
        let word = |offset: usize| -> u32 {
            u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        let params = Self {
            canvas_id: word(0),
            x: word(4) as i32,
            y: word(8) as i32,
            width: word(12) as i32,
            height: word(16) as i32,
            format: word(20),
            type_: word(24),
        };
        // A zero or negative rectangle has no pixels to return, and a producer
        // that asked for one is not going to read an empty buffer usefully.
        if params.width <= 0 || params.height <= 0 {
            return Err(SyncError::UnsupportedOperation);
        }
        if params.format != GL_RGBA || params.type_ != GL_UNSIGNED_BYTE {
            return Err(SyncError::UnsupportedOperation);
        }
        Ok(params)
    }

    /// How many bytes the answer will be, or `None` if that does not fit in a
    /// `u32` -- which is itself a refusal, not a number to truncate.
    pub fn reply_bytes(&self) -> Option<u32> {
        let width = u32::try_from(self.width).ok()?;
        let height = u32::try_from(self.height).ok()?;
        // RGBA8: four bytes per pixel. Checked, because width*height*4 for a
        // rectangle a producer named can overflow before it is ever refused.
        width.checked_mul(height)?.checked_mul(4)
    }
}

/// Where a request is.
///
/// The numbers are what the producer reads out of a shared cell with an atomic
/// load, so they are stable and never renumbered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SyncState {
    /// No request. The only state a new request may be posted from.
    Free = 0,
    /// Posted, and the producer is waiting.
    Pending = 1,
    /// Answered. `reply_bytes` says how much of the reply buffer is the answer.
    Ready = 2,
    /// Not answered, and will not be. `error` says why.
    Failed = 3,
    /// Withdrawn by the producer before an answer arrived.
    Cancelled = 4,
}

impl SyncState {
    /// Every state, for consumers that must cover all of them.
    ///
    /// Checked against this file's source by
    /// `tests/wire_document_agreement.rs`: a variant added without being
    /// listed here breaks that test rather than quietly escaping every
    /// consumer that iterates this.
    pub const ALL: &'static [SyncState] = &[
        Self::Free,
        Self::Pending,
        Self::Ready,
        Self::Failed,
        Self::Cancelled,
    ];

    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }

    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Free),
            1 => Some(Self::Pending),
            2 => Some(Self::Ready),
            3 => Some(Self::Failed),
            4 => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether a waiter should stop waiting.
    #[inline]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Cancelled)
    }
}

/// Why a synchronous request failed.
///
/// Stable, and carried to the producer, which reports them as exceptions. Never
/// renumber; only append.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SyncError {
    /// A request was posted while another was outstanding.
    AlreadyPending = 1,
    /// The reply named a request that is not the one outstanding.
    RequestIdMismatch = 2,
    /// The reply was built against a generation or epoch that has since moved.
    StaleGeneration = 3,
    /// The reply is larger than the producer reserved room for.
    ReplyTooLarge = 4,
    /// The deadline passed with no reply.
    TimedOut = 5,
    /// The session went away while the producer was waiting.
    SessionEnded = 6,
    /// The operation is not one this host implements.
    UnsupportedOperation = 7,
    /// A reply arrived for a request that was already settled.
    LateReply = 8,
    /// The request named a deadline in the past, or none at all.
    BadDeadline = 9,
    /// The request reserved more reply room than the protocol allows.
    BadReplyReservation = 10,
    /// The host implements the operation, tried it, and it failed.
    ///
    /// Distinct from [`Self::UnsupportedOperation`] because the two say
    /// opposite things about whether to ask again. "This host does not do
    /// readPixels" is permanent, and a producer told that will stop asking;
    /// "the readback failed this time" is not. Mapping a driver error onto the
    /// permanent one would turn one transient GL failure into a session that
    /// never reads a pixel again.
    OperationFailed = 11,
}

impl SyncError {
    /// Every failure, for the C ABI mirror and its coverage test.
    ///
    /// Checked against this file's source by
    /// `tests/wire_document_agreement.rs`: a variant added without being
    /// listed here breaks that test rather than quietly escaping every
    /// consumer that iterates this.
    pub const ALL: &'static [SyncError] = &[
        Self::AlreadyPending,
        Self::RequestIdMismatch,
        Self::StaleGeneration,
        Self::ReplyTooLarge,
        Self::TimedOut,
        Self::SessionEnded,
        Self::UnsupportedOperation,
        Self::LateReply,
        Self::BadDeadline,
        Self::BadReplyReservation,
        Self::OperationFailed,
    ];

    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyPending => "a synchronous request is already outstanding",
            Self::RequestIdMismatch => "the reply does not answer the outstanding request",
            Self::StaleGeneration => "the reply was built against a generation that has moved",
            Self::ReplyTooLarge => "the reply is larger than the producer reserved",
            Self::TimedOut => "the deadline passed with no reply",
            Self::SessionEnded => "the session ended while the producer was waiting",
            Self::UnsupportedOperation => "this host does not implement that operation",
            Self::LateReply => "the reply arrived after the request was settled",
            Self::BadDeadline => "the deadline is not in the future",
            Self::BadReplyReservation => "the reserved reply size is outside the protocol's bounds",
            Self::OperationFailed => "the host tried the operation and it failed",
        })
    }
}

/// One synchronous request, as both sides see it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncRequest {
    pub request_id: u32,
    pub runtime_generation: u64,
    pub surface_generation: u64,
    pub resource_epoch: u64,
    /// The frame the producer had submitted when it blocked. A reply is only
    /// meaningful after that frame has been executed, and recording it is what
    /// lets the host say so rather than guess.
    pub triggering_sequence: u64,
    pub operation: u32,
    pub max_reply_bytes: u32,
    /// Monotonic nanoseconds, on the host's clock. Not wall time: a producer
    /// that blocked across a clock adjustment would otherwise wake early or
    /// never.
    pub deadline_nanos: u64,
}

/// The single-slot mailbox, and the rules that make a blocked producer safe.
///
/// Host-side. The producer's half is a few atomic operations on the shared cell
/// this describes; keeping the rules here means there is one place they are
/// written down, and the producer's implementation is checked against the same
/// document rather than against a second reading of it.
#[derive(Debug)]
pub struct SyncMailbox {
    runtime_generation: u64,
    state: SyncState,
    request: Option<SyncRequest>,
    error: Option<SyncError>,
    reply_bytes: u32,
    next_request_id: u32,
    /// Set once the session is going away. Every later post fails immediately
    /// rather than blocking a producer that nothing will answer.
    ended: bool,
}

impl SyncMailbox {
    pub fn new(runtime_generation: u64) -> Self {
        Self {
            runtime_generation,
            state: SyncState::Free,
            request: None,
            error: None,
            reply_bytes: 0,
            next_request_id: 1,
            ended: false,
        }
    }

    #[inline]
    pub const fn state(&self) -> SyncState {
        self.state
    }

    #[inline]
    pub const fn error(&self) -> Option<SyncError> {
        self.error
    }

    #[inline]
    pub const fn reply_bytes(&self) -> u32 {
        self.reply_bytes
    }

    #[inline]
    pub const fn request(&self) -> Option<SyncRequest> {
        self.request
    }

    /// The id the next accepted request will carry.
    ///
    /// Monotonic and never zero: zero is the value a cleared mailbox holds, so
    /// a reply that arrives with id zero is a reply to nothing rather than a
    /// reply to whatever happens to be outstanding.
    #[inline]
    pub const fn next_request_id(&self) -> u32 {
        self.next_request_id
    }

    /// Post a request. Fails without blocking anything if it cannot be accepted.
    pub fn post(&mut self, request: SyncRequest, now_nanos: u64) -> Result<u32, SyncError> {
        if self.ended {
            return Err(SyncError::SessionEnded);
        }
        if self.state == SyncState::Pending {
            return Err(SyncError::AlreadyPending);
        }
        if request.runtime_generation != self.runtime_generation {
            return Err(SyncError::StaleGeneration);
        }
        if request.max_reply_bytes == 0 || request.max_reply_bytes > MAX_REPLY_BYTES {
            return Err(SyncError::BadReplyReservation);
        }
        if request.deadline_nanos <= now_nanos {
            return Err(SyncError::BadDeadline);
        }

        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.state = SyncState::Pending;
        self.request = Some(SyncRequest {
            request_id: id,
            ..request
        });
        self.error = None;
        self.reply_bytes = 0;
        Ok(id)
    }

    /// Deliver a reply. The bytes themselves are the caller's; this decides
    /// whether they may be handed to the producer at all.
    pub fn complete(&mut self, request_id: u32, reply_bytes: u32) -> Result<(), SyncError> {
        let Some(request) = self.request else {
            return Err(SyncError::LateReply);
        };
        if self.state != SyncState::Pending {
            // Settled already -- timed out, cancelled, or failed. The producer
            // has moved on and its reply buffer may be someone else's now.
            return Err(SyncError::LateReply);
        }
        if request_id != request.request_id {
            self.fail(SyncError::RequestIdMismatch);
            return Err(SyncError::RequestIdMismatch);
        }
        if reply_bytes > request.max_reply_bytes {
            // Fails the request rather than truncating. A truncated
            // `readPixels` is a wrong answer that looks like a right one.
            self.fail(SyncError::ReplyTooLarge);
            return Err(SyncError::ReplyTooLarge);
        }
        self.state = SyncState::Ready;
        self.reply_bytes = reply_bytes;
        self.error = None;
        Ok(())
    }

    /// The deadline passed. Returns whether this call is what settled it.
    pub fn expire_if_due(&mut self, now_nanos: u64) -> bool {
        if self.state != SyncState::Pending {
            return false;
        }
        let Some(request) = self.request else {
            return false;
        };
        if now_nanos < request.deadline_nanos {
            return false;
        }
        self.fail(SyncError::TimedOut);
        true
    }

    /// The producer withdrew the request.
    pub fn cancel(&mut self) -> bool {
        if self.state != SyncState::Pending {
            return false;
        }
        self.state = SyncState::Cancelled;
        self.error = None;
        true
    }

    /// A generation or epoch moved under an outstanding request.
    pub fn invalidate(&mut self) -> bool {
        if self.state != SyncState::Pending {
            return false;
        }
        self.fail(SyncError::StaleGeneration);
        true
    }

    /// The session is going away.
    ///
    /// Every outstanding request is failed, and every later one is refused. A
    /// producer inside `Atomics.wait` on a session that has gone is blocked
    /// until its agent is destroyed, which on iOS means until WebKit reclaims
    /// the process.
    pub fn end_session(&mut self) -> bool {
        self.ended = true;
        if self.state != SyncState::Pending {
            return false;
        }
        self.fail(SyncError::SessionEnded);
        true
    }

    /// The host cannot answer this request, and says which request and why.
    ///
    /// Separate from [`Self::complete`] rather than a `Result` variant of it,
    /// because the two carry different obligations: `complete` is the caller
    /// asserting it HAS an answer and the mailbox deciding whether the producer
    /// may have it, while this is the caller stating there will not be one. The
    /// id is matched the same way either way -- a failure recorded against a
    /// request that is no longer outstanding would wake a producer with another
    /// call's verdict.
    pub fn fail_request(&mut self, request_id: u32, error: SyncError) -> Result<(), SyncError> {
        let Some(request) = self.request else {
            return Err(SyncError::LateReply);
        };
        if self.state != SyncState::Pending {
            return Err(SyncError::LateReply);
        }
        if request_id != request.request_id {
            return Err(SyncError::RequestIdMismatch);
        }
        self.fail(error);
        Ok(())
    }

    /// The producer has read the answer; the slot is reusable.
    pub fn acknowledge(&mut self) {
        if self.state.is_settled() {
            self.state = SyncState::Free;
            self.request = None;
            self.error = None;
            self.reply_bytes = 0;
        }
    }

    fn fail(&mut self, error: SyncError) {
        self.state = SyncState::Failed;
        self.error = Some(error);
        self.reply_bytes = 0;
    }
}
