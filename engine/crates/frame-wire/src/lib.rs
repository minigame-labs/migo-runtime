//! The `WireFramePacket`: one frame of drawing work, crossing a process
//! boundary, produced by code we do not control.
//!
//! # Why this is not `FramePacket`
//!
//! [`shared::protocol::frame_packet::FramePacket`] is the in-process shape: a
//! Rust enum holding pooled `Vec`s of commands, handed from the JavaScript
//! thread to the render thread. Its comments call that hand-off an IPC, and in
//! process terms it is not one -- both ends are the same address space, so the
//! `Vec` pointers inside it are meaningful on both sides.
//!
//! On iOS the producer is an external JavaScript agent inside WebKit's
//! WebContent process -- a Dedicated Worker in the default topology, though
//! nothing in this crate depends on which agent it is. Nothing in the
//! in-process packet survives that trip: not the pointers, not the enum
//! layout, not the padding. So this crate defines the shape that does --
//! explicit little-endian, offset-driven, self-describing, and validatable
//! without knowing anything about the producer.
//!
//! # Threat model
//!
//! The producer is the game's own JavaScript. It is not hostile by assumption,
//! but it is arbitrary: a bug in a shipped game, or a compromised content
//! bundle, reaches this parser with whatever bytes it likes. So the rules are
//! the ones for parsing anything untrusted:
//!
//! - validate before allocating, and before creating any GPU object;
//! - never panic, never index out of bounds, never loop unboundedly, never
//!   allocate proportionally to an attacker-chosen count;
//! - unknown *required* structure is a rejection, never a silent skip.
//!
//! Validation here touches only the envelope: header, section table, bounds,
//! canonical layout and checksum. The command payload inside a section is
//! validated separately by the stream validator that already exists for WebGL,
//! which is a pure function over `&[u32]`. Keeping the two apart is deliberate:
//! this layer must stay correct without knowing which opcodes exist this month.
//!
//! # One layout per packet
//!
//! The envelope is *canonical*: a given list of sections has exactly one legal
//! encoding. Sections start where the previous one ended, rounded up to eight;
//! the pad bytes are zero; `total_bytes` is the aligned end of the last
//! section. Nothing in a valid packet is unaccounted for.
//!
//! That is stricter than it needs to be to parse, and the strictness is the
//! product. A format that tolerates gaps has room in it -- room the checksum
//! covers and no consumer interprets, which is where a second channel hides.
//! It is also what makes the golden corpus mean something: byte-for-byte
//! agreement between two independent encoders is only a real check when one
//! input has one encoding.
//!
//! # Alignment
//!
//! Nothing here requires the input to be aligned. The bytes arrive from Swift
//! inside `Data.withUnsafeBytes`, whose base pointer carries no alignment
//! guarantee, so every multi-byte read goes through `from_le_bytes` on a byte
//! subslice rather than a pointer cast. A cast would work on every machine we
//! test on and fault on one we do not.

#![forbid(unsafe_code)]

use core::fmt;

/// `MGPF`, matching the convention the WebGL command stream already uses for
/// its own `MGL1`: the magic is compared as the little-endian `u32` the
/// producer writes, not as a byte string.
pub const WIRE_MAGIC: u32 = 0x4D47_5046;

/// Bumped only for a change no v1 reader could survive. Additive change goes
/// through new section kinds, which have their own forward-compatibility rule.
///
/// v1 was refrozen in place on 2026-09-03 after an audit established it had
/// never shipped -- no tag, no `master`, no exported submit entry point. That
/// was the last renumber; see `contracts/frame-wire/wire-v1.md`.
pub const WIRE_VERSION: u32 = 1;

/// Fixed, and validated exactly rather than trusted from the packet: a header
/// that announces its own length lets a producer move the section table.
///
/// 80 is also a multiple of 16, so the 16-byte section table that follows is
/// naturally aligned and the first section payload needs no gap.
pub const HEADER_BYTES: u32 = 80;

/// One section-table entry.
pub const SECTION_ENTRY_BYTES: u32 = 16;

/// Every section starts on an 8-byte boundary so a reader may hand a payload
/// straight to code that wants `u64`-aligned access without copying.
pub const SECTION_ALIGNMENT: u32 = 8;

