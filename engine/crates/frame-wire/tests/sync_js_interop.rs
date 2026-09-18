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

use frame_wire::sync::{
    Canvas2DQueryParams, GL_QUERY_HEADER_BYTES, GL_RGBA, GL_UNSIGNED_BYTE, GlQueryParams,
    ReadPixelsParams, canvas2d_query, gl_query,
};

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

/// A string-valued field, for 64-bit values the manifest writes as strings.
fn wide(line: &str, key: &str) -> u64 {
    field(line, key)
        .parse()
        .unwrap_or_else(|error| panic!("{key} is not a u64: {error}"))
}

#[test]
#[ignore = "needs calls emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn calls_from_the_javascript_producer_decode_unchanged() {
    use frame_wire::sync::SyncCall;

    let directory = PathBuf::from(
        std::env::var("MIGO_JS_SYNC_CALL_DIR")
            .expect("MIGO_JS_SYNC_CALL_DIR must name the emitter's output directory"),
    );
    let manifest = fs::read_to_string(directory.join("calls.jsonl"))
        .expect("the emitter writes calls.jsonl beside the bodies");
    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        entries.len() >= 64,
        "the manifest holds {} calls; the emitter writes 64",
        entries.len()
    );

    for entry in &entries {
        let name = field(entry, "file");
        let bytes =
            fs::read(directory.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"));
        let call = SyncCall::decode(&bytes)
            .unwrap_or_else(|error| panic!("{name}: the JavaScript producer emitted {error}"));
        assert_eq!(
            (
                call.runtime_generation,
                call.surface_generation,
                call.resource_epoch,
                call.triggering_sequence,
            ),
            (
                wide(entry, "runtime_generation"),
                wide(entry, "surface_generation"),
                wide(entry, "resource_epoch"),
                wide(entry, "triggering_sequence"),
            ),
            "{name}: a 64-bit field"
        );
        assert_eq!(
            i64::from(call.operation),
            number(entry, "operation"),
            "{name} operation"
        );
        assert_eq!(
            i64::from(call.max_reply_bytes),
            number(entry, "max_reply_bytes"),
            "{name} reservation"
        );
        assert_eq!(
            i64::from(call.timeout_millis),
            number(entry, "timeout_millis"),
            "{name} timeout"
        );
        assert_eq!(
            call.service_sequence,
            wide(entry, "service_sequence"),
            "{name} service_sequence"
        );
        assert_eq!(
            call.params.len() as i64,
            number(entry, "params_bytes"),
            "{name} argument length"
        );
        assert_eq!(
            call.params.iter().map(|&b| i64::from(b)).sum::<i64>(),
            number(entry, "params_sum"),
            "{name} arguments"
        );
    }
    println!(
        "decoded {} JavaScript-encoded synchronous calls",
        entries.len()
    );
}

