//! The wire-format document and this crate, compared field by field.
//!
//! `contracts/frame-wire/wire-v1.md` is the specification; neither encoder nor
//! reader is. That only means anything if the two are actually checked against
//! each other, and "checked by eye at review time" is how a format ends up
//! with a document describing a layout no implementation writes.
//!
//! So the header table is parsed out of the document and compared to
//! `frame_wire::HEADER_LAYOUT`, which is built from the same offset constants
//! `validate` reads. Drift in either direction turns this red.

use std::{fs, path::PathBuf};

use frame_wire::{
    HEADER_BYTES, HEADER_LAYOUT, HeaderField, MAX_SECTIONS, MAX_TOTAL_BYTES, WireError,
    ingress::{INGRESS_ERROR_BASE, INGRESS_ERROR_CODES},
    resource::{ResourceError, ResourceState},
    sync::{
        MAX_REPLY_BYTES, SYNC_ANSWER_HEADER_BYTES, SYNC_ANSWER_LAYOUT, SYNC_CALL_HEADER_BYTES,
        SYNC_CALL_LAYOUT, SYNC_CALL_MAX_BYTES, SYNC_CALL_MAX_TIMEOUT_MILLIS, SYNC_LAYOUT,
        SYNC_RECORD_BYTES, SyncError, SyncState,
    },
};

fn document() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/frame-wire/wire-v1.md");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Rows of the first markdown table after `heading`, as trimmed cell vectors.
fn table_after(document: &str, heading: &str) -> Vec<Vec<String>> {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("the document has no {heading:?} heading"));
    let mut rows = Vec::new();
    let mut seen_header_row = false;
    for line in document[start..].lines().skip(1) {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            if rows.is_empty() && !seen_header_row {
                continue;
            }
            break;
        }
        let cells: Vec<String> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect();
        if !seen_header_row {
            seen_header_row = true;
            continue;
        }
        if cells
            .iter()
            .all(|cell| cell.chars().all(|c| c == '-' || c == ':'))
        {
            continue;
        }
        rows.push(cells);
    }
    assert!(!rows.is_empty(), "no table rows found after {heading:?}");
    rows
}

fn strip_code_ticks(cell: &str) -> String {
    cell.trim_matches('`').to_string()
}

#[test]
fn the_document_header_table_matches_the_exported_layout() {
    let document = document();
    let rows = table_after(&document, "## Header");

    let declared: Vec<HeaderField> = rows
        .iter()
        .map(|cells| {
            assert!(
                cells.len() >= 3,
                "a header row needs offset, size and field: {cells:?}"
            );
            HeaderField {
                offset: cells[0].parse().expect("offset column is a number"),
                size: cells[1].parse().expect("size column is a number"),
                // Leaked so the comparison is against &'static str without a
                // second representation of the same data.
                name: Box::leak(strip_code_ticks(&cells[2]).into_boxed_str()),
            }
        })
        .collect();

    assert_eq!(
        declared.len(),
        HEADER_LAYOUT.len(),
        "the document declares {} header fields, the crate exports {}",
        declared.len(),
        HEADER_LAYOUT.len()
    );
    for (document_field, code_field) in declared.iter().zip(HEADER_LAYOUT) {
        assert_eq!(
            document_field, code_field,
            "header field mismatch: document says {document_field:?}, code says {code_field:?}"
        );
    }
}

/// The layout has to be gapless and add up, or the offsets could agree with a
/// document that describes a header of a different size.
#[test]
fn the_exported_layout_is_gapless_and_sums_to_the_header_size() {
    let mut expected_offset = 0u32;
    for field in HEADER_LAYOUT {
        assert_eq!(
            field.offset, expected_offset,
            "field {} starts at {} but the previous field ends at {expected_offset}",
            field.name, field.offset
        );
        expected_offset += field.size;
    }
    assert_eq!(
        expected_offset, HEADER_BYTES,
        "the header fields sum to {expected_offset}, HEADER_BYTES is {HEADER_BYTES}"
    );
}

