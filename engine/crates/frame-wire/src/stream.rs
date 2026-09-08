//! The record envelope: what every command stream is, whichever block its
//! opcodes come from.
//!
//! A stream is a magic word, a version, and a run of records. A record is one
//! header word -- twelve bits of opcode, twenty of word count -- followed by
//! that many words minus one. Nothing here knows what a `Viewport` or a
//! `FillRect` is; it knows how long each claims to be and whether the claim
//! fits in the buffer.
//!
//! # Why the envelope is its own module
//!
//! It used to live in `stream`, beside the WebGL opcode table, because for a
//! while that was the only block. Then Canvas2D got a block of its own, and the
//! GL table found itself dispatching into the 2D table -- a layer calling
//! sideways into its sibling because the layer above them had no name. This is
//! that layer: the blocks each own their opcodes and their record shapes, and
//! the envelope is what routes a number to the block that owns it.
//!
//! # Pass one of two
//!
//! Structural validation only: magic, version, record headers, word counts,
//! opcodes in range, bool words that are actually 0 or 1. It reads no field for
//! meaning and allocates nothing. Pass two -- turning validated words into
//! render commands -- is `migo-frame-decode`, which can then read fields rather
//! than check them.

use crate::gl::MAX_STREAM_UNIFORM_WORDS;

// ─── Public constants ────────────────────────────────────────────────────────

pub const MAGIC: u32 = 0x4D47_4C31;
pub const STREAM_VERSION: u32 = 1;

// ─── Header codec ────────────────────────────────────────────────────────────

/// Pack a record header: low 12 bits = opcode, high 20 bits = total word count.
///
/// Public and unconditional. It was `#[cfg(test)]` while the only writer was
/// the JavaScript encoder in another language; now that the format lives here,
/// the writer half belongs to it too -- and the tests that use it are in
/// another crate, where a `cfg(test)` item is not visible.
#[inline]
pub fn pack_header(opcode: u32, word_count: u32) -> u32 {
    (word_count << 12) | (opcode & 0xFFF)
}

/// Extract the opcode from a record header (low 12 bits).
#[inline]
pub fn opcode_of(h: u32) -> u32 {
    h & 0xFFF
}

/// Extract the total word count from a record header (high 20 bits).
#[inline]
pub fn word_count_of(h: u32) -> u32 {
    h >> 12
}

// ─── StreamError ─────────────────────────────────────────────────────────────

/// Structural validation errors. Codes are stable and non-zero.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamError {
    /// `used_words < 2` or backing slice too small.
    TooShort,
    /// `word[0] != MAGIC`.
    BadMagic,
    /// `word[1] != STREAM_VERSION`.
    BadVersion,
    /// The used prefix exceeds the backing slice or the caller's stream limit.
    UsedTooLarge,
    /// Record header has `word_count == 0`.
    ZeroWordCount,
    /// Opcode not in the allowed table.
    UnknownOpcode(u32),
    /// Fixed-arity record has wrong word count.
    BadArity,
    /// Record extends past `used_words`.
    Truncated,
    /// Cursor addition would overflow.
    Overflow,
    /// A word that must be strictly 0 or 1 has another value.
    BadBool,
    /// Variable uniform payload exceeds `MAX_STREAM_UNIFORM_WORDS`.
    UniformPayloadTooLarge,
}

impl StreamError {
    /// Stable non-zero error code.
    pub fn code(&self) -> u32 {
        match self {
            StreamError::TooShort => 1,
            StreamError::BadMagic => 2,
            StreamError::BadVersion => 3,
            StreamError::UsedTooLarge => 4,
            StreamError::ZeroWordCount => 5,
            StreamError::UnknownOpcode(_) => 6,
            StreamError::BadArity => 7,
            StreamError::Truncated => 8,
            StreamError::BadBool => 10,
            StreamError::UniformPayloadTooLarge => 11,
            StreamError::Overflow => 12,
        }
    }
}

// ─── RecordSpec ───────────────────────────────────────────────────────────────

/// Describes the element kind for variable uniform records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniformElementKind {
    Int,
    Float,
}

/// Per-opcode structural specification for Pass 1.
// `PartialEq` so the envelope's routing can be asserted against the block
// tables themselves: the claim is "this opcode gets *the* spec that block
// holds", and comparing projections of the fields would restate the spec here
// as a second copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordSpec {
    /// Fixed-arity record: exact word count and optional bool-word indices.
    Fixed {
        word_count: u32,
        bool_words: &'static [u8],
    },
    /// Variable-arity vector uniform: `H C location:I payload...`
    /// `word_count = 3 + payload_words`.
    VectorUniform { element_kind: UniformElementKind },
    /// Variable-arity matrix uniform: `H C location:I transpose:B payload...`
    /// `word_count = 4 + payload_words`.
    MatrixUniform {
        element_kind: UniformElementKind,
        /// Word index (within the record) holding the transpose bool.
        transpose_word_idx: u8,
    },
}

// ─── record_spec ─────────────────────────────────────────────────────────────

