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

/// Wait for the window to open, and say what it is.
///
/// A producer whose calls are synchronous cannot wait for a credit the way a
/// running one does -- on the next advertisement to arrive -- because its agent
/// is inside a GL call and nothing arrives until it returns. It meets this when
/// it has to send a barrier (see `FLAG_PRESENT`) while the renderer holds every
/// credit: before a synchronous query, or when a frame outgrows one packet. So
/// it asks here, blocked, and the host answers once
///
/// 1. every packet through `triggering_sequence` -- the last one the producer
///    sent -- has been admitted, and
/// 2. at least one credit is free,
///
/// with the advertisement read at that moment ([`WindowReply`]). No parameters;
/// the reply is [`WINDOW_REPLY_BYTES`]. The deadline bounds the wait like any
/// other request: a renderer that never finishes a frame is a `TIMED_OUT`, not
/// a producer blocked for good.
pub const SYNC_OP_AWAIT_WINDOW: u32 = 2;

/// A WebGL query whose answer is one number.
///
/// `getProgramParameter`, `getShaderParameter`, `getUniformLocation`,
/// `checkFramebufferStatus`, `getError` and the rest: the calls a WebGL program
/// makes between recording work and drawing with it, whose return value is the
/// answer and for which there is no safe default. Which query is in the
/// parameters ([`GlQuery`]), not the operation, because the operation is what
/// sizes the reply -- a host that answered a location and an info log through
/// one code would have to guess a reply size for both.
///
/// Reply: four bytes, little-endian, read as `i32` or `u32` by the query.
pub const SYNC_OP_GL_QUERY_SCALAR: u32 = 3;

/// A WebGL query whose answer is text: the info logs, and `getParameter` for
/// the strings a context reports about itself.
///
/// Reply: the UTF-8 bytes, and nothing else -- the request already carries how
/// many there are.
pub const SYNC_OP_GL_QUERY_TEXT: u32 = 4;

/// A WebGL query whose answer describes an active variable:
/// `getActiveAttrib`, `getActiveUniform`, `getTransformFeedbackVarying`.
///
/// Reply: `size:i32`, `type:u32`, then the name's UTF-8 bytes. A reply of
/// exactly [`ACTIVE_VARIABLE_HEADER_BYTES`] with an empty name is the answer for
/// an index the program does not have, which WebGL returns as `null`; the
/// producer distinguishes the two by the `type` being zero.
pub const SYNC_OP_GL_QUERY_ACTIVE: u32 = 5;

/// `size` and `type` before an active variable's name.
pub const ACTIVE_VARIABLE_HEADER_BYTES: usize = 8;

/// Which WebGL query a [`GlQueryParams`] asks.
///
/// Numbered and stable, like an opcode: the producer writes one of these and
/// the host dispatches on it, and nothing between them is typed.
pub mod gl_query {
    /// `getProgramParameter(program, pname)` -- `object` is the program.
    pub const PROGRAM_PARAMETER: u32 = 1;
    /// `getShaderParameter(shader, pname)` -- `object` is the shader.
    pub const SHADER_PARAMETER: u32 = 2;
    /// `getQueryParameter(query, pname)` -- `object` is the query object.
    pub const QUERY_PARAMETER: u32 = 3;
    /// `checkFramebufferStatus(target)` -- `pname` is the target.
    pub const CHECK_FRAMEBUFFER_STATUS: u32 = 4;
    /// `clientWaitSync(sync, flags, timeout)` -- `object` is the sync object,
    /// `pname` the flags, `extra` the timeout in milliseconds.
    pub const CLIENT_WAIT_SYNC: u32 = 5;
    /// `getError()`, answered from the errors the host recorded while decoding
    /// this producer's records -- there is no round trip to the renderer,
    /// because the error queue is the host's.
    pub const GET_ERROR: u32 = 6;
    /// `getUniformLocation(program, name)`; -1 when there is none.
    pub const UNIFORM_LOCATION: u32 = 7;
    /// `getAttribLocation(program, name)`; -1 when there is none.
    pub const ATTRIB_LOCATION: u32 = 8;
    /// `getUniformBlockIndex(program, name)`; `INVALID_INDEX` when there is none.
    pub const UNIFORM_BLOCK_INDEX: u32 = 9;
    /// `getProgramInfoLog(program)`.
    pub const PROGRAM_INFO_LOG: u32 = 10;
    /// `getShaderInfoLog(shader)`.
    pub const SHADER_INFO_LOG: u32 = 11;
    /// `getParameter(pname)`, whose answer this host renders as text.
    pub const PARAMETER: u32 = 12;
    /// `getActiveAttrib(program, index)` -- `pname` is the index.
    pub const ACTIVE_ATTRIB: u32 = 13;
    /// `getActiveUniform(program, index)`.
    pub const ACTIVE_UNIFORM: u32 = 14;
    /// `getTransformFeedbackVarying(program, index)`.
    pub const TRANSFORM_FEEDBACK_VARYING: u32 = 15;

