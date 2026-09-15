//! The other direction: what the host sends the producer.
//!
//! `contracts/frame-wire/wire-v1.md` specifies producer-to-host -- frame
//! packets, credits, checksums, the synchronous barrier -- and said nothing
//! about the return path, because until there was a transport there was nothing
//! to return it through. This is that half.
//!
//! # Why there is no framing question here
//!
//! There was one, and the topology answered it. The hybrid transport switches
//! at 64 KiB for the *uplink*, where frame packets are large; a custom URL
//! scheme is request/response and the host cannot push through it at all. Both
//! things the host has to send -- a per-frame verdict and a frame-clock tick --
//! are host-initiated. So the downlink is the loopback WebSocket, always, and a
//! WebSocket already carries message boundaries.
//!
//! # Shape: the uplink's envelope, a different magic, its own kinds
//!
//! ```text
//! message = MAGIC_DOWN, DOWNLINK_VERSION, then a run of records
//! record  = stream::pack_header(kind, word_count), then word_count - 1 words
//! ```
//!
//! One envelope per message rather than a magic per record, which is the shape
//! [`crate::stream`] already uses. It buys the thing a downlink actually needs:
//! a verdict and a clock tick can ride one message without a second framing
//! layer, and the producer's reader is the same loop either way.
//!
//! # No checksum, and the asymmetry is the point
//!
//! The uplink is checksummed because the host must not trust the producer --
//! that is a security boundary, and the whole crate is written for it. This
//! direction is the reverse: the host is the trusted end, the producer is not a
//! boundary anything is being defended across, and loopback TCP already carries
//! integrity. Adding a checksum here would cost nothing and imply something
//! false -- that the two directions are symmetric in trust. Magic, version,
//! kind and length stay, because those catch a misrouted message and a version
//! mismatch, which are real and are not about tampering.

use crate::stream::{opcode_of, pack_header, word_count_of};

/// "MDL1", and deliberately not [`crate::stream::MAGIC`]: a producer that
/// somehow reads its own uplink bytes back, or a host that writes an uplink
/// packet into the downlink socket, is caught by the first word rather than by
/// a kind that happens to be in range.
pub const MAGIC_DOWN: u32 = 0x4D44_4C31;

/// Independent of `STREAM_VERSION`. The two directions carry different records
/// and will not change together; one number for both would force a lockstep
/// neither side needs.
pub const DOWNLINK_VERSION: u32 = 1;

/// The verdict on one submitted frame. Mirrors [`crate::IngressOutcome`] field
/// for field, because the producer's scheduling decisions are exactly the ones
/// that struct was defined to inform.
pub const DOWN_FRAME_VERDICT: u32 = 1;
/// One frame-clock tick, which is what drives `requestAnimationFrame` in the
/// producer. Host-driven on every Migo platform; see
/// `runtime-v8/src/rendering/webgl/03_raf.js` for the in-process shape this
/// keeps isomorphic.
pub const DOWN_CLOCK_TICK: u32 = 2;

/// Header word plus generation, decision, wire_error_code, remaining_credits,
/// and the two halves of `accepted_sequence`.
pub const FRAME_VERDICT_WORDS: u32 = 7;
/// Header word plus generation, frame_id, and the two halves of the timestamp.
pub const CLOCK_TICK_WORDS: u32 = 5;

/// The envelope's two leading words.
pub const ENVELOPE_WORDS: usize = 2;

/// A record the host has to send, in the form the producer reads it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownlinkRecord {
    FrameVerdict {
        generation: u32,
        decision: u32,
        wire_error_code: u32,
        remaining_credits: u32,
        accepted_sequence: u64,
    },
    ClockTick {
        generation: u32,
        frame_id: u32,
        timestamp_ns: u64,
    },
}

