//! Every rejection path, one case each, plus the properties that hold for all
//! inputs.
//!
//! The cases mutate a packet that is known good, so each one isolates a single
//! violation. A test that builds a bad packet from scratch tends to be wrong in
//! several ways at once and then passes for the wrong reason.

use frame_wire::{
    DAMAGE_RECT_BYTES, FLAG_PRESENT, HEADER_BYTES, HEADER_LAYOUT, MAX_SECTIONS, MAX_TOTAL_BYTES,
    RESOURCE_REFERENCE_BYTES, SECTION_ENTRY_BYTES, SECTION_KIND_COMMAND_STREAM,
    SECTION_KIND_DAMAGE, SECTION_KIND_INLINE_DATA, SECTION_KIND_RESOURCE_REFERENCES,
    SECTION_KIND_TIMING, WIRE_MAGIC, WIRE_VERSION, WireError, builder::WireFrameBuilder,
    stamp_checksum, validate,
};

const OFF_MAGIC: usize = 0;
const OFF_WIRE_VERSION: usize = 4;
const OFF_HEADER_BYTES: usize = 8;
const OFF_TOTAL_BYTES: usize = 12;
const OFF_FLAGS: usize = 68;
const OFF_SECTION_COUNT: usize = 72;
const OFF_CHECKSUM: usize = 76;

/// The offsets above are hand-written on purpose -- a test that imports the
/// constants it is checking cannot notice them moving. This ties them to the
/// exported layout once, so a layout change fails here rather than silently
/// mutating a different field than the case intends.
#[test]
fn the_offsets_these_cases_poke_are_the_ones_the_layout_declares() {
    let named = |name: &str| {
        HEADER_LAYOUT
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("no header field named {name}"))
            .offset as usize
    };
    assert_eq!(named("magic"), OFF_MAGIC);
    assert_eq!(named("wire_version"), OFF_WIRE_VERSION);
    assert_eq!(named("header_bytes"), OFF_HEADER_BYTES);
    assert_eq!(named("total_bytes"), OFF_TOTAL_BYTES);
    assert_eq!(named("flags"), OFF_FLAGS);
    assert_eq!(named("section_count"), OFF_SECTION_COUNT);
    assert_eq!(named("payload_checksum"), OFF_CHECKSUM);
}

/// A minimal, valid packet: one command stream of four words.
fn good() -> Vec<u8> {
    let stream = [0u8; 16];
    WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .build()
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Corrupt a field and restamp, so the case under test is the only violation.
/// Without restamping every mutation would be reported as a checksum failure --
/// technically a rejection, and useless as evidence that the specific rule works.
fn mutate(mutation: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = good();
    mutation(&mut bytes);
    stamp_checksum(&mut bytes);
    bytes
}

#[test]
fn a_well_formed_packet_round_trips() {
    let bytes = good();
    let frame = validate(&bytes).expect("the builder must produce a valid packet");

    assert_eq!(frame.total_bytes(), bytes.len());
    assert_eq!(frame.section_count(), 1);
    assert!(frame.presents());
    assert_eq!(frame.sequence(), 1);
    assert!(!frame.references_resources());

    let stream = frame.command_stream().expect("command stream section");
    assert_eq!(stream.kind, SECTION_KIND_COMMAND_STREAM);
    assert_eq!(stream.bytes.len(), 16);
    assert_eq!(stream.item_count, 4);
}

/// The identity and timeline fields are the ones a stale packet gets wrong, so
/// their full width has to survive the round trip. A truncating read would pass
/// every test that used small numbers.
#[test]
fn the_wide_identity_and_timeline_fields_survive_at_full_width() {
    let stream = [0u8; 4];
    let mut packet = WireFrameBuilder::new();
    packet.launch_nonce = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
    packet.sequence = u64::MAX - 3;
    packet.runtime_generation = 0x8000_0000_0000_0001;
    packet.surface_generation = 0x7FFF_FFFF_FFFF_FFFF;
    packet.resource_epoch = 0x0000_0001_0000_0000;
    let bytes = packet
        .section(SECTION_KIND_COMMAND_STREAM, 1, &stream)
        .build();

    let frame = validate(&bytes).expect("wide values are valid values");
    assert_eq!(
        frame.launch_nonce(),
        0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210
    );
    assert_eq!(frame.sequence(), u64::MAX - 3);
    assert_eq!(frame.runtime_generation(), 0x8000_0000_0000_0001);
    assert_eq!(frame.surface_generation(), 0x7FFF_FFFF_FFFF_FFFF);
    assert_eq!(frame.resource_epoch(), 0x0000_0001_0000_0000);
}

#[test]
fn sections_come_back_in_the_order_they_were_written() {
    let stream = [0u8; 8];
    let inline = [7u8; 12];
    let damage = [0u8; 16];
    let bytes = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 2, &stream)
        .section(SECTION_KIND_INLINE_DATA, 12, &inline)
        .section(SECTION_KIND_DAMAGE, 1, &damage)
        .build();

    let frame = validate(&bytes).expect("three sections is valid");
    let kinds: Vec<u32> = frame.sections().map(|section| section.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SECTION_KIND_COMMAND_STREAM,
            SECTION_KIND_INLINE_DATA,
            SECTION_KIND_DAMAGE
        ]
    );
}