/// There are five kinds. A packet with more sections than that is malformed by
/// construction, and the cap is what stops a 4-billion-entry table from
/// becoming a loop the parser runs before it notices.
pub const MAX_SECTIONS: u32 = 8;

/// 4 MiB, the absolute parser ceiling, covering the whole packet including the
/// envelope.
///
/// Far above any real frame -- a heavy WebGL frame measures in tens of
/// kilobytes -- and equal to the largest payload class in the device probe
/// matrix, so nothing a product sends can need more. A session may set a
/// *lower* ceiling; see [`ingress::FrameIngress::with_max_packet_bytes`].
/// Neither can be raised at runtime.
pub const MAX_TOTAL_BYTES: u32 = 4 * 1024 * 1024;

/// Section kinds.
///
/// A kind with [`SECTION_KIND_ADVISORY_BIT`] set may be skipped by a reader
/// that does not know it; any other unknown kind is a rejection. Silently
/// ignoring an unknown required section is how a reader ends up drawing a frame
/// that is missing the resource binding it depended on.
pub const SECTION_KIND_COMMAND_STREAM: u32 = 1;
pub const SECTION_KIND_INLINE_DATA: u32 = 2;
pub const SECTION_KIND_RESOURCE_REFERENCES: u32 = 3;
pub const SECTION_KIND_DAMAGE: u32 = 0x8000_0001;
pub const SECTION_KIND_TIMING: u32 = 0x8000_0002;

/// Kinds at or above this are advisory: a reader that does not recognise one
/// skips it. Below it, an unrecognised kind is fatal.
pub const SECTION_KIND_ADVISORY_BIT: u32 = 0x8000_0000;

/// One `u32` resource id per reference.
///
/// Fixed-width, so `item_count` is pinned to `byte_length` exactly rather than
/// merely bounded by it. A count that only fits is a count the consumer loops
/// on next to a length it trusts, and the two disagreeing is how a reader walks
/// off the end of the meaningful data while staying inside the buffer.
pub const RESOURCE_REFERENCE_BYTES: u32 = 4;

/// Four `u32` per advisory damage rectangle.
pub const DAMAGE_RECT_BYTES: u32 = 16;

/// Set when this packet is a complete frame: execute it and end the frame.
///
/// Required on every packet. v1 has no `CONTINUED` flag and no semantic frame
/// continuation: a packet that carries drawing work without ending a frame is a
/// packet whose effects a later one depends on, which turns every question
/// about credits, sequence gaps and generation loss into a question about
/// partial renderer state. Requiring this bit is how the absence is enforced
/// rather than merely intended.
pub const FLAG_PRESENT: u32 = 1 << 0;

pub(crate) const FLAG_KNOWN_MASK: u32 = FLAG_PRESENT;

// Header field offsets. Named rather than open-coded so the layout is one list
// that can be read against the wire format documentation -- and
// `HEADER_LAYOUT` below makes that reading a test rather than a habit.
pub(crate) const OFF_MAGIC: usize = 0;
pub(crate) const OFF_WIRE_VERSION: usize = 4;
pub(crate) const OFF_HEADER_BYTES: usize = 8;
pub(crate) const OFF_TOTAL_BYTES: usize = 12;
pub(crate) const OFF_LAUNCH_NONCE: usize = 16;
pub(crate) const OFF_SEQUENCE: usize = 32;
pub(crate) const OFF_RUNTIME_GENERATION: usize = 40;
pub(crate) const OFF_SURFACE_GENERATION: usize = 48;
pub(crate) const OFF_RESOURCE_EPOCH: usize = 56;
pub(crate) const OFF_FRAME_ID: usize = 64;
pub(crate) const OFF_FLAGS: usize = 68;
pub(crate) const OFF_SECTION_COUNT: usize = 72;
pub(crate) const OFF_CHECKSUM: usize = 76;
pub(crate) const OFF_CHECKSUM_END: usize = OFF_CHECKSUM + 4;

/// One header field, as the wire-format document declares it.
///
/// Exported so the document and this file can be compared field by field
/// instead of by eye. `tests/wire_document_agreement.rs` parses the header
/// table out of `contracts/frame-wire/wire-v1.md` and checks it against this
/// list; a layout change that updates one and not the other turns that test
/// red. The offsets below are the ones `validate` reads from, so the
/// comparison is against the code's actual behaviour rather than a second
/// hand-written copy of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderField {
    pub offset: u32,
    pub size: u32,
    pub name: &'static str,
}

