//! The service stream: everything a producer asks the host to do that is not
//! drawing.
//!
//! A frame carries drawing. What content also does -- read a file, write a save,
//! load an image, start a sound, open a socket -- is answered by host services
//! that on every other Migo platform are an op call away. Here they are a
//! process away, so they travel on a stream of their own. See *The service
//! stream* in `contracts/frame-wire/wire-v1.md`, which this implements.
//!
//! # Why a stream of its own, and one stream
//!
//! Not the frame packet: a frame is admitted only once the renderer is up and
//! only inside the credit window, and a game reads its configuration and its
//! save before its first frame. Not one request per fetch: commands and
//! requests to one subsystem have to run in the order content made them --
//! the embedded runtime's audio commands and its awaited `stop`/`connect`
//! share one channel to the audio thread, and "create the node, then stop it"
//! is the meaning, not a detail. So every service message carries a sequence,
//! and the host admits them strictly in order.
//!
//! # Uplink: `MUS1`
//!
//! ```text
//! 0   magic       u32  "MUS1"
//! 4   version     u32  1
//! 8   generation  u32  low 32 bits of the runtime generation
//! 12  reserved    u32  0
//! 16  sequence    u64  1, 2, 3 ... per generation, no gaps
//! 24  records     until the end
//! ```
//!
//! A record is `kind u32, byte_length u32`, then `byte_length` bytes of body
//! and zero padding to a word. Not the command stream's 20-bit word count: a
//! message carried as a scheme request can be many megabytes -- a file write,
//! a request body -- and a record has to be able to say so.
//!
//! # Downlink: `MDS1`
//!
//! Its own envelope rather than more kinds in the frame downlink's `MDL1`,
//! because the two queues have opposite rules: a frame verdict or a tick is a
//! level that the next one supersedes, so that queue coalesces and may drop;
//! an answer to a request is owed exactly once and none may be lost.
//!
//! # Trust
//!
//! The uplink is read as hostile input, like a frame: validated in full before
//! any of it is acted on, bounded before it is read, and allocated by what was
//! parsed rather than what was claimed. The downlink is the host's own writing
//! and is read by the producer.

use core::fmt;

use crate::value::{OwnedValue, Value, ValueError, ValueWriter, read_value, read_values};

/// "MUS1". Not the frame magic, the control magic or either downlink's, and
/// checked below, so a transport's one-word routing decision is never
/// ambiguous.
pub const MAGIC_SERVICE: u32 = 0x4D55_5331;

/// "MDS1": the host's answers and events.
pub const MAGIC_SERVICE_DOWN: u32 = 0x4D44_5331;

/// Independent of every other version in the crate, for the reason the
/// downlink gives: the streams carry different records and do not change
/// together.
pub const SERVICE_VERSION: u32 = 1;

/// Bytes before an uplink message's first record.
pub const SERVICE_HEADER_BYTES: usize = 24;

/// Bytes before a downlink message's first record.
pub const SERVICE_DOWN_HEADER_BYTES: usize = 16;

/// Bytes in a record's header: kind and body length.
pub const RECORD_HEADER_BYTES: usize = 8;

/// The largest uplink message, in bytes.
///
/// Sized by the largest single thing content can hand a service in one call --
/// a file write or a request body -- and bounded because the transport holds the
/// whole message before this reads it. A write larger than this is refused by
/// the producer with a message naming the limit, before anything crosses.
pub const MAX_SERVICE_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// The largest answer carried inline on the socket, body included.
///
/// G0's crossover (`transport-crossover-sweep.json`): below 64 KiB the loopback
/// socket is faster and cheaper on the host's CPU, above it a scheme request is.
/// A larger answer is parked (see [`DOWN_REPLY_PARKED`]) and the producer takes
/// it with one request -- the same rule the uplink follows, applied to the
/// direction that carries file contents.
pub const MAX_INLINE_REPLY_BYTES: usize = 64 * 1024;

/// A request: `request_id u32` (never 0), `op u32`, then its arguments.
pub const UP_REQUEST: u32 = 1;
/// A command: `op u32`, then its arguments. Nothing answers it.
pub const UP_COMMAND: u32 = 2;
/// Withdraw a request: `request_id u32`. Its answer, if one is produced, is
/// still sent -- the producer drops it -- so cancelling never loses the one
/// answer a request is owed.
pub const UP_CANCEL: u32 = 3;