#[test]
fn a_truncated_packet_is_rejected_before_any_field_is_read() {
    let bytes = good();
    for length in 0..HEADER_BYTES as usize {
        assert_eq!(
            validate(&bytes[..length]),
            Err(WireError::ShorterThanHeader),
            "a {length}-byte packet must not reach a field read"
        );
    }
}

#[test]
fn each_header_rule_has_its_own_rejection() {
    let cases: Vec<(&str, Vec<u8>, WireError)> = vec![
        (
            "magic",
            mutate(|bytes| put_u32(bytes, OFF_MAGIC, WIRE_MAGIC ^ 1)),
            WireError::BadMagic,
        ),
        (
            "version",
            mutate(|bytes| put_u32(bytes, OFF_WIRE_VERSION, WIRE_VERSION + 1)),
            WireError::UnsupportedVersion,
        ),
        (
            "header_bytes",
            mutate(|bytes| put_u32(bytes, OFF_HEADER_BYTES, HEADER_BYTES + 8)),
            WireError::BadHeaderBytes,
        ),
        (
            "total_bytes below the header",
            mutate(|bytes| put_u32(bytes, OFF_TOTAL_BYTES, HEADER_BYTES - 8)),
            WireError::BadTotalBytes,
        ),
        (
            "total_bytes above the absolute ceiling",
            mutate(|bytes| put_u32(bytes, OFF_TOTAL_BYTES, MAX_TOTAL_BYTES + 1)),
            WireError::BadTotalBytes,
        ),
        (
            "total_bytes disagreeing with the delivered length",
            mutate(|bytes| {
                let shorter = bytes.len() as u32 - 8;
                put_u32(bytes, OFF_TOTAL_BYTES, shorter);
            }),
            WireError::LengthMismatch,
        ),
        (
            "an unknown flag bit",
            mutate(|bytes| put_u32(bytes, OFF_FLAGS, FLAG_PRESENT | 1 << 7)),
            WireError::UnknownFlags,
        ),
        (
            "too many sections",
            mutate(|bytes| put_u32(bytes, OFF_SECTION_COUNT, MAX_SECTIONS + 1)),
            WireError::TooManySections,
        ),
        (
            "a section table that cannot fit",
            mutate(|bytes| put_u32(bytes, OFF_SECTION_COUNT, MAX_SECTIONS)),
            WireError::SectionTableOutOfBounds,
        ),
        (
            "a corrupted checksum",
            {
                let mut bytes = good();
                let stored = u32::from_le_bytes([
                    bytes[OFF_CHECKSUM],
                    bytes[OFF_CHECKSUM + 1],
                    bytes[OFF_CHECKSUM + 2],
                    bytes[OFF_CHECKSUM + 3],
                ]);
                put_u32(&mut bytes, OFF_CHECKSUM, stored ^ 1);
                bytes
            },
            WireError::ChecksumMismatch,
        ),
    ];

    for (name, bytes, expected) in cases {
        assert_eq!(validate(&bytes), Err(expected), "case: {name}");
    }
}