/// The header, in order, with no gaps: the sizes sum to [`HEADER_BYTES`] and
/// each offset is the previous field's end. The agreement test asserts both,
/// so a field cannot be added to one end without accounting for it.
pub const HEADER_LAYOUT: &[HeaderField] = &[
    HeaderField {
        offset: OFF_MAGIC as u32,
        size: 4,
        name: "magic",
    },
    HeaderField {
        offset: OFF_WIRE_VERSION as u32,
        size: 4,
        name: "wire_version",
    },
    HeaderField {
        offset: OFF_HEADER_BYTES as u32,
        size: 4,
        name: "header_bytes",
    },
    HeaderField {
        offset: OFF_TOTAL_BYTES as u32,
        size: 4,
        name: "total_bytes",
    },
    HeaderField {
        offset: OFF_LAUNCH_NONCE as u32,
        size: 16,
        name: "launch_nonce",
    },
    HeaderField {
        offset: OFF_SEQUENCE as u32,
        size: 8,
        name: "sequence",
    },
    HeaderField {
        offset: OFF_RUNTIME_GENERATION as u32,
        size: 8,
        name: "runtime_generation",
    },
    HeaderField {
        offset: OFF_SURFACE_GENERATION as u32,
        size: 8,
        name: "surface_generation",
    },
    HeaderField {
        offset: OFF_RESOURCE_EPOCH as u32,
        size: 8,
        name: "resource_epoch",
    },
    HeaderField {
        offset: OFF_FRAME_ID as u32,
        size: 4,
        name: "frame_id",
    },
    HeaderField {
        offset: OFF_FLAGS as u32,
        size: 4,
        name: "flags",
    },
    HeaderField {
        offset: OFF_SECTION_COUNT as u32,
        size: 4,
        name: "section_count",
    },
    HeaderField {
        offset: OFF_CHECKSUM as u32,
        size: 4,
        name: "payload_checksum",
    },
];

/// Why a packet was rejected.
///
/// Stable numbers: they cross the C ABI to the host, which reports them in
/// telemetry, so a reader six months from now can tell "the producer sent a
/// short packet" apart from "the producer sent someone else's packet". Never
/// renumber; only append.
///
/// Two codes here each cover what an earlier draft split into several.
/// [`WireError::SectionNotCanonical`] absorbed a separate misaligned code and a
/// separate overlap/order code, and [`WireError::ItemCountInconsistent`]
/// absorbed a bound-only variant. Those were not lost by accident: with a
/// canonical layout there is no input that is misaligned but canonical, or
/// overlapping but canonical, so the finer codes were branches nothing could
/// reach. An unreachable diagnostic is worse than one message that names all
/// the cases, because the next reader assumes it fires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum WireError {
    ShorterThanHeader = 1,
    BadMagic = 2,
    UnsupportedVersion = 3,
    BadHeaderBytes = 4,
    BadTotalBytes = 5,
    LengthMismatch = 6,
    TooManySections = 7,
    SectionTableOutOfBounds = 8,
    SectionOutOfBounds = 9,
    SectionNotCanonical = 10,
    PaddingNotZero = 11,
    DuplicateSection = 12,
    UnknownRequiredSection = 13,
    ChecksumMismatch = 14,
    UnknownFlags = 15,
    MissingPresent = 16,
    CommandStreamNotWordAligned = 17,
    ItemCountInconsistent = 18,
    MissingCommandStream = 19,
    TotalBytesNotCanonical = 20,
}