/// Why a downlink message could not be read.
///
/// Returned rather than logged: the producer is the consumer, it is in another
/// process, and a log line on the host answers nobody there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownlinkError {
    /// Fewer than two words, so there is no envelope to check.
    TooShortForEnvelope,
    /// The first word is not [`MAGIC_DOWN`].
    BadMagic,
    /// The second word is not a version this build reads.
    UnsupportedVersion(u32),
    /// A record's declared word count runs past the end of the message, or is
    /// smaller than the one header word it must contain.
    RecordLengthOutOfRange,
    /// A record's kind is not one this build knows. A rejection rather than a
    /// skip: "unknown required structure is a rejection" is the uplink's rule
    /// and the reason is the same here -- a producer that skipped an unknown
    /// verdict would keep scheduling frames against a host that had stopped
    /// agreeing with it.
    UnknownKind(u32),
    /// A known kind whose declared length is not the one that kind has. The
    /// lengths are fixed, so this is a wrong writer rather than an extension.
    WrongLengthForKind { kind: u32, words: u32 },
}

impl DownlinkRecord {
    /// The number of words this record occupies, header included.
    pub const fn word_count(&self) -> u32 {
        match self {
            Self::FrameVerdict { .. } => FRAME_VERDICT_WORDS,
            Self::ClockTick { .. } => CLOCK_TICK_WORDS,
        }
    }

    const fn kind(&self) -> u32 {
        match self {
            Self::FrameVerdict { .. } => DOWN_FRAME_VERDICT,
            Self::ClockTick { .. } => DOWN_CLOCK_TICK,
        }
    }

    /// Append this record's words to `out`.
    ///
    /// The 64-bit fields go low word first, which is the order
    /// [`crate::sync`]'s mailbox already uses. One convention per crate, so a
    /// reader never has to remember which side of the boundary it is on.
    pub fn write_words(&self, out: &mut Vec<u32>) {
        out.push(pack_header(self.kind(), self.word_count()));
        match *self {
            Self::FrameVerdict {
                generation,
                decision,
                wire_error_code,
                remaining_credits,
                accepted_sequence,
            } => {
                out.push(generation);
                out.push(decision);
                out.push(wire_error_code);
                out.push(remaining_credits);
                out.push(accepted_sequence as u32);
                out.push((accepted_sequence >> 32) as u32);
            }
            Self::ClockTick {
                generation,
                frame_id,
                timestamp_ns,
            } => {
                out.push(generation);
                out.push(frame_id);
                out.push(timestamp_ns as u32);
                out.push((timestamp_ns >> 32) as u32);
            }
        }
    }
}

/// Build one downlink message from a run of records.
///
/// Takes a slice rather than one record because batching is the reason the
/// envelope exists: a tick and the verdict for the frame it is acknowledging
/// are one message, not two round trips.
pub fn encode_message(records: &[DownlinkRecord]) -> Vec<u32> {
    let mut words = Vec::with_capacity(
        ENVELOPE_WORDS
            + records
                .iter()
                .map(|r| r.word_count() as usize)
                .sum::<usize>(),
    );
    words.push(MAGIC_DOWN);
    words.push(DOWNLINK_VERSION);
    for record in records {
        record.write_words(&mut words);
    }
    words
}