/// The bit that says "this packet is not a whole frame" is gone, and the
/// absence is enforced rather than documented: v1's only legal `flags` value is
/// exactly `PRESENT`.
#[test]
fn the_retired_continuation_flag_is_now_an_unknown_bit() {
    let retired_continued_bit = 1u32 << 1;
    let bytes = mutate(|bytes| put_u32(bytes, OFF_FLAGS, FLAG_PRESENT | retired_continued_bit));
    assert_eq!(validate(&bytes), Err(WireError::UnknownFlags));

    let only_continued = mutate(|bytes| put_u32(bytes, OFF_FLAGS, retired_continued_bit));
    assert_eq!(
        validate(&only_continued),
        Err(WireError::UnknownFlags),
        "a barrier is flags == 0, not a packet with the retired bit and no PRESENT"
    );
}

/// A packet without `PRESENT` is a barrier: valid, and it says so.
///
/// It used to be `MissingPresent`. That code is retired rather than reused --
/// nothing produces it now -- and this is the test that would fail if the
/// parser started refusing barriers again.
#[test]
fn a_packet_without_present_is_a_valid_barrier() {
    let barrier = mutate(|bytes| put_u32(bytes, OFF_FLAGS, 0));
    let frame = validate(&barrier).expect("a barrier is a valid packet");
    assert!(!frame.presents());
    assert_eq!(frame.flags(), 0);

    let fixture = good();
    let presenting = validate(&fixture).expect("the fixture presents");
    assert!(presenting.presents());
}

/// A transport that splits a packet must reassemble before calling the parser.
/// The parser contains no reassembly, and this is the property that makes a
/// fragment unmistakable for a packet rather than a smaller one.
#[test]
fn a_transport_fragment_is_never_mistaken_for_a_packet() {
    let bytes = good();
    for length in HEADER_BYTES as usize..bytes.len() {
        assert_eq!(
            validate(&bytes[..length]),
            Err(WireError::LengthMismatch),
            "a {length}-byte prefix is a fragment, not a packet"
        );
    }
}