#[test]
#[ignore = "writes answers for node to read; run through scripts/test-frame-wire-js-encoder.sh"]
fn the_rust_host_writes_answers_the_producer_reads() {
    use frame_wire::sync::{SYNC_ANSWER_HEADER_BYTES, SyncAnswer, SyncError, SyncState};

    let directory = PathBuf::from(
        std::env::var("MIGO_SYNC_ANSWER_OUT_DIR")
            .expect("MIGO_SYNC_ANSWER_OUT_DIR must name where the answers go"),
    );
    fs::create_dir_all(&directory).expect("answer directory");

    let mut answers: Vec<(SyncAnswer, Vec<u8>)> = vec![
        (
            SyncAnswer::failed(0, SyncError::UnsupportedOperation),
            Vec::new(),
        ),
        (
            SyncAnswer::failed(u32::MAX, SyncError::OperationFailed),
            Vec::new(),
        ),
        (
            SyncAnswer {
                state: SyncState::Cancelled,
                error: None,
                request_id: 0x8000_0001,
                reply_bytes: 0,
            },
            Vec::new(),
        ),
    ];
    for error in SyncError::ALL {
        answers.push((SyncAnswer::failed(error.code() * 7, *error), Vec::new()));
    }
    for (index, length) in [1usize, 4, 255, 4096, 65_537].into_iter().enumerate() {
        let reply: Vec<u8> = (0..length).map(|i| (i * 31 + index) as u8).collect();
        answers.push((
            SyncAnswer {
                state: SyncState::Ready,
                error: None,
                request_id: 0x0102_0304 + index as u32,
                reply_bytes: length as u32,
            },
            reply,
        ));
    }

    let mut manifest = String::new();
    for (index, (answer, reply)) in answers.iter().enumerate() {
        let name = format!("answer-{index:04}.bin");
        let mut body = vec![0u8; answer.body_bytes()];
        answer.write_header(&mut body);
        body[SYNC_ANSWER_HEADER_BYTES..].copy_from_slice(reply);
        fs::write(directory.join(&name), &body).expect("write answer");
        manifest.push_str(&format!(
            "{{\"file\":\"{name}\",\"state\":{},\"error\":{},\"request_id\":{},\"reply_bytes\":{},\"reply_sum\":{}}}\n",
            answer.state.code(),
            answer.error.map_or(0, SyncError::code),
            answer.request_id,
            answer.reply_bytes,
            reply.iter().map(|&b| u64::from(b)).sum::<u64>(),
        ));
    }
    fs::write(directory.join("answers.jsonl"), manifest).expect("write manifest");
    println!("wrote {} synchronous answers", answers.len());
}

/// The same check for the WebGL queries' arguments.
///
/// A query is three numbers and a name, and every one of them is a way to ask
/// about the wrong thing: an object id read from the wrong word asks about
/// another program, and a name length read as characters rather than bytes
/// truncates a uniform's name into one that does not exist. Both are answered
/// rather than refused -- `getUniformLocation` returns -1 for a name nothing
/// has -- so the failure is a uniform that silently does nothing.
#[test]
#[ignore = "needs records emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn gl_query_arguments_from_the_javascript_producer_decode_unchanged() {
    let directory = PathBuf::from(
        std::env::var("MIGO_JS_GL_QUERY_DIR")
            .expect("MIGO_JS_GL_QUERY_DIR must name the emitter's output directory"),
    );
    let manifest = fs::read_to_string(directory.join("manifest.jsonl"))
        .expect("the emitter writes manifest.jsonl beside the records");
    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        entries.len() >= 15,
        "the manifest has {} entries; a run that checked nothing would pass",
        entries.len()
    );

    let mut kinds_seen = std::collections::BTreeSet::new();
    let mut names_with_padding = 0;
    for line in entries {
        let file = field(line, "file");
        let bytes = fs::read(directory.join(file)).expect("a record the manifest names");
        let decoded = GlQueryParams::decode(&bytes)
            .unwrap_or_else(|error| panic!("{file} was refused: {error:?}"));

        assert_eq!(
            i64::from(decoded.kind),
            number(line, "kind"),
            "{file}: kind"
        );
        assert_eq!(
            i64::from(decoded.canvas_id),
            number(line, "canvas_id"),
            "{file}: canvas"
        );
        assert_eq!(
            i64::from(decoded.object),
            number(line, "object"),
            "{file}: object"
        );
        assert_eq!(
            i64::from(decoded.pname),
            number(line, "pname"),
            "{file}: pname"
        );
        assert_eq!(
            i64::from(decoded.extra),
            number(line, "extra"),
            "{file}: extra"
        );
        assert_eq!(
            decoded.name.len() as i64,
            number(line, "name_bytes"),
            "{file}: the name's length is a byte count"
        );
        assert_eq!(
            bytes.len() as i64,
            number(line, "total_bytes"),
            "{file}: the record's size"
        );
        // Re-encoding must produce the same bytes: one encoding of a request,
        // which is what makes a comparison of bytes meaningful at all.
        assert_eq!(
            decoded.encode(),
            bytes,
            "{file} did not survive a round trip through the Rust encoder"
        );
        if (bytes.len() - GL_QUERY_HEADER_BYTES) != decoded.name.len() {
            names_with_padding += 1;
        }
        kinds_seen.insert(decoded.kind);
    }

    assert_eq!(
        kinds_seen.len(),
        (gl_query::TRANSFORM_FEEDBACK_VARYING - gl_query::PROGRAM_PARAMETER + 1) as usize,
        "every query kind has to appear, or the corpus covers a table it does not exercise"
    );
    assert!(
        names_with_padding > 0,
        "no record had a padded name, so the padding rule went unchecked"
    );
    println!("read {} JavaScript-encoded query records", kinds_seen.len());
}