/// The header size, the caps and the credit ceiling appear in the document as
/// prose numbers. Those are the numbers a producer author reads, so they are
/// checked too -- a document that says 64 while the code says 80 is worse than
/// no document.
#[test]
fn the_document_states_the_same_constants_the_code_enforces() {
    let document = document();

    assert!(
        document.contains(&format!("## Header — {HEADER_BYTES} bytes, fixed")),
        "the header heading does not state {HEADER_BYTES} bytes"
    );
    assert!(
        document.contains(&format!("exactly `{HEADER_BYTES}`")),
        "the header_bytes rule does not state {HEADER_BYTES}"
    );
    assert!(
        document.contains(&format!("`<= {MAX_TOTAL_BYTES}`")),
        "the total_bytes rule does not state the {MAX_TOTAL_BYTES}-byte ceiling"
    );
    assert!(
        document.contains(&format!("`<= {MAX_SECTIONS}`")),
        "the section_count rule does not state the cap of {MAX_SECTIONS}"
    );
}

// --- rejection codes: source, exported list, and document, three ways -------
//
// The lists `WireError::ALL` and `INGRESS_ERROR_CODES` exist so consumers can
// iterate every failure. A hand-written list of variants is exactly how
// coverage goes quietly missing -- a variant is added, nothing iterates it, and
// the test that "covers all of them" keeps passing. So the lists are checked
// against the enum and the constants as they appear in this crate's own source,
// and both are checked against the document's table.

fn source(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// `Name = 12,` lines inside `pub enum WireError { .. }`.
fn wire_error_variants_in_source() -> Vec<(String, u32)> {
    let text = source("lib.rs");
    let start = text
        .find("pub enum WireError {")
        .expect("the WireError enum is declared in lib.rs");
    let body = &text[start..];
    let end = body.find("\n}").expect("the enum has a closing brace");
    let mut variants = Vec::new();
    for line in body[..end].lines().skip(1) {
        let line = line.trim().trim_end_matches(',');
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let (name, value) = line
            .split_once(" = ")
            .unwrap_or_else(|| panic!("every variant carries an explicit code: {line:?}"));
        variants.push((
            name.trim().to_string(),
            value.trim().parse().expect("the code is a number"),
        ));
    }
    assert!(!variants.is_empty(), "no variants parsed out of the enum");
    variants
}

/// `pub const INGRESS_ERROR_FOO: u32 = 1001;` lines in ingress.rs.
fn ingress_error_constants_in_source() -> Vec<(String, u32)> {
    let text = source("ingress.rs");
    let mut constants = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub const INGRESS_ERROR_") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(": u32 = ") else {
            continue;
        };
        // Skip the range marker: it is a boundary, not a reason a packet failed.
        if name == "BASE" {
            continue;
        }
        constants.push((
            format!("INGRESS_ERROR_{name}"),
            value
                .trim_end_matches(';')
                .trim()
                .parse()
                .expect("the code is a number"),
        ));
    }
    assert!(!constants.is_empty(), "no INGRESS_ERROR_* constants parsed");
    constants
}

#[test]
fn the_exported_wire_error_list_covers_every_variant_in_the_source() {
    let in_source = wire_error_variants_in_source();
    let exported: Vec<(String, u32)> = WireError::ALL
        .iter()
        .map(|error| (error.name(), error.code()))
        .collect();

    assert_eq!(
        exported.len(),
        in_source.len(),
        "the source declares {} variants, WireError::ALL lists {}",
        in_source.len(),
        exported.len()
    );
    assert_eq!(
        exported, in_source,
        "WireError::ALL is not the source's variants in the source's order"
    );

    // Contiguous from 1, so a new variant cannot be given a code that leaves a
    // hole for a retired one -- and so `ALL.len()` is a usable count.
    for (index, (name, code)) in exported.iter().enumerate() {
        assert_eq!(
            *code,
            index as u32 + 1,
            "{name} is {code}; codes must be contiguous from 1"
        );
    }
}

#[test]
fn the_exported_ingress_code_list_covers_every_constant_in_the_source() {
    let in_source = ingress_error_constants_in_source();
    let exported = INGRESS_ERROR_CODES;

    assert_eq!(
        exported.len(),
        in_source.len(),
        "the source declares {} INGRESS_ERROR_* codes, INGRESS_ERROR_CODES lists {}",
        in_source.len(),
        exported.len()
    );
    let source_codes: Vec<u32> = in_source.iter().map(|(_, code)| *code).collect();
    assert_eq!(exported, source_codes.as_slice());

    for (index, (name, code)) in in_source.iter().enumerate() {
        assert_eq!(
            *code,
            INGRESS_ERROR_BASE + index as u32,
            "{name} is {code}; identity codes must be contiguous from {INGRESS_ERROR_BASE}"
        );
    }

    // The two ranges must not meet. One telemetry field carries either.
    let highest_envelope = WireError::ALL
        .iter()
        .map(|error| error.code())
        .max()
        .expect("there is at least one envelope error");
    assert!(
        highest_envelope < INGRESS_ERROR_BASE,
        "envelope code {highest_envelope} has reached the identity range"
    );
}