#[test]
fn each_section_rule_has_its_own_rejection() {
    let stream = [0u8; 16];
    let entry0 = HEADER_BYTES as usize;

    // Offset: pushed past the packet end, and pushed off the canonical spot.
    let out_of_bounds = mutate(|bytes| {
        let end = bytes.len() as u32;
        put_u32(bytes, entry0 + 8, end);
    });
    assert_eq!(validate(&out_of_bounds), Err(WireError::SectionOutOfBounds));

    let misaligned = mutate(|bytes| {
        let offset = u32::from_le_bytes([
            bytes[entry0 + 4],
            bytes[entry0 + 5],
            bytes[entry0 + 6],
            bytes[entry0 + 7],
        ]);
        put_u32(bytes, entry0 + 4, offset + 4);
    });
    assert_eq!(validate(&misaligned), Err(WireError::SectionNotCanonical));

    // An aligned gap: legal alignment, illegal position. Without a canonical
    // rule this would be accepted, and the gap bytes would be inside the
    // checksum and outside every consumer.
    let mut gapped = WireFrameBuilder::new();
    gapped.extra_gap = 8;
    let gapped = gapped
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .build();
    assert_eq!(validate(&gapped), Err(WireError::SectionNotCanonical));

    // A ragged section leaves a pad; the pad must be zero. There are two such
    // pads and they are checked in two different places, so they get two cases:
    // a single ragged section leaves only the *final* pad, and a ragged section
    // followed by one that ends aligned leaves only an *inter-section* pad.
    //
    // Splitting them is not pedantry. The first version of this test had only
    // the single-section case, and deleting the inter-section check left it
    // green -- the post-loop check was answering for a rule it does not cover.
    let ragged = [1u8; 12];
    let mut final_pad_dirty = WireFrameBuilder::new();
    final_pad_dirty.pad_fill = 0xAA;
    let final_pad_dirty = final_pad_dirty
        .section(SECTION_KIND_COMMAND_STREAM, 3, &ragged)
        .build();
    assert_eq!(
        validate(&final_pad_dirty),
        Err(WireError::PaddingNotZero),
        "the pad after the last section"
    );

    // 80 header + 32 table + 12 + 4 pad + 8 = 136, which is aligned, so this
    // packet has an inter-section pad and no final one.
    let ends_aligned = [2u8; 8];
    let mut inner_pad_dirty = WireFrameBuilder::new();
    inner_pad_dirty.pad_fill = 0xAA;
    let inner_pad_dirty = inner_pad_dirty
        .section(SECTION_KIND_COMMAND_STREAM, 3, &ragged)
        .section(SECTION_KIND_INLINE_DATA, 8, &ends_aligned)
        .build();
    assert_eq!(inner_pad_dirty.len() % 8, 0);
    assert_eq!(
        validate(&inner_pad_dirty),
        Err(WireError::PaddingNotZero),
        "the pad between two sections"
    );

    // Bytes past the aligned end of the last section.
    let mut trailing = WireFrameBuilder::new();
    trailing.trailing_bytes = 8;
    let trailing = trailing
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .build();
    assert_eq!(
        validate(&trailing),
        Err(WireError::TotalBytesNotCanonical),
        "trailing bytes are neither payload nor pad"
    );

    // A command stream that is not a whole number of words.
    let ragged_stream = [0u8; 13];
    let not_words = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 3, &ragged_stream)
        .build();
    assert_eq!(
        validate(&not_words),
        Err(WireError::CommandStreamNotWordAligned)
    );

    // An unknown required kind, and an unknown advisory kind that is fine.
    let unknown_required = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .section(7, 0, &[])
        .build();
    assert_eq!(
        validate(&unknown_required),
        Err(WireError::UnknownRequiredSection)
    );

    let unknown_advisory = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .section(0x8000_00FF, 0, &[])
        .build();
    validate(&unknown_advisory).expect("an unknown advisory kind is skipped, not fatal");
}

/// `item_count` is pinned to `byte_length` for every kind with a fixed record
/// width, and bounded by it for the two that do not have one. A count that
/// merely fits is a count the consumer loops on beside a length it trusts.
#[test]
fn item_count_must_agree_with_byte_length_for_each_kind() {
    let stream = [0u8; 16];

    let refs = [0u8; 12];
    let exact = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .section(
            SECTION_KIND_RESOURCE_REFERENCES,
            refs.len() as u32 / RESOURCE_REFERENCE_BYTES,
            &refs,
        )
        .build();
    validate(&exact).expect("three 4-byte references in twelve bytes is exact");

    for wrong in [2u32, 4] {
        let bytes = WireFrameBuilder::new()
            .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
            .section(SECTION_KIND_RESOURCE_REFERENCES, wrong, &refs)
            .build();
        assert_eq!(
            validate(&bytes),
            Err(WireError::ItemCountInconsistent),
            "{wrong} references cannot fit exactly in {} bytes",
            refs.len()
        );
    }

    let rects = [0u8; 32];
    let exact_damage = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .section(
            SECTION_KIND_DAMAGE,
            rects.len() as u32 / DAMAGE_RECT_BYTES,
            &rects,
        )
        .build();
    validate(&exact_damage).expect("two 16-byte rects in thirty-two bytes is exact");

    let wrong_damage = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
        .section(SECTION_KIND_DAMAGE, 1, &rects)
        .build();
    assert_eq!(
        validate(&wrong_damage),
        Err(WireError::ItemCountInconsistent)
    );

    // Command stream records are variable length, so the bound is all the
    // envelope can say -- but it does say it.
    let too_many_records = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 5, &stream)
        .build();
    assert_eq!(
        validate(&too_many_records),
        Err(WireError::ItemCountInconsistent)
    );

    // Same for the two kinds whose record shape the envelope does not know.
    let blob = [0u8; 8];
    for kind in [SECTION_KIND_INLINE_DATA, SECTION_KIND_TIMING] {
        let bytes = WireFrameBuilder::new()
            .section(SECTION_KIND_COMMAND_STREAM, 4, &stream)
            .section(kind, blob.len() as u32 + 1, &blob)
            .build();
        assert_eq!(
            validate(&bytes),
            Err(WireError::ItemCountInconsistent),
            "kind {kind:#x} cannot hold more items than bytes"
        );
    }
}

