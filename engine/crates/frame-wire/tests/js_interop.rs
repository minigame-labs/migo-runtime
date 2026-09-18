//! Packets built by the JavaScript encoder, validated by the Rust reader.
//!
//! The golden corpus pins three shapes byte for byte, which answers "do both
//! encoders agree on these bytes". It cannot answer "does the JavaScript
//! encoder ever produce something this reader refuses", because three cases do
//! not cover section counts, ragged payload lengths, the padding those imply,
//! or wide-field values that only occur at run time. This does.
//!
//! `#[ignore]` because it needs packets that a `cargo test` invocation has no
//! way to produce: they come from `node`, driven by
//! `scripts/test-frame-wire-js-encoder.sh`. The ignore is the visible form of
//! that dependency -- the alternative, a test that returns early when an
//! environment variable is unset, is the silent-green shape this repository
//! keeps finding, and the gate is what guarantees this actually runs.

use std::{fs, path::PathBuf};

use frame_wire::{FrameIngress, IngressDecision, validate};

/// One flat JSON object's value for `key`.
///
/// A JSON dependency here would be a dependency of the crate whose point is a
/// small trust boundary. The manifest is written by a script in this repository
/// as one flat object per line, which is what makes five lines enough.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("manifest line has no {key}: {line}"))
        + needle.len();
    let rest = line[start..].trim_start();
    match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"').expect("closing quote")],
        None => rest[..rest.find([',', '}']).expect("value ends")].trim(),
    }
}

#[test]
#[ignore = "needs packets emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn packets_from_the_javascript_encoder_are_accepted_unchanged() {
    let directory = PathBuf::from(
        std::env::var("MIGO_JS_PACKET_DIR")
            .expect("MIGO_JS_PACKET_DIR must name the emitter's output directory"),
    );
    let manifest = fs::read_to_string(directory.join("manifest.jsonl"))
        .expect("the emitter writes manifest.jsonl beside the packets");

    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        entries.len() >= 16,
        "the manifest holds {} entries; the emitter writes at least 16",
        entries.len()
    );

    let mut validated = 0usize;
    let mut total_bytes = 0usize;
    for entry in &entries {
        let name = field(entry, "name");
        let bytes =
            fs::read(directory.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"));
        let frame = validate(&bytes)
            .unwrap_or_else(|error| panic!("{name}: the JavaScript encoder produced {error}"));

        // Every wide field, at full width. A truncating read on either side
        // survives small values and fails here.
        assert_eq!(
            frame.launch_nonce().to_string(),
            field(entry, "launch_nonce"),
            "{name}: launch_nonce"
        );
        assert_eq!(
            frame.sequence().to_string(),
            field(entry, "sequence"),
            "{name}: sequence"
        );
        assert_eq!(
            frame.runtime_generation().to_string(),
            field(entry, "runtime_generation"),
            "{name}: runtime_generation"
        );
        assert_eq!(
            frame.surface_generation().to_string(),
            field(entry, "surface_generation"),
            "{name}: surface_generation"
        );
        assert_eq!(
            frame.resource_epoch().to_string(),
            field(entry, "resource_epoch"),
            "{name}: resource_epoch"
        );
        assert_eq!(
            frame.frame_id().to_string(),
            field(entry, "frame_id"),
            "{name}: frame_id"
        );
        assert_eq!(
            frame.section_count().to_string(),
            field(entry, "section_count"),
            "{name}: section_count"
        );
        assert_eq!(
            frame.total_bytes().to_string(),
            field(entry, "bytes"),
            "{name}: total_bytes"
        );
        assert_eq!(
            frame.presents().to_string(),
            field(entry, "presents"),
            "{name}: PRESENT, or a barrier"
        );
        assert!(
            frame.command_stream().is_some(),
            "{name}: every packet carries a command stream"
        );

        validated += 1;
        total_bytes += bytes.len();
    }

    assert_eq!(
        validated,
        entries.len(),
        "every manifest entry was validated"
    );

    // And the same packets through the ingress, in sequence order, which is the
    // path a real producer takes. The emitter numbers them from 1, so strict
    // contiguity is exercised rather than merely satisfied.
    let first = entries.first().expect("at least one entry");
    let nonce: u128 = field(first, "launch_nonce").parse().expect("nonce parses");
    let generation: u64 = field(first, "runtime_generation")
        .parse()
        .expect("generation parses");
    let mut ingress = FrameIngress::new(nonce, generation);
    // The producer's epoch, adopted before the packet is offered. A host that
    // skipped this would be rejecting the frame for naming resources from an
    // epoch the host never entered -- which is the check working, and is worth
    // saying out loud here because it is the first thing that goes wrong when a
    // real transport is wired up.
    let epoch: u64 = field(first, "resource_epoch")
        .parse()
        .expect("epoch parses");
    assert!(ingress.set_resource_epoch(epoch), "the epoch only advances");
    ingress.mark_resources_ready();
    let bytes = fs::read(directory.join(field(first, "name"))).expect("read the first packet");
    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(
        outcome.decision,
        IngressDecision::Accepted,
        "the first packet of a generation must be accepted (error {})",
        outcome.wire_error_code
    );
    assert!(frame.is_some());

    println!("validated {validated} JavaScript-encoded packets, {total_bytes} bytes");
}