#[test]
fn the_document_rejection_table_lists_exactly_the_codes_the_crate_defines() {
    let document = document();
    let rows = table_after(&document, "## Rejection codes");

    let mut documented: Vec<(u32, String)> = Vec::new();
    for cells in &rows {
        assert!(cells.len() >= 2, "a rejection row needs a code and a name");
        documented.push((
            cells[0].parse().expect("the code column is a number"),
            strip_code_ticks(&cells[1]),
        ));
    }

    let mut expected: Vec<(u32, String)> = WireError::ALL
        .iter()
        .map(|error| (error.code(), error.name()))
        .collect();
    for (name, code) in ingress_error_constants_in_source() {
        // The document names them without the shared prefix, which is how they
        // read in a table that already says what range they are in.
        expected.push((code, name.trim_start_matches("INGRESS_ERROR_").to_string()));
    }

    assert_eq!(
        documented, expected,
        "the document's rejection table and the crate's codes disagree"
    );
}

/// The synchronous barrier's record gets the same treatment as the frame
/// header, for the same reason: it is a layout two implementations write, and
/// "checked by eye at review time" is how one of them ends up writing a
/// different one.
#[test]
fn the_document_sync_record_table_matches_the_exported_layout() {
    let document = document();
    let rows = table_after(&document, "### Record");

    let declared: Vec<HeaderField> = rows
        .iter()
        .map(|cells| {
            assert!(
                cells.len() >= 3,
                "a record row needs offset, size and field: {cells:?}"
            );
            HeaderField {
                offset: cells[0].parse().expect("offset column is a number"),
                size: cells[1].parse().expect("size column is a number"),
                name: Box::leak(strip_code_ticks(&cells[2]).into_boxed_str()),
            }
        })
        .collect();

    assert_eq!(
        declared.len(),
        SYNC_LAYOUT.len(),
        "the document declares {} sync fields, the crate exports {}",
        declared.len(),
        SYNC_LAYOUT.len()
    );
    for (document_field, code_field) in declared.iter().zip(SYNC_LAYOUT) {
        assert_eq!(
            document_field, code_field,
            "sync record mismatch: document says {document_field:?}, code says {code_field:?}"
        );
    }

    assert!(
        document.contains(&format!("### Record — {SYNC_RECORD_BYTES} bytes, fixed")),
        "the sync record heading does not state {SYNC_RECORD_BYTES} bytes"
    );
}

/// A layout table under `heading`, as fields.
fn layout_table(document: &str, heading: &str) -> Vec<HeaderField> {
    table_after(document, heading)
        .iter()
        .map(|cells| {
            assert!(
                cells.len() >= 3,
                "a row under {heading:?} needs offset, size and field: {cells:?}"
            );
            HeaderField {
                offset: cells[0].parse().expect("offset column is a number"),
                size: cells[1].parse().expect("size column is a number"),
                name: Box::leak(strip_code_ticks(&cells[2]).into_boxed_str()),
            }
        })
        .collect()
}