#[test]
fn duplicate_and_unordered_sections_are_rejected() {
    let stream = [0u8; 8];
    let more = [0u8; 8];
    let duplicate = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 2, &stream)
        .section(SECTION_KIND_COMMAND_STREAM, 2, &more)
        .build();
    assert_eq!(validate(&duplicate), Err(WireError::DuplicateSection));

    // Swap two entries in the table so the offsets descend. With a canonical
    // layout that is the same rejection as a gap: there is one legal position
    // per section, and neither entry is now at its own.
    let inline = [3u8; 8];
    let mut swapped = WireFrameBuilder::new()
        .section(SECTION_KIND_COMMAND_STREAM, 2, &stream)
        .section(SECTION_KIND_INLINE_DATA, 8, &inline)
        .build();
    let entry = HEADER_BYTES as usize;
    let width = SECTION_ENTRY_BYTES as usize;
    let (first, second) = (
        swapped[entry..entry + width].to_vec(),
        swapped[entry + width..entry + 2 * width].to_vec(),
    );
    swapped[entry..entry + width].copy_from_slice(&second);
    swapped[entry + width..entry + 2 * width].copy_from_slice(&first);
    stamp_checksum(&mut swapped);
    assert_eq!(validate(&swapped), Err(WireError::SectionNotCanonical));
}

#[test]
fn a_packet_with_no_command_stream_is_rejected() {
    let inline = [1u8; 8];
    let bytes = WireFrameBuilder::new()
        .section(SECTION_KIND_INLINE_DATA, 8, &inline)
        .build();
    assert_eq!(validate(&bytes), Err(WireError::MissingCommandStream));

    let empty = mutate(|bytes| put_u32(bytes, OFF_SECTION_COUNT, 0));
    assert_eq!(
        validate(&empty),
        Err(WireError::MissingCommandStream),
        "zero sections is answered by the rule that says why, not by a count"
    );
}

#[test]
fn the_checksum_covers_the_header_not_just_the_payload() {
    // Flip a header field that changes nothing structural, and do not restamp.
    // Only a checksum over the header notices.
    let mut bytes = good();
    let advisory_field = HEADER_LAYOUT
        .iter()
        .find(|field| field.name == "frame_id")
        .expect("frame_id is a header field")
        .offset as usize;
    put_u32(&mut bytes, advisory_field, 0xDEAD_BEEF);
    assert_eq!(
        validate(&bytes),
        Err(WireError::ChecksumMismatch),
        "an advisory header field is still inside the integrity check"
    );
}

