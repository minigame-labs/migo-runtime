//! What the producer sends that is not a frame.
//!
//! One thing, today: a request for the next frame. Migo's
//! `requestAnimationFrame` is fed by host vsync on every platform, and that
//! demand has to cross the process boundary for a frame-clock tick to exist --
//! a host that ticked whether or not anyone asked would wake a producer sixty
//! times a second to tell it nothing. See *Uplink control messages* in
//! `contracts/frame-wire/wire-v1.md`, which this implements.
//!
//! # Its own envelope, so it can never be read as a frame
//!
//! A control message shares the socket with frame packets, and a transport has
//! to route each message to one door or the other before anything reads it.
//! [`is_control_message`] decides by the first word, which is never the frame
//! magic: a control message offered to frame ingress fails on its magic, and a
//! frame offered here fails on its magic, so a routing mistake is a refusal and
//! never a misreading.
//!
//! # Read without allocating
//!
//! A producer asks for a frame every frame. [`read_control`] validates the whole
//! message and hands back a view over the caller's bytes; the records are
//! decoded from that view as they are iterated. Validating everything first is
//! what makes a refusal total: nothing in a message is acted on unless all of
//! it is well formed.

use crate::stream::{opcode_of, pack_header, word_count_of};

/// "MUC1". Neither the frame magic nor the downlink's, and checked below, so the
/// router's one-word decision cannot be ambiguous.
pub const MAGIC_CONTROL: u32 = 0x4D55_4331;

/// Independent of the frame and downlink versions, for the reason the downlink
/// gives: the directions carry different records and do not change together.
pub const CONTROL_VERSION: u32 = 1;

/// Magic and version.
pub const CONTROL_ENVELOPE_WORDS: usize = 2;

/// The longest message, in words, envelope included. Small because a control
/// message carries requests, not data, and a bound this size makes the reader's
/// work per message a constant rather than a function of what content sent.
pub const MAX_CONTROL_WORDS: usize = 64;

/// A request for the next frame.
pub const UP_REQUEST_FRAME: u32 = 1;
/// Header word plus `generation`.
pub const REQUEST_FRAME_WORDS: u32 = 2;

const _: () = assert!(MAGIC_CONTROL != crate::WIRE_MAGIC);
const _: () = assert!(MAGIC_CONTROL != crate::downlink::MAGIC_DOWN);
const _: () = assert!(MAGIC_CONTROL != crate::stream::MAGIC);

/// One record in a control message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRecord {
    /// Ask for one frame-clock tick. `generation` is the low 32 bits of the
    /// runtime generation the request was made in; a request from another
    /// generation is ignored rather than refused, because the producer that
    /// sent it is gone and nothing it asked for is owed to its replacement.
    RequestFrame { generation: u32 },
}

impl ControlRecord {
    /// Words this record occupies, header included.
    pub const fn word_count(&self) -> u32 {
        match self {
            Self::RequestFrame { .. } => REQUEST_FRAME_WORDS,
        }
    }
}

/// Why a control message was refused.
///
/// Numbered from [`CONTROL_ERROR_BASE`], above the frame envelope, ingress and
/// external-session ranges, so the one telemetry field a transport reports a
/// refusal in carries any of them without ambiguity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    /// The byte count is not a whole number of words.
    TrailingBytes,
    /// Fewer words than an envelope and one record header.
    TooShort,
    /// More than [`MAX_CONTROL_WORDS`] words.
    TooLong,
    /// The first word is not [`MAGIC_CONTROL`].
    BadMagic,
    /// The second word is not [`CONTROL_VERSION`].
    UnsupportedVersion(u32),
    /// A record's word count is zero or runs past the end of the message.
    RecordLengthOutOfRange,
    /// A kind this build does not know. Refused rather than skipped: a host
    /// that skipped a request it did not understand would leave a producer
    /// waiting for an answer that is never coming.
    UnknownKind(u32),
    /// A known kind with a length that kind does not have.
    WrongLengthForKind { kind: u32, words: u32 },
}

/// The first control refusal code.
pub const CONTROL_ERROR_BASE: u32 = 3001;

impl ControlError {
    /// Every refusal, in code order, for consumers that must cover all of them
    /// and for the document-agreement test that holds this list to the table.
    pub const ALL: [ControlError; 8] = [
        Self::TrailingBytes,
        Self::TooShort,
        Self::TooLong,
        Self::BadMagic,
        Self::UnsupportedVersion(0),
        Self::RecordLengthOutOfRange,
        Self::UnknownKind(0),
        Self::WrongLengthForKind { kind: 0, words: 0 },
    ];

    /// The stable code carried across the C ABI.
    pub const fn code(&self) -> u32 {
        CONTROL_ERROR_BASE
            + match self {
                Self::TrailingBytes => 0,
                Self::TooShort => 1,
                Self::TooLong => 2,
                Self::BadMagic => 3,
                Self::UnsupportedVersion(_) => 4,
                Self::RecordLengthOutOfRange => 5,
                Self::UnknownKind(_) => 6,
                Self::WrongLengthForKind { .. } => 7,
            }
    }