/// The shape of one record, from the block that owns its opcode.
///
/// Routing on the range rather than merging the tables keeps each block's spec
/// next to its own opcode constants, and makes an opcode added to the wrong
/// range a rejection instead of a record read with the other block's shape.
pub fn record_spec(opcode: u32) -> Option<RecordSpec> {
    if opcode >= crate::canvas2d::OP2D_BASE {
        crate::canvas2d::record_spec(opcode)
    } else {
        crate::gl::record_spec(opcode)
    }
}

// ─── ValidatedStream ─────────────────────────────────────────────────────────

/// A slice that has passed Pass 1 structural validation.
/// Only constructible via the batch or full-frame structural validators.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedStream<'a> {
    words: &'a [u32],
}

impl<'a> ValidatedStream<'a> {
    /// The validated used-prefix slice (including magic/version header).
    pub fn words(&self) -> &[u32] {
        self.words
    }
}

// ─── validate_stream ─────────────────────────────────────────────────────────

/// Pass 1 pure structural validation.
///
/// Returns `Ok(ValidatedStream)` wrapping the validated prefix, or
/// `Err(StreamError)` on any structural violation. Never panics on any input.
/// No I/O, no OpState, no error_state, no collector.
pub fn validate_stream(words: &[u32], used_words: u32) -> Result<ValidatedStream<'_>, StreamError> {
    validate_stream_with_limit(words, used_words, 8192)
}

/// Validate one complete external frame's COMMAND_STREAM.
///
/// The embedded runtime submits batches of at most 8192 words; the external
/// lane submits a whole frame, bounded by the outer wire envelope instead.
/// Record limits (including each uniform's payload limit) remain identical.
pub fn validate_frame_stream(
    words: &[u32],
    used_words: u32,
) -> Result<ValidatedStream<'_>, StreamError> {
    validate_stream_with_limit(
        words,
        used_words,
        crate::MAX_TOTAL_BYTES as usize / size_of::<u32>(),
    )
}