/// The answer to one request: `request_id u32`, `outcome u32`, then exactly one
/// value when the outcome is [`OUTCOME_OK`], or a class string and a message
/// string when it is [`OUTCOME_ERROR`].
pub const DOWN_REPLY: u32 = 1;
/// An answer too large to carry inline: `request_id u32`, `byte_length u32`.
/// The producer takes the whole [`DOWN_REPLY`] record, `byte_length` bytes, by
/// requesting it from the host.
pub const DOWN_REPLY_PARKED: u32 = 2;
/// Something the host has to tell content that no request asked for: `event
/// u32`, then its values.
pub const DOWN_EVENT: u32 = 3;
/// The host refused a service message: `code u32`, `reserved u32`, `sequence
/// u64`. The stream is broken from that point -- every later message is out of
/// sequence -- so this is terminal for the producer, as a refused frame is.
pub const DOWN_REFUSED: u32 = 4;

pub const OUTCOME_OK: u32 = 0;
pub const OUTCOME_ERROR: u32 = 1;

const _: () = assert!(MAGIC_SERVICE != crate::WIRE_MAGIC);
const _: () = assert!(MAGIC_SERVICE != crate::control::MAGIC_CONTROL);
const _: () = assert!(MAGIC_SERVICE != crate::downlink::MAGIC_DOWN);
const _: () = assert!(MAGIC_SERVICE != crate::stream::MAGIC);
const _: () = assert!(MAGIC_SERVICE_DOWN != crate::downlink::MAGIC_DOWN);
const _: () = assert!(MAGIC_SERVICE_DOWN != MAGIC_SERVICE);

/// Whether a message that arrived on the socket is a service message.
#[inline]
pub fn is_service_message(bytes: &[u8]) -> bool {
    matches!(bytes.first_chunk::<4>(), Some(first) if u32::from_le_bytes(*first) == MAGIC_SERVICE)
}

/// Why a service message was refused.
///
/// Numbered from [`SERVICE_ERROR_BASE`], clear of the frame (1..), ingress
/// (1001..), external-session (2001..) and control (3001..) ranges, so one
/// telemetry field carries any of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceError {
    /// Fewer bytes than the header and one record header.
    TooShort,
    /// More than [`MAX_SERVICE_MESSAGE_BYTES`].
    TooLong,
    /// The byte count is not a whole number of words.
    NotWordAligned,
    /// The first word is not [`MAGIC_SERVICE`].
    BadMagic,
    /// The version is not [`SERVICE_VERSION`].
    UnsupportedVersion(u32),
    /// The reserved word is not zero.
    ReservedNotZero,
    /// Sequence zero, which no message carries: the first is one.
    ZeroSequence,
    /// A record's length runs past the end of the message.
    RecordOutOfRange,
    /// Padding after a record's body is not zero.
    PaddingNotZero,
    /// A record kind this version does not define. Refused rather than skipped:
    /// a request the host skipped is a promise content waits on forever.
    UnknownKind(u32),
    /// A record too short for its kind, or a request id of zero.
    MalformedRecord(u32),
    /// A record's arguments are not a valid run of values.
    BadValues(ValueError),
    /// At or behind the last admitted sequence, or a second copy of a message
    /// already held: a replay.
    OutOfSequence { expected: u64, received: u64 },
    /// Ahead of a predecessor that has not arrived, with the host already
    /// holding as many messages or bytes ahead of it as it keeps.
    TooFarAhead { expected: u64, received: u64 },
}

/// The first service refusal code.
pub const SERVICE_ERROR_BASE: u32 = 4001;

impl ServiceError {
    /// Every refusal, in code order, for the document-agreement test.
    pub const ALL: [ServiceError; 14] = [
        Self::TooShort,
        Self::TooLong,
        Self::NotWordAligned,
        Self::BadMagic,
        Self::UnsupportedVersion(0),
        Self::ReservedNotZero,
        Self::ZeroSequence,
        Self::RecordOutOfRange,
        Self::PaddingNotZero,
        Self::UnknownKind(0),
        Self::MalformedRecord(0),
        Self::BadValues(ValueError::Truncated),
        Self::OutOfSequence {
            expected: 0,
            received: 0,
        },
        Self::TooFarAhead {
            expected: 0,
            received: 0,
        },
    ];