    /// Whether a kind is one this build knows. A query nobody implements is
    /// [`super::SyncError::UnsupportedOperation`], never an answer of zero.
    pub fn is_known(kind: u32) -> bool {
        (PROGRAM_PARAMETER..=TRANSFORM_FEEDBACK_VARYING).contains(&kind)
    }
}

/// The arguments of every WebGL query, in one shape.
///
/// Six words and a name, because that is the union of what fifteen queries
/// take: an object id, a `pname` or an index, one more number for
/// `clientWaitSync`'s timeout, and a name for the three that look one up. The
/// name is UTF-8, its length is a byte count, and the bytes are padded to a word
/// with zeros -- the frame stream's payload rule, for the same reason: one
/// encoding of a given request rather than four.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlQueryParams<'a> {
    pub kind: u32,
    pub canvas_id: u32,
    pub object: u32,
    pub pname: u32,
    pub extra: u32,
    pub name: &'a [u8],
}

/// Words before a [`GlQueryParams`]'s name: the five fields above and the
/// name's byte length.
pub const GL_QUERY_HEADER_BYTES: usize = 24;

/// The longest name a query may carry. A GLSL identifier is bounded by the
/// shader it came from; this is far above any real one and far below the body
/// ceiling, so a name that reaches it is a producer bug rather than a program.
pub const GL_QUERY_MAX_NAME_BYTES: usize = 1024;

impl<'a> GlQueryParams<'a> {
    /// Encode into a fresh buffer, which is what a test or a host-side producer
    /// needs; the producer proper writes these bytes in JavaScript.
    pub fn encode(&self) -> Vec<u8> {
        let padded = self.name.len().div_ceil(4) * 4;
        let mut out = Vec::with_capacity(GL_QUERY_HEADER_BYTES + padded);
        for word in [
            self.kind,
            self.canvas_id,
            self.object,
            self.pname,
            self.extra,
            self.name.len() as u32,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(self.name);
        out.resize(GL_QUERY_HEADER_BYTES + padded, 0);
        out
    }

    /// Decode and validate. Refuses a kind it does not know, a length that
    /// disagrees with the body, and padding that is not zero.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, SyncError> {
        if bytes.len() < GL_QUERY_HEADER_BYTES {
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
        let kind = word(0);
        if !gl_query::is_known(kind) {
            return Err(SyncError::UnsupportedOperation);
        }
        let name_len = word(20) as usize;
        if name_len > GL_QUERY_MAX_NAME_BYTES {
            return Err(SyncError::UnsupportedOperation);
        }
        let padded = name_len.div_ceil(4) * 4;
        if bytes.len() != GL_QUERY_HEADER_BYTES + padded {
            return Err(SyncError::UnsupportedOperation);
        }
        if bytes[GL_QUERY_HEADER_BYTES + name_len..]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(SyncError::UnsupportedOperation);
        }
        Ok(Self {
            kind,
            canvas_id: word(4),
            object: word(8),
            pname: word(12),
            extra: word(16),
            name: &bytes[GL_QUERY_HEADER_BYTES..GL_QUERY_HEADER_BYTES + name_len],
        })
    }