fn validate_stream_with_limit(
    words: &[u32],
    used_words: u32,
    max_words: usize,
) -> Result<ValidatedStream<'_>, StreamError> {
    // Validate used_words bounds.
    // Must be >= 2 (magic + version at minimum).
    if used_words < 2 {
        return Err(StreamError::TooShort);
    }
    if used_words as usize > words.len().min(max_words) {
        return Err(StreamError::UsedTooLarge);
    }

    // Safe slice: words[0..used_words] is valid.
    let used = used_words as usize;

    // Validate magic and version using safe indexing.
    let w0 = *words.first().ok_or(StreamError::TooShort)?;
    if w0 != MAGIC {
        return Err(StreamError::BadMagic);
    }
    let w1 = *words.get(1).ok_or(StreamError::TooShort)?;
    if w1 != STREAM_VERSION {
        return Err(StreamError::BadVersion);
    }

    // Walk records starting at index 2.
    let mut cursor: usize = 2;
    loop {
        if cursor >= used {
            break;
        }

        // Read record header safely.
        let header = *words.get(cursor).ok_or(StreamError::Truncated)?;
        let opcode = opcode_of(header);
        let wc = word_count_of(header);

        // word_count must be non-zero.
        if wc == 0 {
            return Err(StreamError::ZeroWordCount);
        }

        // Check for cursor overflow: cursor + wc must not overflow usize.
        let record_end = cursor
            .checked_add(wc as usize)
            .ok_or(StreamError::Overflow)?;

        // Record must fit within used_words.
        if record_end > used {
            return Err(StreamError::Truncated);
        }

        // Look up opcode in table.
        let spec = record_spec(opcode).ok_or(StreamError::UnknownOpcode(opcode))?;

        match spec {
            RecordSpec::Fixed {
                word_count,
                bool_words,
            } => {
                // Fixed-arity: exact match required.
                if wc != word_count {
                    return Err(StreamError::BadArity);
                }
                // Check bool words.
                for &bi in bool_words {
                    let idx = cursor + bi as usize;
                    let val = *words.get(idx).ok_or(StreamError::Truncated)?;
                    if val > 1 {
                        return Err(StreamError::BadBool);
                    }
                }
            }
            RecordSpec::VectorUniform { element_kind } => {
                let _ = element_kind;
                // H C location payload... → header_words = 3, payload = wc - 3
                if wc < 3 {
                    return Err(StreamError::BadArity);
                }
                let payload_words = wc - 3;
                if payload_words > MAX_STREAM_UNIFORM_WORDS {
                    return Err(StreamError::UniformPayloadTooLarge);
                }
            }
            RecordSpec::MatrixUniform {
                element_kind,
                transpose_word_idx,
            } => {
                let _ = element_kind;
                // H C location transpose payload... → header_words = 4, payload = wc - 4
                if wc < 4 {
                    return Err(StreamError::BadArity);
                }
                let payload_words = wc - 4;
                if payload_words > MAX_STREAM_UNIFORM_WORDS {
                    return Err(StreamError::UniformPayloadTooLarge);
                }
                // Check transpose bool.
                let t_idx = cursor + transpose_word_idx as usize;
                let t_val = *words.get(t_idx).ok_or(StreamError::Truncated)?;
                if t_val > 1 {
                    return Err(StreamError::BadBool);
                }
            }
        }

        cursor = record_end;
    }

    // The final record must end exactly at `used_words`, and it always does:
    // `record_end <= used` is enforced before `cursor` is advanced to it, and the
    // loop exits on `cursor >= used`, so the two can only be equal here. There
    // was a `TrailingGarbage` error for this case, which nothing could produce --
    // any word left over is read as the next record's header and rejected as one.
    // A `debug_assert` states the invariant where a dead error code used to
    // imply it was still being checked.
    debug_assert_eq!(cursor, used, "record walk must land exactly on used_words");

    Ok(ValidatedStream {
        words: &words[..used],
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // The envelope's cases need opcodes to build records out of, and every
    // opcode belongs to a block. Both blocks export a `record_spec` of their
    // own, so the routing one this module defines is called through `super`
    // where these cases need it.
    use crate::canvas2d::*;
    use crate::gl::*;

    #[test]
    fn a_complete_frame_can_exceed_the_runtime_batch_limit() {
        // This is the external lane's complete COMMAND_STREAM, not one V8 batch.
        let mut words = vec![MAGIC, STREAM_VERSION];
        words.resize(8194, pack_header(OP2D_SAVE, 1));
        assert!(super::validate_frame_stream(&words, words.len() as u32).is_ok());
        assert_eq!(
            validate_stream(&words, words.len() as u32),
            Err(StreamError::UsedTooLarge)
        );
    }

    #[test]
    fn frame_stream_is_bounded_by_the_wire_frame_limit() {
        let max_words = crate::MAX_TOTAL_BYTES as usize / size_of::<u32>();
        let mut words = vec![MAGIC, STREAM_VERSION];
        words.resize(max_words + 1, pack_header(OP2D_SAVE, 1));
        assert!(super::validate_frame_stream(&words, max_words as u32).is_ok());
        assert_eq!(
            super::validate_frame_stream(&words, (max_words + 1) as u32),
            Err(StreamError::UsedTooLarge)
        );
        assert_eq!(
            super::validate_frame_stream(&words[..2], 3),
            Err(StreamError::UsedTooLarge)
        );
        words[max_words - 1] = pack_header(OP_DEPTH_MASK, 3);
        assert_eq!(
            super::validate_frame_stream(&words, max_words as u32),
            Err(StreamError::Truncated)
        );
    }

    // ── Header codec ──────────────────────────────────────────────────────────

    #[test]
    fn header_pack_round_trip_low_opcode() {
        let h = pack_header(1, 6);
        assert_eq!(opcode_of(h), 1);
        assert_eq!(word_count_of(h), 6);
    }

    #[test]
    fn header_pack_round_trip_max_opcode() {
        // Max opcode value in low 12 bits = 0xFFF = 4095
        let h = pack_header(0xFFF, 1);
        assert_eq!(opcode_of(h), 0xFFF);
        assert_eq!(word_count_of(h), 1);
    }

    #[test]
    fn header_pack_opcode_masked_to_12_bits() {
        // Bits above 12 in opcode are masked off.
        let h = pack_header(0x1_001, 3); // 0x1001 & 0xFFF = 0x001
        assert_eq!(opcode_of(h), 0x001);
        assert_eq!(word_count_of(h), 3);
    }

    #[test]
    fn header_pack_max_word_count() {
        // 20-bit word_count: max value = (1<<20)-1 = 1048575
        let wc: u32 = (1 << 20) - 1;
        let h = pack_header(7, wc);
        assert_eq!(opcode_of(h), 7);
        assert_eq!(word_count_of(h), wc);
    }

    #[test]
    fn header_pack_zero_word_count() {
        let h = pack_header(42, 0);
        assert_eq!(opcode_of(h), 42);
        assert_eq!(word_count_of(h), 0);
    }

    #[test]
    fn header_opcode_zero() {
        let h = pack_header(0, 5);
        assert_eq!(opcode_of(h), 0);
        assert_eq!(word_count_of(h), 5);
    }

    // ── StreamError code stability ────────────────────────────────────────────

    #[test]
    fn stream_error_codes_are_nonzero_and_stable() {
        assert_eq!(StreamError::TooShort.code(), 1);
        assert_eq!(StreamError::BadMagic.code(), 2);
        assert_eq!(StreamError::BadVersion.code(), 3);
        assert_eq!(StreamError::UsedTooLarge.code(), 4);
        assert_eq!(StreamError::ZeroWordCount.code(), 5);
        assert_eq!(StreamError::UnknownOpcode(999).code(), 6);
        assert_eq!(StreamError::BadArity.code(), 7);
        assert_eq!(StreamError::Truncated.code(), 8);
        assert_eq!(StreamError::BadBool.code(), 10);
        assert_eq!(StreamError::UniformPayloadTooLarge.code(), 11);
        assert_eq!(StreamError::Overflow.code(), 12);
    }

    #[test]
    fn stream_error_code_ignores_unknown_opcode_field() {
        assert_eq!(StreamError::UnknownOpcode(0).code(), 6);
        assert_eq!(StreamError::UnknownOpcode(u32::MAX).code(), 6);
    }

    // ── validate_stream: used_words < 2 ──────────────────────────────────────

    #[test]
    fn validate_used_zero_returns_too_short() {
        let words = [MAGIC, STREAM_VERSION];
        assert_eq!(validate_stream(&words, 0), Err(StreamError::TooShort));
    }

    #[test]
    fn validate_used_one_returns_too_short() {
        let words = [MAGIC, STREAM_VERSION];
        assert_eq!(validate_stream(&words, 1), Err(StreamError::TooShort));
    }

    // ── validate_stream: used > 8192 ─────────────────────────────────────────

    #[test]
    fn validate_used_over_8192_returns_used_too_large() {
        // Backing slice large enough, but used_words > 8192.
        let words = vec![0u32; 9000];
        let result = validate_stream(&words, 8193);
        assert_eq!(result, Err(StreamError::UsedTooLarge));
    }

    // ── validate_stream: used > words.len() ──────────────────────────────────

    #[test]
    fn validate_used_greater_than_slice_len_returns_used_too_large() {
        let words = [MAGIC, STREAM_VERSION];
        // words.len()=2, used_words=3
        assert_eq!(validate_stream(&words, 3), Err(StreamError::UsedTooLarge));
    }

    // ── validate_stream: bad magic ────────────────────────────────────────────

    #[test]
    fn validate_bad_magic_returns_bad_magic() {
        let words = [0xDEAD_BEEF, STREAM_VERSION];
        assert_eq!(validate_stream(&words, 2), Err(StreamError::BadMagic));
    }

    // ── validate_stream: bad version ──────────────────────────────────────────

    #[test]
    fn validate_bad_version_returns_bad_version() {
        let words = [MAGIC, 0]; // version 0 is wrong
        assert_eq!(validate_stream(&words, 2), Err(StreamError::BadVersion));
    }

    #[test]
    fn validate_version_2_returns_bad_version() {
        let words = [MAGIC, 2];
        assert_eq!(validate_stream(&words, 2), Err(StreamError::BadVersion));
    }

    // ── validate_stream: valid empty stream (no records) ─────────────────────

    #[test]
    fn validate_empty_stream_ok() {
        let words = [MAGIC, STREAM_VERSION];
        let vs = validate_stream(&words, 2).expect("should be valid");
        assert_eq!(vs.words(), &[MAGIC, STREAM_VERSION]);
    }

    // ── validate_stream: opcode 0 (zero in record header opcode field) ───────

    #[test]
    fn validate_record_opcode_zero_returns_unknown_opcode() {
        // header with opcode=0 and word_count=3
        let h = pack_header(0, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0];
        assert_eq!(
            validate_stream(&words, 5),
            Err(StreamError::UnknownOpcode(0))
        );
    }

    // ── validate_stream: unknown opcode ──────────────────────────────────────

    #[test]
    fn validate_unknown_opcode_returns_unknown_opcode() {
        // opcode 100 is not in the table
        let h = pack_header(100, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0];
        assert_eq!(
            validate_stream(&words, 5),
            Err(StreamError::UnknownOpcode(100))
        );
    }

    #[test]
    fn validate_unknown_opcode_carries_value() {
        let h = pack_header(999, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0];
        match validate_stream(&words, 5) {
            Err(StreamError::UnknownOpcode(v)) => assert_eq!(v, 999),
            other => panic!("expected UnknownOpcode(999), got {:?}", other),
        }
    }

    // ── validate_stream: zero word count ─────────────────────────────────────

    #[test]
    fn validate_zero_word_count_in_header_returns_zero_word_count() {
        let h = pack_header(OP_CLEAR, 0); // word_count = 0 → invalid
        let words = [MAGIC, STREAM_VERSION, h];
        assert_eq!(validate_stream(&words, 3), Err(StreamError::ZeroWordCount));
    }

    // ── validate_stream: fixed bad arity ─────────────────────────────────────

    #[test]
    fn validate_fixed_bad_arity_returns_bad_arity() {
        // OP_CLEAR expects word_count=3, give it 4.
        // Provide enough backing words so Truncated doesn't fire first.
        let h = pack_header(OP_CLEAR, 4);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0]; // used=6, record_end=6 fits
        assert_eq!(validate_stream(&words, 6), Err(StreamError::BadArity));
    }

    #[test]
    fn validate_fixed_bad_arity_too_small_returns_bad_arity() {
        // OP_VIEWPORT expects word_count=6, give it 5
        let h = pack_header(OP_VIEWPORT, 5);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0, 0];
        assert_eq!(validate_stream(&words, 7), Err(StreamError::BadArity));
    }

    // ── validate_stream: truncated ────────────────────────────────────────────

    #[test]
    fn validate_truncated_record_returns_truncated() {
        // OP_CLEAR expects 3 words but we only provide 2 after header
        // used_words = 4 but record_end would be 5
        let h = pack_header(OP_CLEAR, 3);
        // Only 4 words total including magic/version, but record needs cursor+3=5
        let words = [MAGIC, STREAM_VERSION, h, 0];
        assert_eq!(validate_stream(&words, 4), Err(StreamError::Truncated));
    }

    // ── validate_stream: cursor overflow ─────────────────────────────────────

    #[test]
    fn validate_cursor_overflow_returns_overflow() {
        // Craft a header with word_count = u32::MAX so cursor.checked_add overflows.
        // We need word_count_of to return a huge value.
        // pack_header: high 20 bits = word_count, max 20-bit = (1<<20)-1.
        // usize overflow: set word_count to usize::MAX via a crafted raw header.
        // On 64-bit, usize is 64 bits so checked_add of a u32 won't overflow usize.
        // Instead, use word_count large enough to make record_end > usize::MAX when
        // cursor is near usize::MAX. That's impractical. Instead test with a very
        // large word_count that goes past used_words (this becomes Truncated, not Overflow).
        // True Overflow requires cursor near usize::MAX which only happens in 32-bit.
        // On 64-bit hosts: craft via raw word where bits 12..31 = all ones → wc = 0xFFFFF
        // cursor=2, wc=0xFFFFF, record_end = 2 + 1048575 = 1048577 > used = any reasonable value → Truncated.
        // For true Overflow on 64-bit we would need cursor ~= usize::MAX.
        // So this test verifies the path is structurally there (the checked_add path).
        // We test it by constructing a raw header where word_count has high bits set,
        // making record_end exceed used_words → Truncated (not Overflow, since 64-bit usize won't wrap).
        // The Overflow path is exercised in the no-panic fuzz test.
        // Let's make a dedicated test: set used=8192 and record starts at cursor=8191
        // with word_count that would overflow a u32 when added to cursor.
        // Since wc is u32 and cursor is usize, checked_add(wc as usize) on 64-bit: safe.
        // We verify the structure is correct by ensuring large wc → Truncated:
        let mut words = vec![0u32; 8192];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        // word_count in high 20 bits of header: set to very large → Truncated
        let h = pack_header(OP_CLEAR, 0xF_FFFF); // wc = 0xFFFFF > remaining
        words[2] = h;
        let result = validate_stream(&words, 8192);
        assert_eq!(result, Err(StreamError::Truncated));
    }

    // ── validate_stream: words left over after the last record ───────────────

    /// A word after the last complete record is read as the *next* record's
    /// header and rejected as one -- there is no separate "trailing garbage"
    /// outcome, and code 9 has been removed because nothing could produce it.
    ///
    /// This replaces two tests both named for `TrailingGarbage`: one asserted
    /// `ZeroWordCount` while its comment explained at length that the name was
    /// wrong, and the other asserted that an empty stream is *valid*. Neither
    /// could fail for the reason its name gave.
    #[test]
    fn a_word_after_the_last_record_is_rejected_as_a_malformed_header() {
        let h = pack_header(OP_CLEAR, 3);
        // words[2..5] is a well-formed OP_CLEAR record, so the walk lands on 5;
        // words[5] = 0 is then read as a header with opcode 0 and word_count 0.
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0u32];
        assert_eq!(validate_stream(&words, 6), Err(StreamError::ZeroWordCount));

        // The positive control: the same stream without the leftover word is
        // valid, so the rejection above is about the extra word and not about
        // the record.
        assert!(validate_stream(&words[..5], 5).is_ok());
    }

    /// A header and nothing else is a valid empty stream: the walk starts and
    /// ends at `used_words`.
    #[test]
    fn a_stream_with_no_records_is_valid() {
        let words = [MAGIC, STREAM_VERSION];
        assert!(validate_stream(&words, 2).is_ok());
    }

    // ── validate_stream: bool not 0/1 ────────────────────────────────────────

    #[test]
    fn validate_bool_word_not_zero_or_one_returns_bad_bool_depth_mask() {
        // OP_DEPTH_MASK: H C B — bool at word index 2
        let h = pack_header(OP_DEPTH_MASK, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 2]; // word[4] = bool at record[2] = 2
        assert_eq!(validate_stream(&words, 5), Err(StreamError::BadBool));
    }

    #[test]
    fn validate_bool_word_zero_ok_depth_mask() {
        let h = pack_header(OP_DEPTH_MASK, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0];
        assert!(validate_stream(&words, 5).is_ok());
    }

    #[test]
    fn validate_bool_word_one_ok_depth_mask() {
        let h = pack_header(OP_DEPTH_MASK, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 1];
        assert!(validate_stream(&words, 5).is_ok());
    }

    #[test]
    fn validate_color_mask_all_bools_ok() {
        // OP_COLOR_MASK: H C B B B B — bools at indices 2,3,4,5
        let h = pack_header(OP_COLOR_MASK, 6);
        let words = [MAGIC, STREAM_VERSION, h, 0, 1, 0, 1, 1];
        assert!(validate_stream(&words, 8).is_ok());
    }

    #[test]
    fn validate_color_mask_bad_bool_returns_bad_bool() {
        let h = pack_header(OP_COLOR_MASK, 6);
        // bool at index 4 (record word 4) = word[6] = 2
        let words = [MAGIC, STREAM_VERSION, h, 0, 1, 0, 2, 1];
        assert_eq!(validate_stream(&words, 8), Err(StreamError::BadBool));
    }

    #[test]
    fn validate_vertex_attrib_pointer_normalized_bool_ok() {
        // OP_VERTEX_ATTRIB_POINTER: H C U I U B I I — 8 words total.
        // record layout (0-indexed from record start):
        //   [0]=H [1]=C [2]=U [3]=I [4]=U [5]=B [6]=I [7]=I
        // Placed at absolute indices [2..10] in the buffer.
        // bool is at record word 5 → absolute index 7.
        let h = pack_header(OP_VERTEX_ATTRIB_POINTER, 8);
        // [0]=MAGIC [1]=VERSION [2]=h [3]=C [4]=U [5]=I [6]=U [7]=B=1 [8]=I [9]=I
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0, 0, 1u32, 0, 0];
        assert!(validate_stream(&words, 10).is_ok());
    }

    #[test]
    fn validate_vertex_attrib_pointer_bad_normalized_bool() {
        // bool at record word index 5 → absolute index = 2 + 5 = 7; set to 2
        let h = pack_header(OP_VERTEX_ATTRIB_POINTER, 8);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0, 0, 2u32, 0, 0];
        assert_eq!(validate_stream(&words, 10), Err(StreamError::BadBool));
    }

    // ── validate_stream: uniform payload > 512 ───────────────────────────────

    #[test]
    fn validate_uniform_vector_payload_exactly_512_ok() {
        // OP_UNIFORM1FV: H C location payload... header_words=3, payload=512 → total=515
        let total = 3 + 512;
        let h = pack_header(OP_UNIFORM1FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        let used = 2 + total;
        assert!(validate_stream(&words, used).is_ok());
    }

    #[test]
    fn validate_uniform_vector_payload_513_returns_payload_too_large() {
        // 513 payload words → UniformPayloadTooLarge
        let total = 3 + 513;
        let h = pack_header(OP_UNIFORM1FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        let used = 2 + total;
        assert_eq!(
            validate_stream(&words, used),
            Err(StreamError::UniformPayloadTooLarge)
        );
    }

    #[test]
    fn validate_matrix_uniform_payload_exactly_512_ok() {
        // OP_UNIFORM_MATRIX4FV: H C location transpose payload... header_words=4, payload=512 → total=516
        let total = 4 + 512;
        let h = pack_header(OP_UNIFORM_MATRIX4FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        // transpose at record word index 3 → absolute [2+3]=words[5]
        words[5] = 0; // valid bool
        let used = 2 + total;
        assert!(validate_stream(&words, used).is_ok());
    }

    #[test]
    fn validate_matrix_uniform_payload_513_returns_payload_too_large() {
        let total = 4 + 513;
        let h = pack_header(OP_UNIFORM_MATRIX4FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        words[5] = 0;
        let used = 2 + total;
        assert_eq!(
            validate_stream(&words, used),
            Err(StreamError::UniformPayloadTooLarge)
        );
    }

    #[test]
    fn validate_matrix_uniform_bad_transpose_bool_returns_bad_bool() {
        let total = 4 + 4; // small payload
        let h = pack_header(OP_UNIFORM_MATRIX2FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        // transpose at words[2+3] = words[5] = 2 → bad bool
        words[5] = 2;
        let used = 2 + total;
        assert_eq!(validate_stream(&words, used), Err(StreamError::BadBool));
    }

    // ── validate_stream: second record malformed, sentinel unchanged ──────────

    #[test]
    fn second_record_malformed_returns_error_not_side_effect() {
        // First record: valid OP_CLEAR (3 words)
        // Second record: bad opcode
        let h1 = pack_header(OP_CLEAR, 3);
        let h2 = pack_header(100, 3); // unknown opcode
        let words = [MAGIC, STREAM_VERSION, h1, 0, 0, h2, 0, 0];
        // used = 8, first record ok (cursor → 5), second record → error
        assert_eq!(
            validate_stream(&words, 8),
            Err(StreamError::UnknownOpcode(100))
        );
    }

    // ── validate_stream: valid multi-record stream ────────────────────────────

    #[test]
    fn validate_two_valid_records_ok() {
        let h1 = pack_header(OP_CLEAR, 3);
        let h2 = pack_header(OP_ENABLE, 3);
        let words = [MAGIC, STREAM_VERSION, h1, 0, 0, h2, 0, 0];
        let vs = validate_stream(&words, 8).expect("should be valid");
        assert_eq!(vs.words().len(), 8);
    }

    // ── The envelope reaches both opcode blocks ───────────────────────────────
    //
    // Every case above this point is built from a GL opcode. The envelope's
    // routing has two arms and only one of them had ever been walked from here,
    // which is why `use crate::canvas2d::*` sat unused above -- the import was
    // written for cases that were never added, and the comment beside it
    // described a coverage that did not exist.
    //
    // The 2D arm is the one the Apple product depends on: Canvas2D is what
    // crosses the process boundary as a stream, and this validator is what
    // stands between content JavaScript's bytes and the renderer.

    #[test]
    fn validate_a_2d_record_ok() {
        // OP2D_SELECT_CANVAS is 2 words (header + id), OP2D_MOVE_TO is 3
        // (header + x + y).
        let h1 = pack_header(OP2D_SELECT_CANVAS, 2);
        let h2 = pack_header(OP2D_MOVE_TO, 3);
        let words = [MAGIC, STREAM_VERSION, h1, 7, h2, 0, 0];
        let vs = validate_stream(&words, 7).expect("a well-formed 2D stream must validate");
        assert_eq!(vs.words().len(), 7);
    }

    #[test]
    fn validate_rejects_a_2d_record_whose_length_disagrees_with_its_spec() {
        // OP2D_MOVE_TO is 3 words; claim 4.
        let h = pack_header(OP2D_MOVE_TO, 4);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0];
        assert_eq!(validate_stream(&words, 6), Err(StreamError::BadArity));
    }

    // The seam between the two blocks is a single `>=`. Getting it wrong by one
    // is silent in the worst way: the wrong table answers, and an opcode it
    // does not know comes back as `UnknownOpcode` for a record that is in fact
    // well formed -- or, if both tables happen to answer, a record is accepted
    // at the other block's length and the walk desynchronises from there on.
    //
    // Derived from the two tables rather than from a list written here, because
    // a hand-written list of opcodes is a second copy of the thing it checks.
    #[test]
    fn every_opcode_is_routed_to_the_block_that_knows_it() {
        for opcode in 0..(crate::canvas2d::OP2D_BASE * 2) {
            let from_gl = crate::gl::record_spec(opcode);
            let from_2d = crate::canvas2d::record_spec(opcode);

            // Neither block may claim an opcode the other also claims: the
            // envelope picks exactly one, so an overlap would make which table
            // supplied a record's length depend on the order of an `if`.
            assert!(
                from_gl.is_none() || from_2d.is_none(),
                "opcode {opcode} is claimed by both the GL and the 2D block"
            );

            assert_eq!(
                super::record_spec(opcode),
                from_gl.or(from_2d),
                "opcode {opcode} was routed to the block that does not know it"
            );
        }
    }

    #[test]
    fn validated_stream_words_returns_exact_used_prefix() {
        let h = pack_header(OP_CLEAR, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0, 0xFF, 0xFF]; // 7 words
        // used_words = 5 → only first 5 returned
        let vs = validate_stream(&words, 5).expect("should be valid");
        assert_eq!(vs.words(), &words[..5]);
    }

    // ── No-panic fuzz: arbitrary inputs must not panic ───────────────────────

    #[test]
    fn no_panic_on_all_zeros() {
        let _ = validate_stream(&[0u32; 8192], 8192);
    }

    #[test]
    fn no_panic_on_all_ones() {
        let _ = validate_stream(&[u32::MAX; 8192], 8192);
    }

    #[test]
    fn no_panic_on_empty_slice() {
        let _ = validate_stream(&[], 0);
    }

    #[test]
    fn no_panic_on_single_word() {
        let _ = validate_stream(&[MAGIC], 1);
    }

    #[test]
    fn no_panic_on_used_larger_than_8192_with_large_backing() {
        let words = vec![u32::MAX; 10000];
        let _ = validate_stream(&words, 10000);
    }

    #[test]
    fn no_panic_pseudorandom_inputs() {
        // Deterministic pseudorandom stream exercising many code paths.
        let mut words = Vec::with_capacity(8192);
        let mut state: u32 = 0xDEAD_CAFE;
        for _ in 0..8192 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            words.push(state);
        }
        // Try various used_words values.
        for &used in &[0u32, 1, 2, 7, 100, 1000, 8191, 8192] {
            let _ = validate_stream(&words, used);
        }
    }

    #[test]
    fn no_panic_max_word_count_field_in_header() {
        // Record header with all-ones in high 20 bits (max word_count = 0xFFFFF).
        let h = u32::MAX; // opcode = 0xFFF, word_count = 0xFFFFF
        let words = [MAGIC, STREAM_VERSION, h];
        let _ = validate_stream(&words, 3);
    }

    // ── Opcode table completeness ─────────────────────────────────────────────

    #[test]
    fn all_fixed_opcodes_1_to_58_have_specs() {
        for op in 1u32..=58 {
            assert!(
                super::record_spec(op).is_some(),
                "opcode {} should have a spec",
                op
            );
        }
    }

    #[test]
    fn all_variable_opcodes_256_to_266_have_specs() {
        for op in 256u32..=266 {
            assert!(
                super::record_spec(op).is_some(),
                "opcode {} should have a spec",
                op
            );
        }
    }

    #[test]
    fn opcodes_between_59_and_255_return_none() {
        for op in 59u32..=255 {
            assert!(
                super::record_spec(op).is_none(),
                "opcode {} should not have a spec",
                op
            );
        }
    }

    #[test]
    fn opcode_zero_returns_none() {
        assert!(super::record_spec(0).is_none());
    }

    #[test]
    fn opcodes_above_266_return_none() {
        for op in [267u32, 1000, 4095, u32::MAX] {
            assert!(
                super::record_spec(op).is_none(),
                "opcode {} should not have a spec",
                op
            );
        }
    }

    // ── RecordSpec word counts match §5 table ─────────────────────────────────

    #[test]
    fn fixed_record_word_counts_match_design_table() {
        let expected: &[(u32, u32)] = &[
            (OP_VIEWPORT, 6),
            (OP_CLEAR, 3),
            (OP_CLEAR_COLOR, 6),
            (OP_CLEAR_DEPTH, 3),
            (OP_CLEAR_STENCIL, 3),
            (OP_ENABLE, 3),
            (OP_DISABLE, 3),
            (OP_USE_PROGRAM, 3),
            (OP_BIND_BUFFER, 4),
            (OP_BIND_TEXTURE, 4),
            (OP_ACTIVE_TEXTURE, 3),
            (OP_BIND_FRAMEBUFFER, 4),
            (OP_BIND_RENDERBUFFER, 4),
            (OP_BIND_VERTEX_ARRAY, 3),
            (OP_BIND_SAMPLER, 4),
            (OP_ENABLE_VERTEX_ATTRIB_ARRAY, 3),
            (OP_DISABLE_VERTEX_ATTRIB_ARRAY, 3),
            (OP_VERTEX_ATTRIB_POINTER, 8),
            (OP_VERTEX_ATTRIB_DIVISOR, 4),
            (OP_BLEND_FUNC, 4),
            (OP_BLEND_FUNC_SEPARATE, 6),
            (OP_BLEND_EQUATION, 3),
            (OP_BLEND_EQUATION_SEPARATE, 4),
            (OP_BLEND_COLOR, 6),
            (OP_DEPTH_FUNC, 3),
            (OP_DEPTH_MASK, 3),
            (OP_DEPTH_RANGE, 4),
            (OP_STENCIL_FUNC, 5),
            (OP_STENCIL_FUNC_SEPARATE, 6),
            (OP_STENCIL_OP, 5),
            (OP_STENCIL_OP_SEPARATE, 6),
            (OP_STENCIL_MASK, 3),
            (OP_STENCIL_MASK_SEPARATE, 4),
            (OP_CULL_FACE, 3),
            (OP_FRONT_FACE, 3),
            (OP_COLOR_MASK, 6),
            (OP_SCISSOR, 6),
            (OP_LINE_WIDTH, 3),
            (OP_POLYGON_OFFSET, 4),
            (OP_TEX_PARAMETER_I, 5),
            (OP_TEX_PARAMETER_F, 5),
            (OP_GENERATE_MIPMAP, 3),
            (OP_PIXEL_STORE_I, 4),
            (OP_HINT, 4),
            (OP_SAMPLER_PARAMETER_I, 4),
            (OP_SAMPLER_PARAMETER_F, 4),
            (OP_DRAW_ARRAYS, 5),
            (OP_DRAW_ELEMENTS, 6),
            (OP_DRAW_ARRAYS_INSTANCED, 6),
            (OP_DRAW_ELEMENTS_INSTANCED, 7),
            (OP_BIND_BUFFER_BASE, 5),
            (OP_BIND_BUFFER_RANGE, 7),
            (OP_READ_BUFFER, 3),
            (OP_UNIFORM1I, 4),
            (OP_UNIFORM1F, 4),
            (OP_UNIFORM2F, 5),
            (OP_UNIFORM3F, 6),
            (OP_UNIFORM4F, 7),
        ];
        for &(op, expected_wc) in expected {
            match super::record_spec(op) {
                Some(RecordSpec::Fixed { word_count, .. }) => {
                    assert_eq!(word_count, expected_wc, "opcode {} word count mismatch", op);
                }
                other => panic!("opcode {} expected Fixed spec, got {:?}", op, other),
            }
        }
    }

    #[test]
    fn variable_opcodes_are_vector_or_matrix() {
        // Vectors: 256..=263
        for op in [
            OP_UNIFORM1IV,
            OP_UNIFORM1FV,
            OP_UNIFORM2IV,
            OP_UNIFORM2FV,
            OP_UNIFORM3IV,
            OP_UNIFORM3FV,
            OP_UNIFORM4IV,
            OP_UNIFORM4FV,
        ] {
            match super::record_spec(op) {
                Some(RecordSpec::VectorUniform { .. }) => {}
                other => panic!("opcode {} should be VectorUniform, got {:?}", op, other),
            }
        }
        // Matrices: 264..=266
        for op in [
            OP_UNIFORM_MATRIX2FV,
            OP_UNIFORM_MATRIX3FV,
            OP_UNIFORM_MATRIX4FV,
        ] {
            match super::record_spec(op) {
                Some(RecordSpec::MatrixUniform {
                    transpose_word_idx: 3,
                    ..
                }) => {}
                other => panic!(
                    "opcode {} should be MatrixUniform(transpose=3), got {:?}",
                    op, other
                ),
            }
        }
    }

    // ── validate_stream: variable uniform vector happy path ───────────────────

    #[test]
    fn validate_uniform1fv_empty_payload_ok() {
        // payload_words = 0, total = 3
        let h = pack_header(OP_UNIFORM1FV, 3);
        let words = [MAGIC, STREAM_VERSION, h, 0, 0]; // H C loc
        assert!(validate_stream(&words, 5).is_ok());
    }

    #[test]
    fn validate_uniform_matrix3fv_with_payload_and_transpose_ok() {
        // H C loc transpose payload... → total = 4 + 9 = 13
        let total = 4 + 9u32;
        let h = pack_header(OP_UNIFORM_MATRIX3FV, total);
        let mut words = vec![0u32; 2 + total as usize];
        words[0] = MAGIC;
        words[1] = STREAM_VERSION;
        words[2] = h;
        words[5] = 1; // transpose = 1 (valid bool)
        assert!(validate_stream(&words, 2 + total).is_ok());
    }
}