    /// The stable code carried across the C ABI and in [`DOWN_REFUSED`].
    pub const fn code(&self) -> u32 {
        SERVICE_ERROR_BASE
            + match self {
                Self::TooShort => 0,
                Self::TooLong => 1,
                Self::NotWordAligned => 2,
                Self::BadMagic => 3,
                Self::UnsupportedVersion(_) => 4,
                Self::ReservedNotZero => 5,
                Self::ZeroSequence => 6,
                Self::RecordOutOfRange => 7,
                Self::PaddingNotZero => 8,
                Self::UnknownKind(_) => 9,
                Self::MalformedRecord(_) => 10,
                Self::BadValues(_) => 11,
                Self::OutOfSequence { .. } => 12,
                Self::TooFarAhead { .. } => 13,
            }
    }

    /// The name the contract's table uses.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TooShort => "TooShort",
            Self::TooLong => "TooLong",
            Self::NotWordAligned => "NotWordAligned",
            Self::BadMagic => "BadMagic",
            Self::UnsupportedVersion(_) => "UnsupportedVersion",
            Self::ReservedNotZero => "ReservedNotZero",
            Self::ZeroSequence => "ZeroSequence",
            Self::RecordOutOfRange => "RecordOutOfRange",
            Self::PaddingNotZero => "PaddingNotZero",
            Self::UnknownKind(_) => "UnknownKind",
            Self::MalformedRecord(_) => "MalformedRecord",
            Self::BadValues(_) => "BadValues",
            Self::OutOfSequence { .. } => "OutOfSequence",
            Self::TooFarAhead { .. } => "TooFarAhead",
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(f, "service version {version} is not 1"),
            Self::UnknownKind(kind) => write!(f, "service record kind {kind} is not defined"),
            Self::MalformedRecord(kind) => write!(f, "a kind-{kind} record is malformed"),
            Self::BadValues(error) => write!(f, "a record's values are refused: {error}"),
            Self::OutOfSequence { expected, received } => {
                write!(
                    f,
                    "service message {received} arrived where {expected} was next"
                )
            }
            Self::TooFarAhead { expected, received } => write!(
                f,
                "service message {received} arrived with {expected} still missing and the hold full"
            ),
            other => f.write_str(other.name()),
        }
    }
}

/// One record of a validated uplink message, borrowing the message.
#[derive(Clone, Debug, PartialEq)]
pub enum ServiceRecord<'a> {
    Request {
        request_id: u32,
        op: u32,
        args: Vec<Value<'a>>,
    },
    Command {
        op: u32,
        args: Vec<Value<'a>>,
    },
    Cancel {
        request_id: u32,
    },
}

/// The same record, owning its arguments, for a call that is carried to
/// another thread and outlives the message.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedServiceRecord {
    Request {
        request_id: u32,
        op: u32,
        args: Vec<OwnedValue>,
    },
    Command {
        op: u32,
        args: Vec<OwnedValue>,
    },
    Cancel {
        request_id: u32,
    },
}

impl ServiceRecord<'_> {
    pub fn to_owned_record(&self) -> OwnedServiceRecord {
        match self {
            ServiceRecord::Request {
                request_id,
                op,
                args,
            } => OwnedServiceRecord::Request {
                request_id: *request_id,
                op: *op,
                args: args.iter().map(Value::to_owned_value).collect(),
            },
            ServiceRecord::Command { op, args } => OwnedServiceRecord::Command {
                op: *op,
                args: args.iter().map(Value::to_owned_value).collect(),
            },
            ServiceRecord::Cancel { request_id } => OwnedServiceRecord::Cancel {
                request_id: *request_id,
            },
        }
    }
}

/// A validated uplink message.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceMessage<'a> {
    pub generation: u32,
    pub sequence: u64,
    pub records: Vec<ServiceRecord<'a>>,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | (u64::from(u32_at(bytes, at + 4)) << 32)
}