/// Read a downlink message back into records.
///
/// Allocates one `Vec` sized by the number of records actually read, never by a
/// count the message claims -- the uplink's rule, kept here even though the
/// trust direction is reversed, because "allocate what you have parsed" costs
/// nothing to keep and is one less thing to have to remember is different.
pub fn decode_message(words: &[u32]) -> Result<Vec<DownlinkRecord>, DownlinkError> {
    if words.len() < ENVELOPE_WORDS {
        return Err(DownlinkError::TooShortForEnvelope);
    }
    if words[0] != MAGIC_DOWN {
        return Err(DownlinkError::BadMagic);
    }
    if words[1] != DOWNLINK_VERSION {
        return Err(DownlinkError::UnsupportedVersion(words[1]));
    }

    let mut records = Vec::new();
    let mut at = ENVELOPE_WORDS;
    while at < words.len() {
        let header = words[at];
        let kind = opcode_of(header);
        let count = word_count_of(header);
        if count < 1 || (count as usize) > words.len() - at {
            return Err(DownlinkError::RecordLengthOutOfRange);
        }
        let body = &words[at + 1..at + count as usize];
        let record = match kind {
            DOWN_FRAME_VERDICT => {
                if count != FRAME_VERDICT_WORDS {
                    return Err(DownlinkError::WrongLengthForKind { kind, words: count });
                }
                DownlinkRecord::FrameVerdict {
                    generation: body[0],
                    decision: body[1],
                    wire_error_code: body[2],
                    remaining_credits: body[3],
                    accepted_sequence: u64::from(body[4]) | (u64::from(body[5]) << 32),
                }
            }
            DOWN_CLOCK_TICK => {
                if count != CLOCK_TICK_WORDS {
                    return Err(DownlinkError::WrongLengthForKind { kind, words: count });
                }
                DownlinkRecord::ClockTick {
                    generation: body[0],
                    frame_id: body[1],
                    timestamp_ns: u64::from(body[2]) | (u64::from(body[3]) << 32),
                }
            }
            other => return Err(DownlinkError::UnknownKind(other)),
        };
        records.push(record);
        at += count as usize;
    }
    Ok(records)
}

/// The message as bytes, little-endian, which is what a socket takes.
pub fn encode_bytes(records: &[DownlinkRecord]) -> Vec<u8> {
    let words = encode_message(records);
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Read a message that arrived as bytes.
///
/// A length that is not a multiple of four is `TooShortForEnvelope` rather than
/// a kind of its own: the only thing a caller can do about either is drop the
/// message, and a byte count that does not divide into words is not a
/// structure this format has.
pub fn decode_bytes(bytes: &[u8]) -> Result<Vec<DownlinkRecord>, DownlinkError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(DownlinkError::TooShortForEnvelope);
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    decode_message(&words)
}

/// How many records the host will hold for a producer that is not reading.
///
/// Small on purpose. The transport drains this every frame; a queue that is
/// deep enough to hide a stalled reader is a queue that turns a dead connection
/// into a memory leak, and the credit window already bounds how many frames can
/// be outstanding.
pub const QUEUE_CAPACITY: usize = 64;

/// The host's outbound records, waiting for the transport to send them.
///
/// # Why the two kinds are treated differently under pressure
///
/// A clock tick is superseded by the next one: a producer that missed tick 41
/// and got tick 42 has lost nothing it can act on, because the only thing it
/// does with a tick is schedule the next frame. So ticks COALESCE -- at most one
/// is ever queued, and pushing a new one replaces it.
///
/// A verdict is about a specific frame, so it does not coalesce. But it is also
/// *absolute* rather than incremental -- `remaining_credits` is a level, not a
/// delta -- so a producer that misses one and reads the next is back in step. On
/// overflow the oldest verdict is dropped and counted rather than the newest,
/// because the newest carries the most current credit level.
///
/// That is the whole reason the format has no acknowledgement and no retry: at
/// every point the newest record is sufficient on its own.
#[derive(Debug, Default)]
pub struct DownlinkQueue {
    records: std::collections::VecDeque<DownlinkRecord>,
    dropped: u32,
}

impl DownlinkQueue {
    pub fn new() -> Self {
        Self {
            records: std::collections::VecDeque::with_capacity(QUEUE_CAPACITY),
            dropped: 0,
        }
    }