/// The two bodies a synchronous call travels as when there is no shared record.
/// A transport and a producer are written from these tables; a table that
/// named a field the decoder does not read at that offset would be a
/// `readPixels` answered with another field's value.
#[test]
fn the_document_call_and_answer_tables_match_the_exported_layouts() {
    let document = document();
    for (heading, exported, fixed_bytes) in [
        (
            "### A request as one body",
            SYNC_CALL_LAYOUT,
            SYNC_CALL_HEADER_BYTES,
        ),
        (
            "### An answer as one body",
            SYNC_ANSWER_LAYOUT,
            SYNC_ANSWER_HEADER_BYTES,
        ),
    ] {
        let declared = layout_table(&document, heading);
        assert_eq!(
            declared.as_slice(),
            exported,
            "{heading}: the document and the crate disagree"
        );
        let last = exported.last().expect("a layout has fields");
        assert_eq!(
            (last.offset + last.size) as usize,
            fixed_bytes,
            "{heading}: the fixed part does not end where the crate says it does"
        );
        let section = &document[document.find(heading).expect("heading")..];
        assert!(
            section.contains(&format!("from offset {fixed_bytes}")),
            "{heading}: the document does not say what follows starts at {fixed_bytes}"
        );
    }

    // The bounds the prose states, against the constants the decoder enforces.
    // Whitespace collapsed, so reflowing a paragraph is not a contract change.
    let prose = document.split_whitespace().collect::<Vec<_>>().join(" ");
    for (stated, what) in [
        (
            format!("at most {SYNC_CALL_MAX_BYTES} bytes"),
            "the body bound",
        ),
        (
            format!("`1..={SYNC_CALL_MAX_TIMEOUT_MILLIS}`"),
            "the timeout bound",
        ),
        (
            format!("`1..={MAX_REPLY_BYTES}`"),
            "the reply reservation bound",
        ),
    ] {
        assert!(
            prose.contains(&stated),
            "the document does not state {what} as {stated:?}"
        );
    }
}

/// `Name = 12,` lines inside a named enum in `file`.
fn variants_in(file: &str, enum_name: &str) -> Vec<(String, u32)> {
    let text = source(file);
    let start = text
        .find(&format!("pub enum {enum_name} {{"))
        .unwrap_or_else(|| panic!("{file} declares no enum {enum_name}"));
    let body = &text[start..];
    let end = body.find("\n}").expect("the enum has a closing brace");
    let mut variants = Vec::new();
    for line in body[..end].lines().skip(1) {
        let line = line.trim().trim_end_matches(',');
        if line.is_empty() || line.starts_with("//") || line.starts_with("#[") {
            continue;
        }
        let Some((name, value)) = line.split_once(" = ") else {
            continue;
        };
        variants.push((
            name.trim().to_string(),
            value.trim().parse().expect("the code is a number"),
        ));
    }
    assert!(
        !variants.is_empty(),
        "no variants parsed out of {enum_name}"
    );
    variants
}

/// The protocol enums cross to the C ABI, where a consumer iterates `ALL` to
/// prove it can report every one. A list that silently fell behind its enum
/// would make that consumer's coverage a claim about whenever someone last
/// looked.
/// One enum to check: which file declares it, its name, and what its exported
/// `ALL` list says. Named because the tuple was complex enough that clippy
/// asked, and it reads better with a name anyway.
struct EnumUnderTest {
    file: &'static str,
    name: &'static str,
    exported: Vec<(String, u32)>,
}

#[test]
fn the_protocol_enums_export_every_variant_their_source_declares() {
    let cases = [
        EnumUnderTest {
            file: "sync.rs",
            name: "SyncState",
            exported: SyncState::ALL
                .iter()
                .map(|state| (format!("{state:?}"), state.code()))
                .collect(),
        },
        EnumUnderTest {
            file: "sync.rs",
            name: "SyncError",
            exported: SyncError::ALL
                .iter()
                .map(|error| (format!("{error:?}"), error.code()))
                .collect(),
        },
        EnumUnderTest {
            file: "resource.rs",
            name: "ResourceState",
            exported: ResourceState::ALL
                .iter()
                .map(|state| (format!("{state:?}"), state.code()))
                .collect(),
        },
        EnumUnderTest {
            file: "resource.rs",
            name: "ResourceError",
            exported: ResourceError::ALL
                .iter()
                .map(|error| (format!("{error:?}"), error.code()))
                .collect(),
        },
    ];

    for EnumUnderTest {
        file,
        name: enum_name,
        exported,
    } in cases
    {
        let in_source = variants_in(file, enum_name);
        assert_eq!(
            exported.len(),
            in_source.len(),
            "{enum_name}: the source declares {} variants, ALL lists {}",
            in_source.len(),
            exported.len()
        );
        assert_eq!(
            exported, in_source,
            "{enum_name}: ALL is not the source's variants in order"
        );

        // Contiguous, so `ALL.len()` is a usable count and a retired variant
        // cannot leave a hole a later one silently fills.
        let base = in_source[0].1;
        for (index, (name, code)) in in_source.iter().enumerate() {
            assert_eq!(
                *code,
                base + index as u32,
                "{enum_name}::{name} is {code}; codes must be contiguous from {base}"
            );
        }
    }
}