/// Read the envelope alone: generation and sequence, without the records.
///
/// What a transport needs to decide whether a message is next, before paying
/// for the records. Every envelope rule is checked; the records are not.
pub fn read_service_envelope(bytes: &[u8]) -> Result<(u32, u64), ServiceError> {
    if bytes.len() > MAX_SERVICE_MESSAGE_BYTES {
        return Err(ServiceError::TooLong);
    }
    if bytes.len() < SERVICE_HEADER_BYTES + RECORD_HEADER_BYTES {
        return Err(ServiceError::TooShort);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(ServiceError::NotWordAligned);
    }
    if u32_at(bytes, 0) != MAGIC_SERVICE {
        return Err(ServiceError::BadMagic);
    }
    let version = u32_at(bytes, 4);
    if version != SERVICE_VERSION {
        return Err(ServiceError::UnsupportedVersion(version));
    }
    if u32_at(bytes, 12) != 0 {
        return Err(ServiceError::ReservedNotZero);
    }
    let sequence = u64_at(bytes, 16);
    if sequence == 0 {
        return Err(ServiceError::ZeroSequence);
    }
    Ok((u32_at(bytes, 8), sequence))
}

/// Split a run of records into `(kind, body)` pairs, checking lengths and
/// padding. Shared by both directions.
fn split_records(bytes: &[u8]) -> Result<Vec<(u32, &[u8])>, ServiceError> {
    let mut records = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes.len() - at < RECORD_HEADER_BYTES {
            return Err(ServiceError::RecordOutOfRange);
        }
        let kind = u32_at(bytes, at);
        let length = u32_at(bytes, at + 4) as usize;
        let body_start = at + RECORD_HEADER_BYTES;
        let padded = length
            .checked_add((4 - length % 4) % 4)
            .ok_or(ServiceError::RecordOutOfRange)?;
        if padded > bytes.len() - body_start {
            return Err(ServiceError::RecordOutOfRange);
        }
        if bytes[body_start + length..body_start + padded]
            .iter()
            .any(|&byte| byte != 0)
        {
            return Err(ServiceError::PaddingNotZero);
        }
        records.push((kind, &bytes[body_start..body_start + length]));
        at = body_start + padded;
    }
    Ok(records)
}

/// Read and validate a whole uplink message.
///
/// Nothing in a message is acted on unless all of it is well formed, which is
/// why this returns every record or none.
pub fn read_service_message(bytes: &[u8]) -> Result<ServiceMessage<'_>, ServiceError> {
    let (generation, sequence) = read_service_envelope(bytes)?;
    let mut records = Vec::new();
    for (kind, body) in split_records(&bytes[SERVICE_HEADER_BYTES..])? {
        // A body must itself be whole words: every field and every value is.
        if !body.len().is_multiple_of(4) {
            return Err(ServiceError::MalformedRecord(kind));
        }
        let record = match kind {
            UP_REQUEST => {
                if body.len() < 8 {
                    return Err(ServiceError::MalformedRecord(kind));
                }
                let request_id = u32_at(body, 0);
                if request_id == 0 {
                    return Err(ServiceError::MalformedRecord(kind));
                }
                ServiceRecord::Request {
                    request_id,
                    op: u32_at(body, 4),
                    args: read_values(&body[8..]).map_err(ServiceError::BadValues)?,
                }
            }
            UP_COMMAND => {
                if body.len() < 4 {
                    return Err(ServiceError::MalformedRecord(kind));
                }
                ServiceRecord::Command {
                    op: u32_at(body, 0),
                    args: read_values(&body[4..]).map_err(ServiceError::BadValues)?,
                }
            }
            UP_CANCEL => {
                if body.len() != 4 || u32_at(body, 0) == 0 {
                    return Err(ServiceError::MalformedRecord(kind));
                }
                ServiceRecord::Cancel {
                    request_id: u32_at(body, 0),
                }
            }
            other => return Err(ServiceError::UnknownKind(other)),
        };
        records.push(record);
    }
    if records.is_empty() {
        // The length check above guarantees a record header's worth of bytes,
        // and `split_records` would have refused a partial one; an empty run
        // here is unreachable, and saying so keeps it that way.
        return Err(ServiceError::TooShort);
    }
    Ok(ServiceMessage {
        generation,
        sequence,
        records,
    })
}