/// The same, for the Canvas2D queries, whose arguments are two strings.
///
/// A pair of payloads is where an encoder goes wrong: the second length read
/// from the first's unpadded end reads the font out of the middle of the text,
/// and `measureText` then measures a string nobody passed. Which is answered,
/// not refused -- a label laid out to the wrong width.
#[test]
#[ignore = "needs records emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn canvas2d_query_arguments_from_the_javascript_producer_decode_unchanged() {
    let directory = PathBuf::from(
        std::env::var("MIGO_JS_GL_QUERY_DIR")
            .expect("MIGO_JS_GL_QUERY_DIR must name the emitter's output directory"),
    );
    let manifest = fs::read_to_string(directory.join("canvas2d-manifest.jsonl"))
        .expect("the emitter writes canvas2d-manifest.jsonl beside the records");
    let entries: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        entries.len() >= 12,
        "the manifest has {} entries; a run that checked nothing would pass",
        entries.len()
    );

    let mut kinds_seen = std::collections::BTreeSet::new();
    let mut padded_pairs = 0;
    for line in &entries {
        let file = field(line, "file");
        let bytes = fs::read(directory.join(file)).expect("a record the manifest names");
        let decoded = Canvas2DQueryParams::decode(&bytes)
            .unwrap_or_else(|error| panic!("{file} was refused: {error:?}"));

        assert_eq!(
            i64::from(decoded.kind),
            number(line, "kind"),
            "{file}: kind"
        );
        assert_eq!(
            i64::from(decoded.canvas_id),
            number(line, "canvas_id"),
            "{file}: canvas"
        );
        assert_eq!(
            i64::from(decoded.flags),
            number(line, "flags"),
            "{file}: flags"
        );
        assert_eq!(
            decoded.text.len() as i64,
            number(line, "text_bytes"),
            "{file}: the text's length is a byte count"
        );
        assert_eq!(
            decoded.font.len() as i64,
            number(line, "font_bytes"),
            "{file}: the font's length is a byte count"
        );
        assert_eq!(decoded.text_str(), field(line, "text"), "{file}: the text");
        assert_eq!(decoded.font_str(), field(line, "font"), "{file}: the font");
        // The size crossed as an `f32`, which the manifest prints rounded the
        // same way; a double that crossed whole would not match.
        let expected: f32 = field(line, "number").parse().expect("a number");
        assert_eq!(decoded.number_f32(), expected, "{file}: the size");
        assert_eq!(
            bytes.len() as i64,
            number(line, "total_bytes"),
            "{file}: the record's size"
        );
        assert_eq!(
            decoded.encode(),
            bytes,
            "{file} did not survive a round trip through the Rust encoder"
        );
        if decoded.text.len() % 4 != 0 && decoded.font.len() % 4 != 0 {
            padded_pairs += 1;
        }
        kinds_seen.insert(decoded.kind);
    }
    assert_eq!(
        kinds_seen.len(),
        (canvas2d_query::TEXT_LINE_HEIGHT - canvas2d_query::MEASURE_TEXT + 1) as usize,
        "every 2D query kind has to appear"
    );
    assert!(
        padded_pairs > 0,
        "no record had two padded strings, so the pair's padding went unchecked"
    );
    println!(
        "read {} JavaScript-encoded Canvas2D query records",
        entries.len()
    );
}