    /// The name the contract's table uses.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TrailingBytes => "TrailingBytes",
            Self::TooShort => "TooShort",
            Self::TooLong => "TooLong",
            Self::BadMagic => "BadMagic",
            Self::UnsupportedVersion(_) => "UnsupportedVersion",
            Self::RecordLengthOutOfRange => "RecordLengthOutOfRange",
            Self::UnknownKind(_) => "UnknownKind",
            Self::WrongLengthForKind { .. } => "WrongLengthForKind",
        }
    }
}

/// Whether a message that arrived on the socket is a control message.
///
/// The router's whole decision, and deliberately only that: everything that is
/// not a control message goes to frame ingress, which refuses what it cannot
/// read with a verdict the producer sees. A third "unknown" answer would need a
/// third place to report it from.
#[inline]
pub fn is_control_message(bytes: &[u8]) -> bool {
    matches!(bytes.first_chunk::<4>(), Some(first) if u32::from_le_bytes(*first) == MAGIC_CONTROL)
}

/// A control message that has been validated in full.
///
/// Borrows the caller's bytes; the records are decoded as they are iterated.
#[derive(Clone, Copy, Debug)]
pub struct ControlMessage<'a> {
    /// The records, as little-endian bytes, envelope removed.
    records: &'a [u8],
}

impl<'a> ControlMessage<'a> {
    /// The records, in wire order.
    pub fn records(&self) -> ControlRecords<'a> {
        ControlRecords {
            remaining: self.records,
        }
    }
}

/// Iterator over a validated message's records.
#[derive(Clone, Debug)]
pub struct ControlRecords<'a> {
    remaining: &'a [u8],
}

impl Iterator for ControlRecords<'_> {
    type Item = ControlRecord;

    fn next(&mut self) -> Option<ControlRecord> {
        let header = word(self.remaining, 0)?;
        // Validated by `read_control`: the kind is known and its length is the
        // kind's, so these reads are in bounds.
        let (record, count) = match opcode_of(header) {
            UP_REQUEST_FRAME => (
                ControlRecord::RequestFrame {
                    generation: word(self.remaining, 1)?,
                },
                REQUEST_FRAME_WORDS,
            ),
            _ => return None,
        };
        self.remaining = &self.remaining[count as usize * 4..];
        Some(record)
    }
}

#[inline]
fn word(bytes: &[u8], index: usize) -> Option<u32> {
    let start = index.checked_mul(4)?;
    let chunk = bytes.get(start..start + 4)?;
    Some(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
}

/// Validate a whole control message.
///
/// Length first, then envelope, then every record, and nothing is returned for
/// the caller to act on until all of it has passed.
pub fn read_control(bytes: &[u8]) -> Result<ControlMessage<'_>, ControlError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(ControlError::TrailingBytes);
    }
    let words = bytes.len() / 4;
    if words > MAX_CONTROL_WORDS {
        return Err(ControlError::TooLong);
    }
    if words < CONTROL_ENVELOPE_WORDS + 1 {
        return Err(ControlError::TooShort);
    }
    if word(bytes, 0) != Some(MAGIC_CONTROL) {
        return Err(ControlError::BadMagic);
    }
    let version = word(bytes, 1).unwrap_or_default();
    if version != CONTROL_VERSION {
        return Err(ControlError::UnsupportedVersion(version));
    }

    let mut at = CONTROL_ENVELOPE_WORDS;
    while at < words {
        let header = word(bytes, at).unwrap_or_default();
        let kind = opcode_of(header);
        let count = word_count_of(header) as usize;
        // `count == 0` is not redundant with the bound: a zero-length record
        // would leave `at` where it is and turn a malformed message into a hang.
        if count == 0 || count > words - at {
            return Err(ControlError::RecordLengthOutOfRange);
        }
        let expected = match kind {
            UP_REQUEST_FRAME => REQUEST_FRAME_WORDS,
            other => return Err(ControlError::UnknownKind(other)),
        };
        if count != expected as usize {
            return Err(ControlError::WrongLengthForKind {
                kind,
                words: count as u32,
            });
        }
        at += count;
    }

    Ok(ControlMessage {
        records: &bytes[CONTROL_ENVELOPE_WORDS * 4..],
    })
}

