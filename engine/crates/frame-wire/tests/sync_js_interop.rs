//! `readPixels` argument records built by the JavaScript producer, read by the
//! Rust decoder.
//!
//! Both sides were written from the same table in
//! `contracts/frame-wire/wire-v1.md`. That is the right way to write them and
//! no evidence at all that they agree: a table can be read two ways, and the
//! way a disagreement reaches a user is a `readPixels` answered over the wrong
//! rectangle -- a picture that is subtly not the one the game asked for, which
//! nothing but a screenshot would catch.
//!
//! `#[ignore]` because it needs records a `cargo test` invocation has no way to
//! produce: they come from `node`, driven by
//! `scripts/test-frame-wire-js-encoder.sh`. The ignore is the visible form of
//! that dependency -- the alternative, a test that returns early when an
//! environment variable is unset, is the silent-green shape this repository
//! keeps finding.

use std::{fs, path::PathBuf};

use frame_wire::sync::{GL_RGBA, GL_UNSIGNED_BYTE, ReadPixelsParams};

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

fn number(line: &str, key: &str) -> i64 {
    field(line, key)
        .parse()
        .unwrap_or_else(|error| panic!("{key} is not a number: {error}"))
}

#[test]
#[ignore = "needs records emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn read_pixels_arguments_from_the_javascript_producer_decode_unchanged() {
    let directory = PathBuf::from(
        std::env::var("MIGO_JS_SYNC_PARAM_DIR")
            .expect("MIGO_JS_SYNC_PARAM_DIR must name the emitter's output directory"),
    );
    let manifest = fs::read_to_string(directory.join("manifest.jsonl"))
        .expect("the emitter writes manifest.jsonl beside the records");

    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    // A manifest that had somehow become empty would make this test pass while
    // reading nothing, which is the shape it exists to avoid.
    assert!(
        entries.len() >= 16,
        "the manifest holds {} entries; the emitter writes at least 16",
        entries.len()
    );

    let mut decoded = 0usize;
    for entry in &entries {
        let name = field(entry, "file");
        let bytes =
            fs::read(directory.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"));
        let params = ReadPixelsParams::decode(&bytes)
            .unwrap_or_else(|error| panic!("{name}: the JavaScript producer emitted {error}"));

        // Every field, including the signed ones. GL allows a negative origin,
        // and a decoder that only ever saw a non-negative x would sign-extend
        // wrongly without anyone noticing until a game scrolled.
        assert_eq!(
            i64::from(params.canvas_id),
            number(entry, "canvas_id"),
            "{name} canvas id"
        );
        assert_eq!(i64::from(params.x), number(entry, "x"), "{name} x");
        assert_eq!(i64::from(params.y), number(entry, "y"), "{name} y");
        assert_eq!(
            i64::from(params.width),
            number(entry, "width"),
            "{name} width"
        );
        assert_eq!(
            i64::from(params.height),
            number(entry, "height"),
            "{name} height"
        );
        assert_eq!(
            i64::from(params.format),
            number(entry, "format"),
            "{name} format"
        );
        assert_eq!(
            i64::from(params.type_),
            number(entry, "type"),
            "{name} type"
        );
        assert_eq!(
            params.format, GL_RGBA,
            "{name} format is the one this host reads back"
        );
        assert_eq!(params.type_, GL_UNSIGNED_BYTE, "{name} type");

        // And the size the two sides will independently compute for the answer.
        // The producer sizes the buffer it reads from and the host sizes the
        // readback; a disagreement here is a truncated or over-read picture.
        let expected = number(entry, "reply_bytes");
        assert_eq!(
            i64::from(params.reply_bytes().expect("a real rectangle fits in u32")),
            expected,
            "{name}: the two sides disagree about how large the answer is"
        );
        decoded += 1;
    }

    println!("decoded {decoded} JavaScript-encoded readPixels argument records");
}