    /// The name as text, with lone surrogates replaced -- the conversion V8
    /// makes for a `#[string]` argument, so a name that crossed as a query and
    /// one that crossed as an op name the same variable.
    pub fn name_str(&self) -> std::borrow::Cow<'a, str> {
        String::from_utf8_lossy(self.name)
    }

    /// Which operation answers this kind, and therefore what shape its reply is.
    pub fn operation(kind: u32) -> u32 {
        match kind {
            gl_query::PROGRAM_INFO_LOG | gl_query::SHADER_INFO_LOG | gl_query::PARAMETER => {
                SYNC_OP_GL_QUERY_TEXT
            }
            gl_query::ACTIVE_ATTRIB
            | gl_query::ACTIVE_UNIFORM
            | gl_query::TRANSFORM_FEEDBACK_VARYING => SYNC_OP_GL_QUERY_ACTIVE,
            _ => SYNC_OP_GL_QUERY_SCALAR,
        }
    }
}

/// Serialised size of [`WindowReply`].
pub const WINDOW_REPLY_BYTES: usize = 16;

/// The window, as `SYNC_OP_AWAIT_WINDOW` answers it: the same two numbers a
/// frame verdict or a clock tick carries, with the same meaning -- "having
/// accepted every packet through `accepted_sequence`, this many credits were
/// free".
///
/// `remaining_credits` u32, a reserved u32 that is zero, `accepted_sequence`
/// u64, little-endian: the sequence at its full width, where it is naturally
/// aligned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowReply {
    pub remaining_credits: u32,
    pub accepted_sequence: u64,
}

impl WindowReply {
    pub fn encode(&self) -> [u8; WINDOW_REPLY_BYTES] {
        let mut out = [0u8; WINDOW_REPLY_BYTES];
        out[0..4].copy_from_slice(&self.remaining_credits.to_le_bytes());
        out[8..16].copy_from_slice(&self.accepted_sequence.to_le_bytes());
        out
    }

    /// Decode and validate: exactly [`WINDOW_REPLY_BYTES`], reserved word zero.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != WINDOW_REPLY_BYTES || bytes[4..8] != [0; 4] {
            return None;
        }
        Some(Self {
            remaining_credits: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            accepted_sequence: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
        })
    }
}

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

/// A synchronous request carried whole in one request body.
///
/// The mailbox record above is a cell two agents share, which needs
/// `SharedArrayBuffer`, and the Apple lane's content origin is a custom scheme
/// on which WebKit does not isolate the page: G0 measured `SharedArrayBuffer is
/// not a constructor` there. What that origin does have is a synchronous request
/// from a Worker. So the same request travels as a body, and the answer as the
/// response -- see [`SyncAnswer`].
///
/// It names no deadline. The mailbox's `deadline_nanos` is on the host's clock,
/// which a producer in another process cannot read; this carries how long the
/// producer is prepared to wait instead, and the host turns that into a deadline
/// on its own clock.
///
/// The layout is [`SYNC_CALL_LAYOUT`], checked against
/// `contracts/frame-wire/wire-v1.md` ("A request as one body").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncCall<'a> {
    pub runtime_generation: u64,
    pub surface_generation: u64,
    pub resource_epoch: u64,
    pub triggering_sequence: u64,
    pub operation: u32,
    pub max_reply_bytes: u32,
    pub timeout_millis: u32,
    pub params: &'a [u8],
}

/// Bytes before a [`SyncCall`]'s arguments.
pub const SYNC_CALL_HEADER_BYTES: usize = 48;

/// The largest body a [`SyncCall`] may be, arguments included.
///
/// Arguments are small by construction -- `readPixels` takes 32 bytes, and
/// anything bulky travels as a frame -- so this is a bound on what a producer
/// can make a transport assemble, not a guess at traffic. It is a constant of
/// the format rather than of a transport so that every transport refuses the
/// same bodies, and refuses them before reading them.
pub const SYNC_CALL_MAX_BYTES: usize = 4096;

/// The longest a producer may ask to wait. A minute: longer than any readback
/// a device can take, and short enough that a producer blocked on a host that
/// will never answer is released while its user is still looking at the screen.
pub const SYNC_CALL_MAX_TIMEOUT_MILLIS: u32 = 60_000;