impl WireError {
    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Every variant, for consumers that must cover all of them -- the C ABI
    /// reporting test, and the document-agreement test.
    ///
    /// Hand-written lists of enum variants are how coverage goes quietly
    /// missing, so this one has a gate behind it:
    /// `tests/wire_document_agreement.rs` parses the variants out of this
    /// file's source and fails if `ALL` is missing one, has one twice, or if
    /// the codes are not exactly `1..=ALL.len()`. A variant added without
    /// being listed here cannot pass that test, so a consumer that iterates
    /// `ALL` is iterating all of them rather than all of them as of whenever
    /// someone last looked.
    pub const ALL: &'static [WireError] = &[
        Self::ShorterThanHeader,
        Self::BadMagic,
        Self::UnsupportedVersion,
        Self::BadHeaderBytes,
        Self::BadTotalBytes,
        Self::LengthMismatch,
        Self::TooManySections,
        Self::SectionTableOutOfBounds,
        Self::SectionOutOfBounds,
        Self::SectionNotCanonical,
        Self::PaddingNotZero,
        Self::DuplicateSection,
        Self::UnknownRequiredSection,
        Self::ChecksumMismatch,
        Self::UnknownFlags,
        Self::MissingPresent,
        Self::CommandStreamNotWordAligned,
        Self::ItemCountInconsistent,
        Self::MissingCommandStream,
        Self::TotalBytesNotCanonical,
    ];

    /// The variant's name, for tests and telemetry that report it as text.
    /// Derived from `Debug` so it cannot drift from the identifier.
    pub fn name(self) -> String {
        format!("{self:?}")
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ShorterThanHeader => "packet is shorter than the fixed header",
            Self::BadMagic => "magic is not MGPF",
            Self::UnsupportedVersion => "wire version is not supported by this reader",
            Self::BadHeaderBytes => "header_bytes does not equal the fixed header size",
            Self::BadTotalBytes => "total_bytes is below the header or above the cap",
            Self::LengthMismatch => "total_bytes does not equal the delivered byte count",
            Self::TooManySections => "section_count exceeds the cap",
            Self::SectionTableOutOfBounds => "the section table does not fit in the packet",
            Self::SectionOutOfBounds => "a section extends past the end of the packet",
            Self::SectionNotCanonical => {
                "a section does not start where the canonical layout puts it: it is misaligned, \
                 out of order, overlapping, or separated from the previous one by a gap"
            }
            Self::PaddingNotZero => "an alignment pad byte is not zero",
            Self::DuplicateSection => "the same section kind appears twice",
            Self::UnknownRequiredSection => "an unknown non-advisory section kind is present",
            Self::ChecksumMismatch => "the payload checksum does not match",
            Self::UnknownFlags => "a flag bit outside this wire version is set",
            Self::MissingPresent => "PRESENT is not set, and v1 has no frame continuation",
            Self::CommandStreamNotWordAligned => {
                "the command stream is not a whole number of words"
            }
            Self::ItemCountInconsistent => {
                "item_count disagrees with byte_length for this section kind"
            }
            Self::MissingCommandStream => "the packet carries no command stream",
            Self::TotalBytesNotCanonical => {
                "total_bytes is not the aligned end of the last section"
            }
        })
    }
}

/// One validated section: a kind and a borrowed slice of the original bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Section<'a> {
    pub kind: u32,
    pub item_count: u32,
    pub bytes: &'a [u8],
}

/// A packet whose envelope has been fully validated.
///
/// Holding one is the proof: every offset inside is in range, the checksum
/// matched, and no field needs re-checking downstream. Nothing constructs this
/// except [`validate`].
///
/// `PartialEq` compares the borrowed bytes, so it is linear in packet size. It
/// exists so a test can write `assert_eq!(validate(bad), Err(..))` and have the
/// accepted case print itself on failure; nothing on the render path compares
/// two frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireFrame<'a> {
    bytes: &'a [u8],
    launch_nonce: u128,
    sequence: u64,
    runtime_generation: u64,
    surface_generation: u64,
    resource_epoch: u64,
    frame_id: u32,
    flags: u32,
    section_count: u32,
}