fn push_record(out: &mut Vec<u8>, kind: u32, body: &[u8]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(body.len())
            .expect("a record body fits in a u32 length")
            .to_le_bytes(),
    );
    out.extend_from_slice(body);
    out.extend(core::iter::repeat_n(0u8, (4 - body.len() % 4) % 4));
}

/// Write one uplink message. The producer's writer is checked against this.
///
/// Test support only: the host never writes this direction, and a writer
/// compiled into the shipping reader is a writer nothing checks.
#[cfg(any(test, feature = "test-support"))]
pub fn encode_service_message(
    generation: u32,
    sequence: u64,
    records: &[OwnedServiceRecord],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&MAGIC_SERVICE.to_le_bytes());
    out.extend_from_slice(&SERVICE_VERSION.to_le_bytes());
    out.extend_from_slice(&generation.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    for record in records {
        let (kind, body) = match record {
            OwnedServiceRecord::Request {
                request_id,
                op,
                args,
            } => {
                let mut body =
                    ValueWriter::over([request_id.to_le_bytes(), op.to_le_bytes()].concat());
                for arg in args {
                    arg.write_to(&mut body);
                }
                (UP_REQUEST, body.into_bytes())
            }
            OwnedServiceRecord::Command { op, args } => {
                let mut body = ValueWriter::over(op.to_le_bytes().to_vec());
                for arg in args {
                    arg.write_to(&mut body);
                }
                (UP_COMMAND, body.into_bytes())
            }
            OwnedServiceRecord::Cancel { request_id } => {
                (UP_CANCEL, request_id.to_le_bytes().to_vec())
            }
        };
        push_record(&mut out, kind, &body);
    }
    out
}

// ---------------------------------------------------------------------------
// Downlink
// ---------------------------------------------------------------------------

/// Why an answer failed, as content will see it: the class of the error it
/// throws and its message.
///
/// The class is carried by name because content matches on it -- the embedded
/// runtime's storage ops throw `StorageError`, its file ops `IOError`, and a
/// game that catches one and not the other has to see the same class here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyError {
    pub class: String,
    pub message: String,
}

/// One downlink record, as the producer reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum ServiceDownRecord {
    Reply {
        request_id: u32,
        outcome: Result<OwnedValue, ReplyError>,
    },
    ReplyParked {
        request_id: u32,
        byte_length: u32,
    },
    Event {
        event: u32,
        values: Vec<OwnedValue>,
    },
    Refused {
        code: u32,
        sequence: u64,
    },
}

/// Encode one downlink record, header and padding included.
///
/// Records rather than whole messages, because the host's outbox holds records
/// and batches them into messages as the transport drains it: a record is
/// encoded once, when it is produced, and a parked reply is exactly one record.
pub fn encode_down_record(record: &ServiceDownRecord) -> Vec<u8> {
    let (kind, body) = match record {
        ServiceDownRecord::Reply {
            request_id,
            outcome,
        } => {
            let mut body = Vec::with_capacity(16);
            body.extend_from_slice(&request_id.to_le_bytes());
            match outcome {
                Ok(value) => {
                    body.extend_from_slice(&OUTCOME_OK.to_le_bytes());
                    let mut writer = ValueWriter::over(body);
                    value.write_to(&mut writer);
                    (DOWN_REPLY, writer.into_bytes())
                }
                Err(error) => {
                    body.extend_from_slice(&OUTCOME_ERROR.to_le_bytes());
                    let mut writer = ValueWriter::over(body);
                    writer.str(&error.class);
                    writer.str(&error.message);
                    (DOWN_REPLY, writer.into_bytes())
                }
            }
        }
        ServiceDownRecord::ReplyParked {
            request_id,
            byte_length,
        } => (
            DOWN_REPLY_PARKED,
            [request_id.to_le_bytes(), byte_length.to_le_bytes()].concat(),
        ),
        ServiceDownRecord::Event { event, values } => {
            let mut writer = ValueWriter::over(event.to_le_bytes().to_vec());
            for value in values {
                value.write_to(&mut writer);
            }
            (DOWN_EVENT, writer.into_bytes())
        }
        ServiceDownRecord::Refused { code, sequence } => {
            let mut body = Vec::with_capacity(16);
            body.extend_from_slice(&code.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&sequence.to_le_bytes());
            (DOWN_REFUSED, body)
        }
    };
    let mut out = Vec::with_capacity(RECORD_HEADER_BYTES + body.len() + 3);
    push_record(&mut out, kind, &body);
    out
}