const SYNC_CALL_OFF_RUNTIME_GENERATION: usize = 0;
const SYNC_CALL_OFF_SURFACE_GENERATION: usize = 8;
const SYNC_CALL_OFF_RESOURCE_EPOCH: usize = 16;
const SYNC_CALL_OFF_TRIGGERING_SEQUENCE: usize = 24;
const SYNC_CALL_OFF_OPERATION: usize = 32;
const SYNC_CALL_OFF_MAX_REPLY_BYTES: usize = 36;
const SYNC_CALL_OFF_TIMEOUT_MILLIS: usize = 40;
const SYNC_CALL_OFF_RESERVED: usize = 44;

/// The call body's fixed part, in order, with no gaps.
pub const SYNC_CALL_LAYOUT: &[crate::HeaderField] = &[
    crate::HeaderField {
        offset: SYNC_CALL_OFF_RUNTIME_GENERATION as u32,
        size: 8,
        name: "runtime_generation",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_SURFACE_GENERATION as u32,
        size: 8,
        name: "surface_generation",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_RESOURCE_EPOCH as u32,
        size: 8,
        name: "resource_epoch",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_TRIGGERING_SEQUENCE as u32,
        size: 8,
        name: "triggering_sequence",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_OPERATION as u32,
        size: 4,
        name: "operation",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_MAX_REPLY_BYTES as u32,
        size: 4,
        name: "max_reply_bytes",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_TIMEOUT_MILLIS as u32,
        size: 4,
        name: "timeout_millis",
    },
    crate::HeaderField {
        offset: SYNC_CALL_OFF_RESERVED as u32,
        size: 4,
        name: "reserved",
    },
];

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(word)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(word)
}

impl<'a> SyncCall<'a> {
    /// Decode and validate the envelope. The arguments are the operation's to
    /// judge; they are borrowed, not copied.
    ///
    /// A malformed envelope is `UnsupportedOperation`, the same answer a
    /// malformed argument record gets: neither is something the producer can
    /// fix by asking again, and neither is a transient failure of the host.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, SyncError> {
        if bytes.len() < SYNC_CALL_HEADER_BYTES || bytes.len() > SYNC_CALL_MAX_BYTES {
            return Err(SyncError::UnsupportedOperation);
        }
        // Zero now so a later version can give the word a meaning without an
        // older host reading it as this version's.
        if u32_at(bytes, SYNC_CALL_OFF_RESERVED) != 0 {
            return Err(SyncError::UnsupportedOperation);
        }
        let timeout_millis = u32_at(bytes, SYNC_CALL_OFF_TIMEOUT_MILLIS);
        if timeout_millis == 0 || timeout_millis > SYNC_CALL_MAX_TIMEOUT_MILLIS {
            return Err(SyncError::BadDeadline);
        }
        Ok(Self {
            runtime_generation: u64_at(bytes, SYNC_CALL_OFF_RUNTIME_GENERATION),
            surface_generation: u64_at(bytes, SYNC_CALL_OFF_SURFACE_GENERATION),
            resource_epoch: u64_at(bytes, SYNC_CALL_OFF_RESOURCE_EPOCH),
            triggering_sequence: u64_at(bytes, SYNC_CALL_OFF_TRIGGERING_SEQUENCE),
            operation: u32_at(bytes, SYNC_CALL_OFF_OPERATION),
            max_reply_bytes: u32_at(bytes, SYNC_CALL_OFF_MAX_REPLY_BYTES),
            timeout_millis,
            params: &bytes[SYNC_CALL_HEADER_BYTES..],
        })
    }

    /// The mailbox request this call is, with its deadline on the host's clock.
    pub fn request(&self, now_nanos: u64) -> SyncRequest {
        SyncRequest {
            request_id: 0,
            runtime_generation: self.runtime_generation,
            surface_generation: self.surface_generation,
            resource_epoch: self.resource_epoch,
            triggering_sequence: self.triggering_sequence,
            operation: self.operation,
            max_reply_bytes: self.max_reply_bytes,
            deadline_nanos: now_nanos
                .saturating_add(u64::from(self.timeout_millis).saturating_mul(1_000_000)),
        }
    }
}