impl<'a> WireFrame<'a> {
    /// The 128-bit identity of the app launch this producer was paired with.
    #[inline]
    pub const fn launch_nonce(&self) -> u128 {
        self.launch_nonce
    }
    #[inline]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    #[inline]
    pub const fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }
    #[inline]
    pub const fn surface_generation(&self) -> u64 {
        self.surface_generation
    }
    #[inline]
    pub const fn resource_epoch(&self) -> u64 {
        self.resource_epoch
    }
    /// The producer's own frame counter. Advisory: it exists for latency
    /// attribution across the process boundary, and nothing is derived from it.
    #[inline]
    pub const fn frame_id(&self) -> u32 {
        self.frame_id
    }
    #[inline]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
    /// Always true for a validated v1 frame: `PRESENT` is required. Kept as an
    /// accessor because a later version may reintroduce non-presenting packets
    /// under semantics that define what happens to them, and a consumer that
    /// asks is then already correct.
    #[inline]
    pub const fn presents(&self) -> bool {
        self.flags & FLAG_PRESENT != 0
    }
    #[inline]
    pub const fn section_count(&self) -> u32 {
        self.section_count
    }
    #[inline]
    pub const fn total_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Every section, in the ascending offset order validation established.
    pub fn sections(&self) -> impl Iterator<Item = Section<'a>> + '_ {
        let bytes = self.bytes;
        (0..self.section_count).map(move |index| {
            // Every read below was bounds-checked by `validate`; the slicing
            // here cannot be out of range for a `WireFrame` that exists.
            let entry = HEADER_BYTES as usize + (index * SECTION_ENTRY_BYTES) as usize;
            let kind = read_u32(bytes, entry);
            let offset = read_u32(bytes, entry + 4) as usize;
            let length = read_u32(bytes, entry + 8) as usize;
            let item_count = read_u32(bytes, entry + 12);
            Section {
                kind,
                item_count,
                bytes: &bytes[offset..offset + length],
            }
        })
    }

    /// The command stream, which every drawing packet must carry.
    pub fn command_stream(&self) -> Option<Section<'a>> {
        self.sections()
            .find(|section| section.kind == SECTION_KIND_COMMAND_STREAM)
    }

    /// Whether this packet names resources from the resource lane.
    ///
    /// The ingress needs it to apply resource admission, and asking here keeps
    /// that policy from re-walking the section table with its own assumptions
    /// about what a resource section looks like.
    pub fn references_resources(&self) -> bool {
        self.sections()
            .any(|section| section.kind == SECTION_KIND_RESOURCE_REFERENCES)
    }
}

#[inline]
fn read_u32(bytes: &[u8], at: usize) -> u32 {
    // Byte-wise on purpose: see the alignment note in the module docs.
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[inline]
fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut buffer = [0u8; 8];
    buffer.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(buffer)
}

#[inline]
fn read_u128(bytes: &[u8], at: usize) -> u128 {
    let mut buffer = [0u8; 16];
    buffer.copy_from_slice(&bytes[at..at + 16]);
    u128::from_le_bytes(buffer)
}

#[inline]
const fn align_up(value: u32) -> Option<u32> {
    // Checked: a length near u32::MAX rounds up past the type, and a wrapped
    // result compares as a smaller, plausible offset.
    match value.checked_add(SECTION_ALIGNMENT - 1) {
        Some(sum) => Some(sum & !(SECTION_ALIGNMENT - 1)),
        None => None,
    }
}