/// The downlink envelope for `generation`, to which encoded records are
/// appended.
pub fn down_envelope(generation: u32) -> [u8; SERVICE_DOWN_HEADER_BYTES] {
    let mut out = [0u8; SERVICE_DOWN_HEADER_BYTES];
    out[0..4].copy_from_slice(&MAGIC_SERVICE_DOWN.to_le_bytes());
    out[4..8].copy_from_slice(&SERVICE_VERSION.to_le_bytes());
    out[8..12].copy_from_slice(&generation.to_le_bytes());
    out
}

/// Why a downlink message could not be read. The producer is the reader; this
/// half exists so the host's writer is tested against a reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceDownError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u32),
    ReservedNotZero,
    Records(ServiceError),
    UnknownKind(u32),
    MalformedRecord(u32),
    BadValues(ValueError),
}

fn read_down_record(kind: u32, body: &[u8]) -> Result<ServiceDownRecord, ServiceDownError> {
    let malformed = ServiceDownError::MalformedRecord(kind);
    if !body.len().is_multiple_of(4) {
        return Err(malformed);
    }
    Ok(match kind {
        DOWN_REPLY => {
            if body.len() < 8 {
                return Err(malformed);
            }
            let request_id = u32_at(body, 0);
            let outcome = match u32_at(body, 4) {
                OUTCOME_OK => Ok(read_value(&body[8..])
                    .map_err(ServiceDownError::BadValues)?
                    .to_owned_value()),
                OUTCOME_ERROR => {
                    let values = read_values(&body[8..]).map_err(ServiceDownError::BadValues)?;
                    match values.as_slice() {
                        [Value::Str(class), Value::Str(message)] => Err(ReplyError {
                            class: (*class).to_owned(),
                            message: (*message).to_owned(),
                        }),
                        _ => return Err(malformed),
                    }
                }
                _ => return Err(malformed),
            };
            ServiceDownRecord::Reply {
                request_id,
                outcome,
            }
        }
        DOWN_REPLY_PARKED => {
            if body.len() != 8 {
                return Err(malformed);
            }
            ServiceDownRecord::ReplyParked {
                request_id: u32_at(body, 0),
                byte_length: u32_at(body, 4),
            }
        }
        DOWN_EVENT => {
            if body.len() < 4 {
                return Err(malformed);
            }
            ServiceDownRecord::Event {
                event: u32_at(body, 0),
                values: read_values(&body[4..])
                    .map_err(ServiceDownError::BadValues)?
                    .iter()
                    .map(Value::to_owned_value)
                    .collect(),
            }
        }
        DOWN_REFUSED => {
            if body.len() != 16 || u32_at(body, 4) != 0 {
                return Err(malformed);
            }
            ServiceDownRecord::Refused {
                code: u32_at(body, 0),
                sequence: u64_at(body, 8),
            }
        }
        other => return Err(ServiceDownError::UnknownKind(other)),
    })
}

/// Read a downlink message: its generation and records.
pub fn read_down_message(bytes: &[u8]) -> Result<(u32, Vec<ServiceDownRecord>), ServiceDownError> {
    if bytes.len() < SERVICE_DOWN_HEADER_BYTES || !bytes.len().is_multiple_of(4) {
        return Err(ServiceDownError::TooShort);
    }
    if u32_at(bytes, 0) != MAGIC_SERVICE_DOWN {
        return Err(ServiceDownError::BadMagic);
    }
    let version = u32_at(bytes, 4);
    if version != SERVICE_VERSION {
        return Err(ServiceDownError::UnsupportedVersion(version));
    }
    if u32_at(bytes, 12) != 0 {
        return Err(ServiceDownError::ReservedNotZero);
    }
    let generation = u32_at(bytes, 8);
    let records = split_records(&bytes[SERVICE_DOWN_HEADER_BYTES..])
        .map_err(ServiceDownError::Records)?
        .into_iter()
        .map(|(kind, body)| read_down_record(kind, body))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((generation, records))
}