/// The answer to a [`SyncCall`], carried whole as the response body.
///
/// Written by the host under the lock that settled the request, from the bytes
/// that request's own readback produced, so the record's identity problem --
/// a slow answer landing in a slot the next request is using -- has nowhere to
/// happen. See `contracts/frame-wire/wire-v1.md`, "An answer as one body".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncAnswer {
    /// Settled: `Ready`, `Failed` or `Cancelled`.
    pub state: SyncState,
    pub error: Option<SyncError>,
    /// `0` when the call was refused before it was given an id.
    pub request_id: u32,
    /// Bytes of reply after the header; `0` unless `Ready`.
    pub reply_bytes: u32,
}

/// Bytes before an answer's reply.
pub const SYNC_ANSWER_HEADER_BYTES: usize = 16;

const SYNC_ANSWER_OFF_STATE: usize = 0;
const SYNC_ANSWER_OFF_ERROR: usize = 4;
const SYNC_ANSWER_OFF_REQUEST_ID: usize = 8;
const SYNC_ANSWER_OFF_REPLY_BYTES: usize = 12;

/// The answer body's fixed part, in order, with no gaps.
pub const SYNC_ANSWER_LAYOUT: &[crate::HeaderField] = &[
    crate::HeaderField {
        offset: SYNC_ANSWER_OFF_STATE as u32,
        size: 4,
        name: "state",
    },
    crate::HeaderField {
        offset: SYNC_ANSWER_OFF_ERROR as u32,
        size: 4,
        name: "error",
    },
    crate::HeaderField {
        offset: SYNC_ANSWER_OFF_REQUEST_ID as u32,
        size: 4,
        name: "request_id",
    },
    crate::HeaderField {
        offset: SYNC_ANSWER_OFF_REPLY_BYTES as u32,
        size: 4,
        name: "reply_bytes",
    },
];

impl SyncAnswer {
    /// A call that ended without an answer.
    pub const fn failed(request_id: u32, error: SyncError) -> Self {
        Self {
            state: SyncState::Failed,
            error: Some(error),
            request_id,
            reply_bytes: 0,
        }
    }

    /// The whole body's length: the header and the reply after it.
    #[inline]
    pub const fn body_bytes(&self) -> usize {
        SYNC_ANSWER_HEADER_BYTES + self.reply_bytes as usize
    }

    /// Write the header. The reply, when there is one, is sent after it as a
    /// separate part: it is the vector the renderer answered with, and placing
    /// it behind a header in one buffer would be a copy of up to 16 MiB.
    ///
    /// # Panics
    /// When `out` is shorter than [`SYNC_ANSWER_HEADER_BYTES`].
    pub fn write_header(&self, out: &mut [u8]) {
        let error = self.error.map_or(0, SyncError::code);
        out[SYNC_ANSWER_OFF_STATE..SYNC_ANSWER_OFF_STATE + 4]
            .copy_from_slice(&self.state.code().to_le_bytes());
        out[SYNC_ANSWER_OFF_ERROR..SYNC_ANSWER_OFF_ERROR + 4].copy_from_slice(&error.to_le_bytes());
        out[SYNC_ANSWER_OFF_REQUEST_ID..SYNC_ANSWER_OFF_REQUEST_ID + 4]
            .copy_from_slice(&self.request_id.to_le_bytes());
        out[SYNC_ANSWER_OFF_REPLY_BYTES..SYNC_ANSWER_OFF_REPLY_BYTES + 4]
            .copy_from_slice(&self.reply_bytes.to_le_bytes());
    }