/// Validate one packet's envelope. Allocates nothing, and cannot panic for any
/// input.
///
/// The order is chosen so cheap, high-signal checks reject first and the
/// checksum -- the only pass over the whole packet -- runs last. A malformed
/// header should never cost a full CRC.
pub fn validate(bytes: &[u8]) -> Result<WireFrame<'_>, WireError> {
    if bytes.len() < HEADER_BYTES as usize {
        return Err(WireError::ShorterThanHeader);
    }

    if read_u32(bytes, OFF_MAGIC) != WIRE_MAGIC {
        return Err(WireError::BadMagic);
    }
    if read_u32(bytes, OFF_WIRE_VERSION) != WIRE_VERSION {
        return Err(WireError::UnsupportedVersion);
    }
    if read_u32(bytes, OFF_HEADER_BYTES) != HEADER_BYTES {
        return Err(WireError::BadHeaderBytes);
    }

    let total_bytes = read_u32(bytes, OFF_TOTAL_BYTES);
    if !(HEADER_BYTES..=MAX_TOTAL_BYTES).contains(&total_bytes) {
        return Err(WireError::BadTotalBytes);
    }
    // The delivered length is the authority, and the announced one must agree
    // exactly. Accepting a packet longer than it claims would leave trailing
    // bytes outside the checksum -- a place to hide anything. It is also what
    // stops a transport fragment being mistaken for a whole packet: this
    // parser has no reassembly, by design.
    if total_bytes as usize != bytes.len() {
        return Err(WireError::LengthMismatch);
    }

    let flags = read_u32(bytes, OFF_FLAGS);
    if flags & !FLAG_KNOWN_MASK != 0 {
        return Err(WireError::UnknownFlags);
    }
    if flags & FLAG_PRESENT == 0 {
        return Err(WireError::MissingPresent);
    }

    let section_count = read_u32(bytes, OFF_SECTION_COUNT);
    if section_count > MAX_SECTIONS {
        return Err(WireError::TooManySections);
    }

    // Checked arithmetic throughout: `section_count * 16` overflows a u32 for
    // large counts, and an overflowed table end compares as "fits".
    let table_bytes = section_count
        .checked_mul(SECTION_ENTRY_BYTES)
        .ok_or(WireError::SectionTableOutOfBounds)?;
    let payload_start = HEADER_BYTES
        .checked_add(table_bytes)
        .ok_or(WireError::SectionTableOutOfBounds)?;
    if payload_start > total_bytes {
        return Err(WireError::SectionTableOutOfBounds);
    }

    // Fixed-size, because MAX_SECTIONS bounds it. A Vec here would be an
    // allocation sized by an attacker-supplied count, inside the parser whose
    // job is to not do that.
    let mut seen_kinds = [0u32; MAX_SECTIONS as usize];
    // `payload_start` is HEADER_BYTES + 16n, so it is already aligned and the
    // first section starts exactly there with no pad.
    let mut previous_end = payload_start;
    let mut has_command_stream = false;

    for index in 0..section_count {
        let entry = HEADER_BYTES as usize + (index * SECTION_ENTRY_BYTES) as usize;
        let kind = read_u32(bytes, entry);
        let offset = read_u32(bytes, entry + 4);
        let length = read_u32(bytes, entry + 8);
        let item_count = read_u32(bytes, entry + 12);

        if kind < SECTION_KIND_ADVISORY_BIT
            && !matches!(
                kind,
                SECTION_KIND_COMMAND_STREAM
                    | SECTION_KIND_INLINE_DATA
                    | SECTION_KIND_RESOURCE_REFERENCES
            )
        {
            return Err(WireError::UnknownRequiredSection);
        }

        // `index` is the count of kinds already recorded: every earlier
        // iteration either returned or wrote exactly one entry.
        let recorded = index as usize;
        for &previous in &seen_kinds[..recorded] {
            if previous == kind {
                return Err(WireError::DuplicateSection);
            }
        }
        seen_kinds[recorded] = kind;

        // Canonical position. Ascending order, disjointness and 8-byte
        // alignment are consequences of this one equality rather than three
        // separate checks: no offset satisfies it and also overlaps, sits out
        // of order, or lands unaligned.
        let expected = align_up(previous_end).ok_or(WireError::SectionNotCanonical)?;
        if offset != expected {
            return Err(WireError::SectionNotCanonical);
        }
        // The pad between the previous section and this one is zero. Both
        // bounds are inside the packet: `previous_end <= offset` by the
        // equality above, and `offset < total_bytes` follows from the section
        // fitting, which is checked next -- so the slice is taken after it.
        let end = offset
            .checked_add(length)
            .ok_or(WireError::SectionOutOfBounds)?;
        if end > total_bytes {
            return Err(WireError::SectionOutOfBounds);
        }
        if bytes[previous_end as usize..offset as usize]
            .iter()
            .any(|&byte| byte != 0)
        {
            return Err(WireError::PaddingNotZero);
        }
        previous_end = end;

        // `item_count` against `byte_length`, per kind. Fixed-width records are
        // pinned exactly; variable-width ones get the only bound the envelope
        // can state.
        match kind {
            SECTION_KIND_COMMAND_STREAM => {
                has_command_stream = true;
                if !length.is_multiple_of(4) {
                    return Err(WireError::CommandStreamNotWordAligned);
                }
                // Each record is at least one word, so a claimed record count
                // above the word count is a lie the consumer would otherwise
                // carry into its own loop bound.
                if item_count > length / 4 {
                    return Err(WireError::ItemCountInconsistent);
                }
            }
            SECTION_KIND_RESOURCE_REFERENCES => {
                if item_count.checked_mul(RESOURCE_REFERENCE_BYTES) != Some(length) {
                    return Err(WireError::ItemCountInconsistent);
                }
            }
            SECTION_KIND_DAMAGE => {
                if item_count.checked_mul(DAMAGE_RECT_BYTES) != Some(length) {
                    return Err(WireError::ItemCountInconsistent);
                }
            }
            _ => {
                if item_count > length {
                    return Err(WireError::ItemCountInconsistent);
                }
            }
        }
    }

    if !has_command_stream {
        return Err(WireError::MissingCommandStream);
    }

    // The packet ends at the aligned end of the last section: no trailing
    // bytes, and no missing final pad.
    let canonical_total = align_up(previous_end).ok_or(WireError::TotalBytesNotCanonical)?;
    if total_bytes != canonical_total {
        return Err(WireError::TotalBytesNotCanonical);
    }
    if bytes[previous_end as usize..total_bytes as usize]
        .iter()
        .any(|&byte| byte != 0)
    {
        return Err(WireError::PaddingNotZero);
    }

    if checksum(bytes) != read_u32(bytes, OFF_CHECKSUM) {
        return Err(WireError::ChecksumMismatch);
    }

    Ok(WireFrame {
        bytes,
        launch_nonce: read_u128(bytes, OFF_LAUNCH_NONCE),
        sequence: read_u64(bytes, OFF_SEQUENCE),
        runtime_generation: read_u64(bytes, OFF_RUNTIME_GENERATION),
        surface_generation: read_u64(bytes, OFF_SURFACE_GENERATION),
        resource_epoch: read_u64(bytes, OFF_RESOURCE_EPOCH),
        frame_id: read_u32(bytes, OFF_FRAME_ID),
        flags,
        section_count,
    })
}