/// Read one record that fills `bytes`: what a parked reply is when it is taken.
pub fn read_down_record_bytes(bytes: &[u8]) -> Result<ServiceDownRecord, ServiceDownError> {
    let mut records = split_records(bytes).map_err(ServiceDownError::Records)?;
    if records.len() != 1 {
        return Err(ServiceDownError::TooShort);
    }
    let (kind, body) = records.remove(0);
    read_down_record(kind, body)
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// Where an arriving message stands against the ones already admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sequencing {
    /// The next message: admit it.
    Next,
    /// From a generation that is not this one: ignore it. The producer that
    /// sent it is gone, and nothing it asked for is owed to its replacement.
    OtherGeneration,
    /// Ahead of the next one: the socket and a scheme request reorder, and the
    /// host holds it until its predecessor arrives.
    Ahead { expected: u64 },
    /// At or behind the last admitted: a replay, which is refused.
    Behind { expected: u64 },
}

/// The host's record of which service messages it has admitted.
///
/// Holds no messages itself: the external session keeps the ones that arrived
/// early, within its own bounds, and admits them here in order.
#[derive(Clone, Debug)]
pub struct ServiceSequencer {
    generation: u32,
    admitted: u64,
}

impl ServiceSequencer {
    pub fn new(generation: u32) -> Self {
        Self {
            generation,
            admitted: 0,
        }
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The last admitted sequence; zero before any.
    pub fn admitted(&self) -> u64 {
        self.admitted
    }

    pub fn classify(&self, generation: u32, sequence: u64) -> Sequencing {
        if generation != self.generation {
            return Sequencing::OtherGeneration;
        }
        let expected = self.admitted + 1;
        match sequence.cmp(&expected) {
            core::cmp::Ordering::Equal => Sequencing::Next,
            core::cmp::Ordering::Greater => Sequencing::Ahead { expected },
            core::cmp::Ordering::Less => Sequencing::Behind { expected },
        }
    }

    /// Record `sequence` as admitted. It must be the next one.
    pub fn admit(&mut self, sequence: u64) {
        debug_assert_eq!(sequence, self.admitted + 1, "admitted out of order");
        self.admitted = sequence;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(request_id: u32, op: u32, args: Vec<OwnedValue>) -> OwnedServiceRecord {
        OwnedServiceRecord::Request {
            request_id,
            op,
            args,
        }
    }

    #[test]
    fn a_message_survives_the_round_trip() {
        let records = vec![
            request(
                1,
                7,
                vec![
                    OwnedValue::Str("key".into()),
                    OwnedValue::Str("value".into()),
                ],
            ),
            OwnedServiceRecord::Command {
                op: 9,
                args: vec![OwnedValue::F64(0.25)],
            },
            OwnedServiceRecord::Cancel { request_id: 1 },
        ];
        let bytes = encode_service_message(3, (1 << 40) + 5, &records);
        let message = read_service_message(&bytes).expect("a message this module wrote");
        assert_eq!(message.generation, 3);
        assert_eq!(message.sequence, (1 << 40) + 5);
        let owned: Vec<_> = message
            .records
            .iter()
            .map(ServiceRecord::to_owned_record)
            .collect();
        assert_eq!(owned, records);
    }

    #[test]
    fn every_envelope_rule_has_its_own_refusal() {
        let good = encode_service_message(1, 1, &[request(1, 1, vec![])]);
        let with = |at: usize, word: u32| {
            let mut bytes = good.clone();
            bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
            bytes
        };
        assert_eq!(
            read_service_message(&with(0, 0)),
            Err(ServiceError::BadMagic)
        );
        assert_eq!(
            read_service_message(&with(4, 2)),
            Err(ServiceError::UnsupportedVersion(2))
        );
        assert_eq!(
            read_service_message(&with(12, 1)),
            Err(ServiceError::ReservedNotZero)
        );
        let mut zero = good.clone();
        zero[16..24].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(read_service_message(&zero), Err(ServiceError::ZeroSequence));
        assert_eq!(
            read_service_message(&good[..SERVICE_HEADER_BYTES]),
            Err(ServiceError::TooShort)
        );
        assert_eq!(
            read_service_message(&good[..good.len() - 1]),
            Err(ServiceError::NotWordAligned)
        );
    }

    #[test]
    fn a_record_that_runs_past_the_end_is_refused() {
        let mut bytes = encode_service_message(1, 1, &[request(1, 1, vec![])]);
        let length_at = SERVICE_HEADER_BYTES + 4;
        bytes[length_at..length_at + 4].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(
            read_service_message(&bytes),
            Err(ServiceError::RecordOutOfRange)
        );
    }

    #[test]
    fn a_request_named_zero_is_refused() {
        let bytes = encode_service_message(1, 1, &[request(0, 1, vec![])]);
        assert_eq!(
            read_service_message(&bytes),
            Err(ServiceError::MalformedRecord(UP_REQUEST))
        );
    }

    #[test]
    fn an_unknown_kind_is_refused_not_skipped() {
        let mut bytes =
            encode_service_message(1, 1, &[OwnedServiceRecord::Cancel { request_id: 1 }]);
        bytes[SERVICE_HEADER_BYTES..SERVICE_HEADER_BYTES + 4].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            read_service_message(&bytes),
            Err(ServiceError::UnknownKind(99))
        );
    }

    #[test]
    fn bad_values_refuse_the_whole_message() {
        let mut bytes =
            encode_service_message(1, 1, &[request(1, 1, vec![OwnedValue::Str("a".into())])]);
        // The string's padding is the last byte of the message.
        let last = bytes.len() - 1;
        bytes[last] = 1;
        assert_eq!(
            read_service_message(&bytes),
            Err(ServiceError::BadValues(ValueError::PaddingNotZero))
        );
    }

    #[test]
    fn refusal_codes_are_contiguous_from_the_base() {
        for (index, error) in ServiceError::ALL.iter().enumerate() {
            assert_eq!(
                error.code(),
                SERVICE_ERROR_BASE + index as u32,
                "{}",
                error.name()
            );
        }
    }

    #[test]
    fn downlink_records_survive_the_round_trip() {
        let records = vec![
            ServiceDownRecord::Reply {
                request_id: 5,
                outcome: Ok(OwnedValue::Array(vec![
                    OwnedValue::U32(9),
                    OwnedValue::Array(vec![OwnedValue::U32(64), OwnedValue::U32(32)]),
                ])),
            },
            ServiceDownRecord::Reply {
                request_id: 6,
                outcome: Err(ReplyError {
                    class: "StorageError".into(),
                    message: "setStorage:fail data exceeds max size".into(),
                }),
            },
            ServiceDownRecord::ReplyParked {
                request_id: 7,
                byte_length: 1 << 20,
            },
            ServiceDownRecord::Event {
                event: 2,
                values: vec![OwnedValue::Str("x".into())],
            },
            ServiceDownRecord::Refused {
                code: ServiceError::OutOfSequence {
                    expected: 2,
                    received: 3,
                }
                .code(),
                sequence: 1 << 33,
            },
        ];
        let mut bytes = down_envelope(4).to_vec();
        for record in &records {
            bytes.extend_from_slice(&encode_down_record(record));
        }
        let (generation, read) = read_down_message(&bytes).expect("a message this module wrote");
        assert_eq!(generation, 4);
        assert_eq!(read, records);
        // One record alone, which is what a parked reply is when taken.
        let one = encode_down_record(&records[0]);
        assert_eq!(read_down_record_bytes(&one), Ok(records[0].clone()));
    }

    #[test]
    fn the_sequencer_admits_only_the_next_message() {
        let mut sequencer = ServiceSequencer::new(1);
        assert_eq!(sequencer.classify(2, 1), Sequencing::OtherGeneration);
        assert_eq!(sequencer.classify(1, 2), Sequencing::Ahead { expected: 1 });
        assert_eq!(sequencer.classify(1, 1), Sequencing::Next);
        sequencer.admit(1);
        assert_eq!(sequencer.classify(1, 1), Sequencing::Behind { expected: 2 });
        assert_eq!(sequencer.classify(1, 2), Sequencing::Next);
    }
}