    /// Queue a verdict, dropping the oldest if there is no room.
    pub fn push_verdict(&mut self, record: DownlinkRecord) {
        debug_assert!(
            matches!(record, DownlinkRecord::FrameVerdict { .. }),
            "push_verdict is for verdicts; ticks go through push_tick so they coalesce"
        );
        while self.records.len() >= QUEUE_CAPACITY {
            self.records.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.records.push_back(record);
    }

    /// Queue a tick, replacing any tick already waiting.
    ///
    /// Replaced in place rather than removed and appended: a verdict queued
    /// before the old tick was sent still has to arrive before it, because the
    /// producer reads a verdict as "this is the credit level as of the frame
    /// I just sent" and a later tick would otherwise overtake it.
    pub fn push_tick(&mut self, record: DownlinkRecord) {
        debug_assert!(
            matches!(record, DownlinkRecord::ClockTick { .. }),
            "push_tick is for ticks"
        );
        if let Some(slot) = self
            .records
            .iter_mut()
            .find(|queued| matches!(queued, DownlinkRecord::ClockTick { .. }))
        {
            *slot = record;
            return;
        }
        while self.records.len() >= QUEUE_CAPACITY {
            self.records.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.records.push_back(record);
    }

    /// How many records are waiting.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// How many records have been dropped for lack of room since the last
    /// [`Self::take_dropped`].
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Read and clear the drop count.
    pub fn take_dropped(&mut self) -> u32 {
        std::mem::take(&mut self.dropped)
    }

    /// Encode as many whole records as fit in `out`, leaving the rest queued.
    ///
    /// Returns the number of bytes written, or zero when there is nothing to
    /// send *or* when `out` cannot hold the envelope plus one record -- the two
    /// are the same for the caller, which either way has no message to send, and
    /// distinguishing them would invite a caller to retry with the same buffer.
    ///
    /// Whole records only. A message carrying half a record is not a smaller
    /// message, it is a malformed one, and the producer's reader is written to
    /// refuse it rather than wait for the rest.
    pub fn drain_into(&mut self, out: &mut [u8]) -> usize {
        if self.records.is_empty() {
            return 0;
        }
        let capacity_words = out.len() / 4;
        if capacity_words <= ENVELOPE_WORDS {
            return 0;
        }
        let mut words: Vec<u32> = Vec::with_capacity(capacity_words.min(QUEUE_CAPACITY * 8));
        words.push(MAGIC_DOWN);
        words.push(DOWNLINK_VERSION);
        while let Some(next) = self.records.front() {
            if words.len() + next.word_count() as usize > capacity_words {
                break;
            }
            let record = self.records.pop_front().expect("peeked on the line above");
            record.write_words(&mut words);
        }
        if words.len() == ENVELOPE_WORDS {
            // Nothing fitted. An envelope with no records is legal, but sending
            // one here would tell the producer "no news" while news is queued.
            return 0;
        }
        for (index, word) in words.iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        words.len() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict() -> DownlinkRecord {
        DownlinkRecord::FrameVerdict {
            generation: 7,
            decision: 1,
            wire_error_code: 0,
            remaining_credits: 2,
            // Past 32 bits on purpose: the split is the part a hand-written
            // reader gets wrong, and a small number would not notice.
            accepted_sequence: 0x0000_00FF_1234_5678,
        }
    }

    fn tick() -> DownlinkRecord {
        DownlinkRecord::ClockTick {
            generation: 7,
            frame_id: 99,
            timestamp_ns: 0x0000_0123_4567_89AB,
        }
    }

    #[test]
    fn a_batch_survives_the_round_trip_in_order() {
        let sent = [tick(), verdict(), tick()];
        let read = decode_message(&encode_message(&sent)).expect("a message this crate wrote");
        assert_eq!(read, sent, "order and contents must both survive");
    }

    #[test]
    fn bytes_and_words_agree() {
        let sent = [verdict()];
        assert_eq!(
            decode_bytes(&encode_bytes(&sent)).expect("round trip"),
            decode_message(&encode_message(&sent)).expect("round trip"),
        );
    }

    #[test]
    fn an_empty_batch_is_a_legal_message_with_no_records() {
        let read = decode_message(&encode_message(&[])).expect("envelope only");
        assert!(read.is_empty(), "an envelope with no records reads as none");
    }

    #[test]
    fn the_uplink_magic_is_rejected_rather_than_read() {
        let mut words = encode_message(&[verdict()]);
        words[0] = crate::stream::MAGIC;
        assert_eq!(decode_message(&words), Err(DownlinkError::BadMagic));
    }

    #[test]
    fn a_version_this_build_does_not_read_is_named_in_the_error() {
        let mut words = encode_message(&[verdict()]);
        words[1] = DOWNLINK_VERSION + 1;
        assert_eq!(
            decode_message(&words),
            Err(DownlinkError::UnsupportedVersion(DOWNLINK_VERSION + 1))
        );
    }

    #[test]
    fn a_record_claiming_more_words_than_the_message_holds_is_rejected() {
        let mut words = encode_message(&[verdict()]);
        words[ENVELOPE_WORDS] = pack_header(DOWN_FRAME_VERDICT, FRAME_VERDICT_WORDS + 1);
        assert_eq!(
            decode_message(&words),
            Err(DownlinkError::RecordLengthOutOfRange)
        );
    }

    #[test]
    fn a_zero_length_record_cannot_stall_the_reader() {
        // Without the `count < 1` check the loop would not advance, which is the
        // shape that turns a malformed message into a hang rather than an error.
        let mut words = encode_message(&[verdict()]);
        words[ENVELOPE_WORDS] = pack_header(DOWN_FRAME_VERDICT, 0);
        assert_eq!(
            decode_message(&words),
            Err(DownlinkError::RecordLengthOutOfRange)
        );
    }

    #[test]
    fn a_known_kind_with_the_wrong_length_is_not_read_as_that_kind() {
        let mut words = encode_message(&[verdict(), tick()]);
        // Long enough to fit inside the message, so only the per-kind check can
        // catch it.
        words[ENVELOPE_WORDS] = pack_header(DOWN_FRAME_VERDICT, CLOCK_TICK_WORDS);
        assert_eq!(
            decode_message(&words),
            Err(DownlinkError::WrongLengthForKind {
                kind: DOWN_FRAME_VERDICT,
                words: CLOCK_TICK_WORDS,
            })
        );
    }

    #[test]
    fn an_unknown_kind_is_rejected_rather_than_skipped() {
        let mut words = encode_message(&[verdict()]);
        words[ENVELOPE_WORDS] = pack_header(0xABC, FRAME_VERDICT_WORDS);
        assert_eq!(
            decode_message(&words),
            Err(DownlinkError::UnknownKind(0xABC))
        );
    }

    #[test]
    fn a_truncated_envelope_is_named_as_such() {
        assert_eq!(
            decode_message(&[MAGIC_DOWN]),
            Err(DownlinkError::TooShortForEnvelope)
        );
        assert_eq!(
            decode_bytes(&[0, 1, 2]),
            Err(DownlinkError::TooShortForEnvelope)
        );
    }

    #[test]
    fn a_drained_message_reads_back_as_what_was_queued() {
        let mut queue = DownlinkQueue::new();
        queue.push_verdict(verdict());
        queue.push_tick(tick());
        let mut out = [0u8; 256];
        let written = queue.drain_into(&mut out);
        assert!(written > 0, "two queued records must produce a message");
        assert_eq!(
            decode_bytes(&out[..written]).expect("a message this crate wrote"),
            vec![verdict(), tick()],
            "queue order is wire order"
        );
        assert!(queue.is_empty(), "a full drain leaves nothing behind");
    }

    #[test]
    fn ticks_coalesce_and_verdicts_do_not() {
        let mut queue = DownlinkQueue::new();
        for _ in 0..10 {
            queue.push_tick(tick());
        }
        assert_eq!(queue.len(), 1, "ten ticks are one queued tick");
        for _ in 0..5 {
            queue.push_verdict(verdict());
        }
        assert_eq!(
            queue.len(),
            6,
            "verdicts are about specific frames and stay"
        );
    }

    #[test]
    fn a_replaced_tick_keeps_its_place_behind_an_earlier_verdict() {
        // The producer reads a verdict as the credit level as of the frame it
        // just sent. A tick that overtook it would let the producer schedule
        // against a level it has not been told about yet.
        let mut queue = DownlinkQueue::new();
        queue.push_tick(DownlinkRecord::ClockTick {
            generation: 1,
            frame_id: 1,
            timestamp_ns: 1,
        });
        queue.push_verdict(verdict());
        queue.push_tick(DownlinkRecord::ClockTick {
            generation: 1,
            frame_id: 2,
            timestamp_ns: 2,
        });

        let mut out = [0u8; 256];
        let written = queue.drain_into(&mut out);
        let read = decode_bytes(&out[..written]).expect("round trip");
        assert_eq!(read.len(), 2, "the tick was replaced, not appended");
        assert!(
            matches!(read[0], DownlinkRecord::ClockTick { frame_id: 2, .. }),
            "the newest tick takes the old one's place: {read:?}"
        );
        assert!(
            matches!(read[1], DownlinkRecord::FrameVerdict { .. }),
            "and the verdict queued after it still follows: {read:?}"
        );
    }

    #[test]
    fn an_unread_queue_drops_the_oldest_and_counts_it() {
        let mut queue = DownlinkQueue::new();
        for i in 0..(QUEUE_CAPACITY as u64 + 5) {
            queue.push_verdict(DownlinkRecord::FrameVerdict {
                generation: 1,
                decision: 1,
                wire_error_code: 0,
                remaining_credits: 0,
                accepted_sequence: i,
            });
        }
        assert_eq!(queue.len(), QUEUE_CAPACITY, "the queue is bounded");
        assert_eq!(queue.take_dropped(), 5, "and says how many it dropped");
        assert_eq!(queue.take_dropped(), 0, "taking clears the count");

        let mut out = [0u8; 4096];
        let written = queue.drain_into(&mut out);
        let read = decode_bytes(&out[..written]).expect("round trip");
        let first = match read[0] {
            DownlinkRecord::FrameVerdict {
                accepted_sequence, ..
            } => accepted_sequence,
            other => panic!("expected a verdict, got {other:?}"),
        };
        assert_eq!(first, 5, "the oldest went, so the newest survived");
    }

    #[test]
    fn a_buffer_too_small_for_one_record_sends_nothing_and_keeps_everything() {
        let mut queue = DownlinkQueue::new();
        queue.push_verdict(verdict());
        // Room for the envelope and nothing else.
        let mut out = [0u8; ENVELOPE_WORDS * 4 + 4];
        assert_eq!(
            queue.drain_into(&mut out),
            0,
            "a message with no records would say `no news` while news is queued"
        );
        assert_eq!(queue.len(), 1, "and the record is still there to send");
    }

    #[test]
    fn a_partial_drain_leaves_the_rest_in_order() {
        let mut queue = DownlinkQueue::new();
        for i in 0..4 {
            queue.push_verdict(DownlinkRecord::FrameVerdict {
                generation: 1,
                decision: 1,
                wire_error_code: 0,
                remaining_credits: 0,
                accepted_sequence: i,
            });
        }
        // Envelope plus exactly two verdicts.
        let mut out = [0u8; (ENVELOPE_WORDS + 2 * FRAME_VERDICT_WORDS as usize) * 4];
        let written = queue.drain_into(&mut out);
        let read = decode_bytes(&out[..written]).expect("round trip");
        assert_eq!(read.len(), 2, "only what fits goes");
        assert_eq!(queue.len(), 2, "and the rest waits");

        let mut rest = [0u8; 256];
        let written = queue.drain_into(&mut rest);
        let read = decode_bytes(&rest[..written]).expect("round trip");
        assert_eq!(read.len(), 2, "the remainder comes next");
        assert!(
            matches!(
                read[0],
                DownlinkRecord::FrameVerdict {
                    accepted_sequence: 2,
                    ..
                }
            ),
            "in order: {read:?}"
        );
    }

    #[test]
    fn an_empty_queue_produces_no_message() {
        let mut queue = DownlinkQueue::new();
        let mut out = [0u8; 256];
        assert_eq!(queue.drain_into(&mut out), 0);
    }

    #[test]
    fn the_declared_word_counts_match_what_is_written() {
        for record in [verdict(), tick()] {
            let mut words = Vec::new();
            record.write_words(&mut words);
            assert_eq!(
                words.len() as u32,
                record.word_count(),
                "a record's declared length has to be the length it writes, or the \
                 reader walks into the next record"
            );
        }
    }
}