/// CRC32 of the whole packet with the checksum field itself read as zero.
///
/// Covering the header, not just the payload, is the point: a flipped
/// `section_count` or `frame_id` is exactly the kind of corruption that still
/// parses. The producer computes this the same way, over the same bytes.
pub fn checksum(bytes: &[u8]) -> u32 {
    // Short input is handled rather than assumed away. Every caller inside this
    // crate checks the length first, so the slicing below could be written
    // unguarded and would be correct today -- and would be a panic reachable
    // from outside the moment anyone called this directly on a truncated
    // packet. In a crate whose contract is "no input panics", a public function
    // that is safe only because of where it happens to be called from is not
    // safe.
    let mut hasher = crc32fast::Hasher::new();
    let head = bytes.len().min(OFF_CHECKSUM);
    hasher.update(&bytes[..head]);
    if bytes.len() >= OFF_CHECKSUM_END {
        hasher.update(&[0u8; 4]);
        hasher.update(&bytes[OFF_CHECKSUM_END..]);
    } else if bytes.len() > OFF_CHECKSUM {
        // Inside the checksum field itself: the bytes there are excluded from
        // their own input, so a partial field contributes nothing.
        hasher.update(&[0u8; 4]);
    }
    hasher.finalize()
}

/// Write the checksum into a packet that is otherwise complete.
///
/// Lives here rather than in the producer so both ends compute it from one
/// implementation. Two encoders that agree with each other and not with a fixed
/// corpus is the failure this avoids.
pub fn stamp_checksum(bytes: &mut [u8]) {
    if bytes.len() < HEADER_BYTES as usize {
        return;
    }
    let value = checksum(bytes).to_le_bytes();
    bytes[OFF_CHECKSUM..OFF_CHECKSUM_END].copy_from_slice(&value);
}

/// The command stream carried inside a `COMMAND_STREAM` section.
///
/// It lives here, and not in the JavaScript runtime where it was written,
/// because both readers need it and only one of them has a JavaScript engine.
/// In-process, `runtime-v8` validates the same words before decoding them; on
/// the cross-process path, the consumer validates them with no V8 linked at all
/// -- which is the product claim `MigoApplePerformancePlus` rests on and could
/// not make if this validator were reachable only through the engine crate.
///
/// A pure function over `&[u32]` is the only shape that can be shared by a
/// trusted in-process caller and an untrusted cross-process one without either
/// inheriting the other's dependencies, so the validator keeps no imports of
/// its own beyond the two opcode blocks it routes to.
pub mod stream;

/// The WebGL block: its opcodes and their record shapes.
pub mod gl;

/// The Canvas2D block. See its module docs for why one stream carries both.
pub mod canvas2d;

pub mod ingress;
pub mod pool;
/// The resource lane: large bytes uploaded out of band and referenceable only
/// once verified.
pub mod resource;
/// The synchronous barrier: the few calls whose return value is the answer.
pub mod sync;
pub use ingress::{FrameIngress, IngressDecision, IngressOutcome};
pub use pool::{CreditWindow, FrameCredit, FramePool, PooledFrame};

#[cfg(any(test, feature = "test-support"))]
pub mod builder;