    /// Read a header back. For tests and for a Rust producer; the host only
    /// writes these.
    pub fn read_header(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < SYNC_ANSWER_HEADER_BYTES {
            return None;
        }
        let error = u32_at(bytes, SYNC_ANSWER_OFF_ERROR);
        Some(Self {
            state: SyncState::from_code(u32_at(bytes, SYNC_ANSWER_OFF_STATE))?,
            error: if error == 0 {
                None
            } else {
                Some(*SyncError::ALL.iter().find(|known| known.code() == error)?)
            },
            request_id: u32_at(bytes, SYNC_ANSWER_OFF_REQUEST_ID),
            reply_bytes: u32_at(bytes, SYNC_ANSWER_OFF_REPLY_BYTES),
        })
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

    /// Whether the session has ended, after which no request is answered.
    #[inline]
    pub const fn is_ended(&self) -> bool {
        self.ended
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

#[cfg(test)]
mod sync_call_tests {
    use super::*;

    fn body(max_reply_bytes: u32, timeout_millis: u32, reserved: u32, params: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for word in [1u64, 2, 3, 4] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for word in [
            SYNC_OP_READ_PIXELS,
            max_reply_bytes,
            timeout_millis,
            reserved,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(params);
        bytes
    }

    #[test]
    fn a_call_decodes_every_field_and_borrows_its_arguments() {
        let bytes = body(16, 250, 0, &[9, 8, 7]);
        let call = SyncCall::decode(&bytes).expect("valid");
        assert_eq!(
            (
                call.runtime_generation,
                call.surface_generation,
                call.resource_epoch,
                call.triggering_sequence,
                call.operation,
                call.max_reply_bytes,
                call.timeout_millis,
                call.params,
            ),
            (1, 2, 3, 4, SYNC_OP_READ_PIXELS, 16, 250, &[9u8, 8, 7][..])
        );
        let request = call.request(1_000);
        assert_eq!(request.deadline_nanos, 1_000 + 250_000_000);
        assert_eq!(request.triggering_sequence, 4);
    }

    #[test]
    fn the_call_layout_is_gapless_and_ends_where_the_arguments_begin() {
        let mut end = 0;
        for field in SYNC_CALL_LAYOUT {
            assert_eq!(
                field.offset, end,
                "{} does not follow its predecessor",
                field.name
            );
            end += field.size;
        }
        assert_eq!(end as usize, SYNC_CALL_HEADER_BYTES);
    }

    #[test]
    fn a_short_long_or_reserved_envelope_is_refused() {
        let bytes = body(16, 250, 0, &[]);
        assert_eq!(
            SyncCall::decode(&bytes[..SYNC_CALL_HEADER_BYTES - 1]),
            Err(SyncError::UnsupportedOperation)
        );
        assert_eq!(
            SyncCall::decode(&body(16, 250, 1, &[])),
            Err(SyncError::UnsupportedOperation)
        );
        let at_bound = body(
            16,
            250,
            0,
            &vec![0; SYNC_CALL_MAX_BYTES - SYNC_CALL_HEADER_BYTES],
        );
        assert!(
            SyncCall::decode(&at_bound).is_ok(),
            "the bound is inclusive"
        );
        let past_bound = body(
            16,
            250,
            0,
            &vec![0; SYNC_CALL_MAX_BYTES - SYNC_CALL_HEADER_BYTES + 1],
        );
        assert_eq!(
            SyncCall::decode(&past_bound),
            Err(SyncError::UnsupportedOperation)
        );
    }

    #[test]
    fn a_wait_of_nothing_or_of_more_than_a_minute_is_refused() {
        for timeout in [0, SYNC_CALL_MAX_TIMEOUT_MILLIS + 1, u32::MAX] {
            assert_eq!(
                SyncCall::decode(&body(16, timeout, 0, &[])),
                Err(SyncError::BadDeadline),
                "timeout {timeout}"
            );
        }
        assert!(SyncCall::decode(&body(16, SYNC_CALL_MAX_TIMEOUT_MILLIS, 0, &[])).is_ok());
    }

    #[test]
    fn an_answer_header_round_trips_every_field() {
        for answer in [
            SyncAnswer {
                state: SyncState::Ready,
                error: None,
                request_id: 7,
                reply_bytes: 4,
            },
            SyncAnswer::failed(0, SyncError::BadDeadline),
            SyncAnswer::failed(u32::MAX, SyncError::OperationFailed),
            SyncAnswer {
                state: SyncState::Cancelled,
                error: None,
                request_id: 3,
                reply_bytes: 0,
            },
        ] {
            let mut out = [0xAAu8; SYNC_ANSWER_HEADER_BYTES];
            answer.write_header(&mut out);
            assert_eq!(SyncAnswer::read_header(&out), Some(answer));
        }
        let mut end = 0;
        for field in SYNC_ANSWER_LAYOUT {
            assert_eq!(
                field.offset, end,
                "{} does not follow its predecessor",
                field.name
            );
            end += field.size;
        }
        assert_eq!(end as usize, SYNC_ANSWER_HEADER_BYTES);
    }
}