/// Encode records into one control message.
///
/// The host never sends one; this is the reference writer the tests and the
/// cross-language gate hold the producer's encoder to.
pub fn encode_control(records: &[ControlRecord]) -> Vec<u8> {
    let words = CONTROL_ENVELOPE_WORDS
        + records
            .iter()
            .map(|record| record.word_count() as usize)
            .sum::<usize>();
    let mut bytes = Vec::with_capacity(words * 4);
    let mut push = |value: u32| bytes.extend_from_slice(&value.to_le_bytes());
    push(MAGIC_CONTROL);
    push(CONTROL_VERSION);
    for record in records {
        match *record {
            ControlRecord::RequestFrame { generation } => {
                push(pack_header(UP_REQUEST_FRAME, REQUEST_FRAME_WORDS));
                push(generation);
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(generation: u32) -> ControlRecord {
        ControlRecord::RequestFrame { generation }
    }

    fn words_of(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn bytes_of(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn a_request_survives_the_round_trip() {
        let bytes = encode_control(&[request(0xDEAD_BEEF)]);
        assert_eq!(bytes.len(), 16, "an envelope and one two-word record");
        let read: Vec<_> = read_control(&bytes).expect("valid").records().collect();
        assert_eq!(read, vec![request(0xDEAD_BEEF)]);
    }

    #[test]
    fn several_records_come_back_in_order() {
        let sent = [request(1), request(2), request(3)];
        let bytes = encode_control(&sent);
        let read: Vec<_> = read_control(&bytes).expect("valid").records().collect();
        assert_eq!(read, sent);
    }

    #[test]
    fn the_router_recognises_a_control_message_and_nothing_else() {
        assert!(is_control_message(&encode_control(&[request(1)])));
        let mut frame = crate::builder::WireFrameBuilder::new();
        frame.sequence = 1;
        let frame = frame
            .section(crate::SECTION_KIND_COMMAND_STREAM, 0, &[])
            .build();
        assert!(!is_control_message(&frame), "a frame packet is not one");
        assert!(!is_control_message(&[]), "nothing is not one");
        assert!(
            !is_control_message(&MAGIC_CONTROL.to_le_bytes()[..3]),
            "three bytes of the magic are not one"
        );
        assert!(
            !is_control_message(&MAGIC_CONTROL.to_be_bytes()),
            "the magic is little-endian like every other word"
        );
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = encode_control(&[request(1)]);
        bytes.push(0);
        assert_eq!(
            read_control(&bytes).unwrap_err(),
            ControlError::TrailingBytes
        );
    }

    #[test]
    fn an_envelope_with_no_record_is_refused() {
        let bytes = bytes_of(&[MAGIC_CONTROL, CONTROL_VERSION]);
        assert_eq!(read_control(&bytes).unwrap_err(), ControlError::TooShort);
        assert_eq!(read_control(&[]).unwrap_err(), ControlError::TooShort);
    }

    #[test]
    fn a_message_past_the_ceiling_is_refused_before_it_is_walked() {
        let records: Vec<_> = (0..(MAX_CONTROL_WORDS as u32 / 2)).map(request).collect();
        let bytes = encode_control(&records);
        assert_eq!(bytes.len() / 4, MAX_CONTROL_WORDS + 2);
        assert_eq!(read_control(&bytes).unwrap_err(), ControlError::TooLong);

        let at_ceiling = encode_control(&records[..records.len() - 1]);
        assert_eq!(at_ceiling.len() / 4, MAX_CONTROL_WORDS);
        assert!(
            read_control(&at_ceiling).is_ok(),
            "exactly the ceiling is legal"
        );
    }

    #[test]
    fn a_wrong_magic_or_version_is_refused() {
        let mut words = words_of(&encode_control(&[request(1)]));
        words[0] = crate::WIRE_MAGIC;
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::BadMagic
        );
        let mut words = words_of(&encode_control(&[request(1)]));
        words[1] = CONTROL_VERSION + 1;
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::UnsupportedVersion(CONTROL_VERSION + 1)
        );
    }

    #[test]
    fn a_record_that_runs_past_the_end_or_has_no_length_is_refused() {
        let mut words = words_of(&encode_control(&[request(1)]));
        words[CONTROL_ENVELOPE_WORDS] = pack_header(UP_REQUEST_FRAME, 3);
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::RecordLengthOutOfRange
        );
        words[CONTROL_ENVELOPE_WORDS] = pack_header(UP_REQUEST_FRAME, 0);
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::RecordLengthOutOfRange
        );
    }

    #[test]
    fn an_unknown_kind_or_a_wrong_length_is_refused() {
        let mut words = words_of(&encode_control(&[request(1)]));
        words[CONTROL_ENVELOPE_WORDS] = pack_header(0x7FF, 2);
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::UnknownKind(0x7FF)
        );
        let words = [
            MAGIC_CONTROL,
            CONTROL_VERSION,
            pack_header(UP_REQUEST_FRAME, 1),
        ];
        assert_eq!(
            read_control(&bytes_of(&words)).unwrap_err(),
            ControlError::WrongLengthForKind {
                kind: UP_REQUEST_FRAME,
                words: 1
            }
        );
    }

    #[test]
    fn a_refusal_in_a_later_record_refuses_the_whole_message() {
        let mut words = words_of(&encode_control(&[request(1), request(2)]));
        words.push(pack_header(0x123, 1));
        assert!(
            read_control(&bytes_of(&words)).is_err(),
            "the valid records ahead of a bad one are not handed out"
        );
    }

    #[test]
    fn the_codes_are_contiguous_from_the_base_and_clear_the_other_ranges() {
        for (index, error) in ControlError::ALL.iter().enumerate() {
            assert_eq!(error.code(), CONTROL_ERROR_BASE + index as u32, "{error:?}");
        }
        assert!(
            crate::ingress::INGRESS_ERROR_CODES
                .iter()
                .all(|code| *code < CONTROL_ERROR_BASE),
            "the ingress range has reached the control range"
        );
    }
}