/// The property, not a case: nothing the producer can send makes this panic.
///
/// A frame arrives from another process on the render path. A panic there is
/// not a caught error -- it is an abort in a shipped app, reachable from
/// content JavaScript.
#[test]
fn no_input_can_panic_the_parser() {
    let template = good();

    // Every single-byte value at every offset in the header and table.
    for offset in 0..template.len().min(HEADER_BYTES as usize + 16) {
        for value in [0u8, 1, 0x7F, 0x80, 0xFE, 0xFF] {
            let mut bytes = template.clone();
            bytes[offset] = value;
            let _ = validate(&bytes);
            let mut restamped = bytes.clone();
            stamp_checksum(&mut restamped);
            let _ = validate(&restamped);
        }
    }

    // Whole-word extremes in every table entry field.
    for offset in (HEADER_BYTES as usize..HEADER_BYTES as usize + 16).step_by(4) {
        for value in [0u32, 1, 4, 7, u32::MAX, u32::MAX - 7, 1 << 31] {
            let mut bytes = template.clone();
            put_u32(&mut bytes, offset, value);
            stamp_checksum(&mut bytes);
            let _ = validate(&bytes);
        }
    }

    // Lengths and shapes with no relation to a packet.
    for shape in [
        vec![],
        vec![0u8; 1],
        vec![0xFFu8; HEADER_BYTES as usize],
        vec![0xFFu8; HEADER_BYTES as usize * 3],
        {
            let mut bytes = vec![0u8; HEADER_BYTES as usize + 32];
            put_u32(&mut bytes, OFF_MAGIC, WIRE_MAGIC);
            put_u32(&mut bytes, OFF_WIRE_VERSION, WIRE_VERSION);
            put_u32(&mut bytes, OFF_HEADER_BYTES, HEADER_BYTES);
            let length = bytes.len() as u32;
            put_u32(&mut bytes, OFF_TOTAL_BYTES, length);
            put_u32(&mut bytes, OFF_SECTION_COUNT, MAX_SECTIONS);
            stamp_checksum(&mut bytes);
            bytes
        },
    ] {
        let _ = validate(&shape);
    }
}

/// The other property: a validated frame's section slices are always in range.
///
/// `sections()` indexes without re-checking, on the strength of validation
/// having run. That is only sound if validation actually established it, so it
/// gets asserted rather than assumed.
#[test]
fn every_accepted_frame_has_in_range_sections() {
    let stream = [1u8; 12];
    let inline = [2u8; 5];
    let refs = [3u8; 8];
    let damage = [4u8; 16];
    let timing = [5u8; 3];

    for flags in [FLAG_PRESENT] {
        let mut packet = WireFrameBuilder::new();
        packet.flags = flags;
        let bytes = packet
            .section(SECTION_KIND_COMMAND_STREAM, 3, &stream)
            .section(SECTION_KIND_INLINE_DATA, 5, &inline)
            .section(SECTION_KIND_RESOURCE_REFERENCES, 2, &refs)
            .section(SECTION_KIND_DAMAGE, 1, &damage)
            .section(SECTION_KIND_TIMING, 3, &timing)
            .build();
        let frame = validate(&bytes).expect("five sections is valid");
        assert!(frame.references_resources());

        let mut covered = 0usize;
        for section in frame.sections() {
            let start = section.bytes.as_ptr() as usize - bytes.as_ptr() as usize;
            assert!(
                start + section.bytes.len() <= bytes.len(),
                "section {:#x} runs past the packet",
                section.kind
            );
            covered += section.bytes.len();
        }
        assert!(covered <= bytes.len());
    }
}

/// `checksum` is public, so it must survive a caller who did not check the
/// length first. Inside this crate every caller does; that is exactly why the
/// hazard would have gone unnoticed until someone outside called it.
#[test]
fn the_public_checksum_helper_survives_any_length() {
    let full = good();
    for length in 0..=full.len() {
        let _ = frame_wire::checksum(&full[..length]);
    }
    let _ = frame_wire::checksum(&[]);
    // And it still agrees with itself on the length that matters.
    assert_eq!(frame_wire::checksum(&full), frame_wire::checksum(&full));
}
