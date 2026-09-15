//! The JavaScript encoder and the Rust wire-format table describe one thing.
//!
//! These cases live here rather than beside the validator because they are
//! about *this crate's* JavaScript: the validator moved to `migo-frame-wire` so
//! the cross-process consumer could use it without linking a JavaScript engine,
//! and a test that reads `00_render_command_stream.js` would have dragged that
//! coupling along with it. The format is shared; the encoder that has to agree
//! with it is not.
//!
//! `include_str!` resolves against this file, so the encoder read here is the
//! one this crate actually bakes into the V8 snapshot.

#![cfg(test)]

mod js_agreement {
    // ── JS/Rust constant contract ────────────────────────────────────────────

    /// Every opcode the wire-format table declares appears in the JavaScript
    /// encoder with the same number.
    ///
    /// Both tables are PARSED, not restated. This test used to hold a
    /// hand-written list of sixty-nine name and value pairs, and a list someone
    /// has to extend is a list that falls behind: an opcode added to the Rust
    /// table and forgotten here would have left the test passing on the
    /// sixty-nine it already knew.
    ///
    /// The WebContent producer has a third copy of this table and cannot be
    /// read from inside this crate; `scripts/test-render-opcode-agreement.sh`
    /// checks all three together.
    #[test]
    fn the_javascript_encoder_declares_every_opcode_the_rust_table_does() {
        const RUST: &str = include_str!("../../../../frame-wire/src/gl.rs");
        const JS: &str = include_str!("00_render_command_stream.js");

        fn parse(text: &str, prefix: &str, suffix: &str) -> Vec<(String, u32)> {
            let mut found = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix(prefix) else {
                    continue;
                };
                let Some((name, value)) = rest.split_once(suffix) else {
                    continue;
                };
                if !name.starts_with("OP_") {
                    continue;
                }
                let value = value.trim().trim_end_matches(';');
                let Ok(number) = value.parse::<u32>() else {
                    continue;
                };
                found.push((name.to_string(), number));
            }
            found
        }

        let rust = parse(RUST, "pub const ", ": u32 = ");
        let js = parse(JS, "const ", " = ");

        assert!(
            rust.len() >= 60,
            "only {} opcodes parsed from the Rust table; the pattern no longer matches",
            rust.len()
        );
        assert_eq!(
            rust.len(),
            js.len(),
            "the Rust table declares {} opcodes, the JavaScript encoder {}",
            rust.len(),
            js.len()
        );

        for (name, value) in &rust {
            let found = js
                .iter()
                .find(|(js_name, _)| js_name == name)
                .unwrap_or_else(|| panic!("the JavaScript encoder declares no {name}"));
            assert_eq!(
                found.1, *value,
                "{name} is {} in JavaScript and {value} in Rust",
                found.1
            );
        }
    }

    #[test]
    fn js_module_buffers_null_at_module_load() {
        let js = include_str!("00_render_command_stream.js");
        assert!(
            js.contains("= null;"),
            "JS module backing buffer vars must be initialized to null (lazy allocation)"
        );
    }

    /// No buffer references on globalThis.
    #[test]
    fn js_module_no_globalthis_assignment_of_buffers() {
        let js = include_str!("00_render_command_stream.js");
        assert!(
            !js.contains("globalThis."),
            "JS module must not assign buffers to globalThis"
        );
    }

    /// Hot encoders must not use rest params or temp words array.
    #[test]
    fn js_module_no_rest_params_in_encoders() {
        let js = include_str!("00_render_command_stream.js");
        assert!(
            !js.contains("...args"),
            "JS module hot encoders must not use rest args (...args)"
        );
        assert!(
            !js.contains("encodeRecord("),
            "JS module must not have a generic encodeRecord(...args) dispatcher"
        );
    }

    /// No temporary words array allocation in hot path.
    #[test]
    fn js_module_no_temp_words_array_in_hot_path() {
        let js = include_str!("00_render_command_stream.js");
        assert!(
            !js.contains("words = []"),
            "JS module must not allocate temporary words[] array in hot path"
        );
    }

    /// flushRenderCommandStream must pass used/cursor to op_submit_render_stream.
    #[test]
    fn js_module_flush_passes_cursor_to_op() {
        let js = include_str!("00_render_command_stream.js");
        assert!(
            js.contains("op_submit_render_stream"),
            "JS module must call op_submit_render_stream in flushRenderCommandStream"
        );
        assert!(
            js.contains("cursor"),
            "JS module flush must pass cursor (used_words) to op_submit_render_stream"
        );
    }
}

/// The Canvas2D half: does each encoder write the record the reader will read?
///
/// The opcode gate next door checks that the three tables agree on *numbers*.
/// It cannot see the mistake that actually costs a frame, which is an encoder
/// that names the right opcode and writes the wrong number of words. Nothing
/// about that is a type error: the header carries the count the encoder claims,
/// the reader trusts it, and the next record starts in the middle of the
/// previous one. The whole rest of the buffer decodes as garbage or is refused,
/// on a device, with the frame not drawing.
///
/// The spec side is not parsed here -- `record_spec` is *called*, so this checks
/// the encoders against the same function the validator uses rather than against
/// a second reading of it.
mod canvas2d_agreement {
    use frame_wire::canvas2d::{OP2D_BASE, OP2D_CREATE_CONTEXT, OP2D_END, OP2D_SELECT_CANVAS};
    use frame_wire::stream::RecordSpec;
    use std::collections::HashMap;

    const JS: &str = include_str!("00_render_command_stream.js");

    /// `OP2D_NAME` -> value, from the JavaScript declarations.
    fn declared_opcodes() -> HashMap<String, u32> {
        let mut found = HashMap::new();
        for line in JS.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("const OP2D_") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(" = ") else {
                continue;
            };
            let Ok(value) = value.trim().trim_end_matches(';').parse::<u32>() else {
                continue;
            };
            found.insert(format!("OP2D_{name}"), value);
        }
        found
    }

    /// The body of a `function <name>(...) { ... }` at column zero, brace-matched.
    fn function_body(name: &str) -> Option<&'static str> {
        let signature = format!("\nfunction {name}(");
        let start = JS.find(&signature)? + 1;
        let open = start + JS[start..].find('{')?;
        let bytes = JS.as_bytes();
        let mut depth = 0usize;
        for (offset, byte) in bytes[open..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&JS[open + 1..open + offset]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Every `function encode2d*` / `_encode2d*` declared, in file order.
    fn encoder_names() -> Vec<String> {
        let mut names = Vec::new();
        for line in JS.lines() {
            let Some(rest) = line.strip_prefix("function ") else {
                continue;
            };
            let Some((name, _)) = rest.split_once('(') else {
                continue;
            };
            if name.starts_with("encode2d") || name.starts_with("_encode2d") {
                names.push(name.to_string());
            }
        }
        names
    }

    /// The word count a body claims: `packHeader(<anything>, N)` and
    /// `cursor = base + N` must be the same N, or the encoder writes one length
    /// into the header and advances by another -- which desynchronises the
    /// stream from the record *after* it, so the record that looks wrong is
    /// never the one that is.
    fn word_count_of(body: &str) -> u32 {
        let header = after(body, "packHeader(").expect("an encoder packs a header");
        let header = header
            .split_once(", ")
            .expect("packHeader takes an opcode and a count")
            .1;
        let claimed = leading_number(header).expect("the word count is a literal");

        let advance = after(body, "cursor = base + ").expect("an encoder advances the cursor");
        let advanced = leading_number(advance).expect("the cursor advance is a literal");

        assert_eq!(
            claimed, advanced,
            "an encoder writes a header claiming {claimed} words and advances {advanced}"
        );
        claimed
    }

    fn after<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
        text.find(needle).map(|at| &text[at + needle.len()..])
    }

    /// The digits this text starts with, if it starts with any.
    fn leading_number(text: &str) -> Option<u32> {
        let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// Each 2D opcode, and the encoder that writes it.
    fn encoders_by_opcode() -> HashMap<u32, (String, u32)> {
        let declared = declared_opcodes();
        // Shared bodies first: the one-line wrappers name a helper, not a header.
        let mut helper_words: HashMap<String, u32> = HashMap::new();
        for name in encoder_names() {
            if !name.starts_with('_') {
                continue;
            }
            let body = function_body(&name).expect("a declared function has a body");
            helper_words.insert(name, word_count_of(body));
        }

        let mut by_opcode = HashMap::new();
        for name in encoder_names() {
            if name.starts_with('_') {
                continue;
            }
            let body = function_body(&name).expect("a declared function has a body");
            // A wrapper delegates to a helper; anything else writes its own header.
            let delegated = helper_words.iter().find_map(|(helper, words)| {
                let call = format!("{helper}(OP2D_");
                let at = body.find(&call)?;
                let rest = &body[at + helper.len() + 1..];
                let opcode = rest.split(&[',', ')'][..]).next()?.trim();
                Some((declared.get(opcode).copied(), *words, opcode.to_string()))
            });

            let (opcode, words) = match delegated {
                Some((Some(opcode), words, _)) => (opcode, words),
                Some((None, _, opcode)) => panic!("{name} names {opcode}, which is not declared"),
                None => {
                    let header = after(body, "packHeader(")
                        .unwrap_or_else(|| panic!("{name} neither delegates nor packs a header"));
                    let opcode_name = header.split(',').next().expect("an opcode name").trim();
                    let opcode = *declared
                        .get(opcode_name)
                        .unwrap_or_else(|| panic!("{name} names undeclared {opcode_name}"));
                    (opcode, word_count_of(body))
                }
            };

            if let Some((other, _)) = by_opcode.insert(opcode, (name.clone(), words)) {
                panic!("opcode {opcode} is encoded by both {other} and {name}");
            }
        }
        by_opcode
    }

    #[test]
    fn every_encoder_writes_the_record_length_the_reader_expects() {
        let by_opcode = encoders_by_opcode();
        assert!(
            by_opcode.len() >= 30,
            "only {} encoders parsed out of the JavaScript; the pattern no longer matches",
            by_opcode.len()
        );

        for (opcode, (name, words)) in &by_opcode {
            let spec = frame_wire::canvas2d::record_spec(*opcode)
                .unwrap_or_else(|| panic!("{name} encodes opcode {opcode}, which has no spec"));
            let RecordSpec::Fixed { word_count, .. } = spec else {
                panic!("{name} encodes opcode {opcode}, which is not a fixed-length record");
            };
            assert_eq!(
                *words, word_count,
                "{name} writes {words} words for opcode {opcode}; the reader expects {word_count}"
            );
        }
    }

    #[test]
    fn every_2d_opcode_has_an_encoder() {
        let by_opcode = encoders_by_opcode();
        for opcode in OP2D_BASE..OP2D_END {
            if opcode == OP2D_SELECT_CANVAS {
                // Emitted by `begin2d`, not by an encoder of its own: content
                // never asks for it, the stream needs it whenever the reader
                // would not know which canvas the records belong to.
                assert!(
                    JS.contains("packHeader(OP2D_SELECT_CANVAS, 2)"),
                    "nothing emits the canvas selection, so every 2D record is one \
                     the reader refuses for not knowing where to draw"
                );
                continue;
            }
            if opcode == OP2D_CREATE_CONTEXT {
                // The one opcode in this table that exists for the OTHER lane.
                //
                // In-process content brings a 2D context into existence through
                // `getContext('2d')`, which is an op crossing that happens once
                // per canvas and never on the per-frame path -- so this encoder
                // would emit a record nothing in this runtime needs. The
                // external-frame producer has no op crossing at all: the command
                // stream is its only path to the renderer, and without this
                // opcode its 2D records were accepted, decoded, batched and
                // silently dropped against a canvas that had no context.
                //
                // The rule this exception is carved out of was written when
                // every 2D opcode served in-process content, which was true
                // until it was not. What keeps the exception honest is that the
                // producer's own table is checked elsewhere:
                // `scripts/test-render-opcode-agreement.sh` requires the Rust
                // table, this file's table and the producer's
                // `render-opcodes.mjs` to agree, so "no encoder here" cannot
                // become "nobody emits it".
                // The DECLARATION, with the number this crate's own table
                // gives it -- not the bare name. `contains("OP2D_CREATE_CONTEXT")`
                // was the first version and it is satisfied by
                // `OP2D_CREATE_CONTEXT_RENAMED`, which is how the injection that
                // was supposed to turn this red came back green.
                let declaration = format!("const OP2D_CREATE_CONTEXT = {OP2D_CREATE_CONTEXT};");
                assert!(
                    JS.contains(&declaration),
                    "this runtime's table does not declare `{declaration}`, so the three tables \
                     scripts/test-render-opcode-agreement.sh compares cannot agree"
                );
                continue;
            }
            assert!(
                by_opcode.contains_key(&opcode),
                "opcode {opcode} is in the wire table with no encoder; content \
                 reaching it falls back to an op crossing, silently"
            );
        }
    }

    /// The direction flags are real bools on the wire -- the validator refuses
    /// anything but 0 or 1 -- so the encoder has to narrow a truthy argument
    /// rather than pass it through.
    #[test]
    fn the_bool_words_are_narrowed_where_the_spec_says_they_are() {
        for (opcode, (name, _)) in encoders_by_opcode() {
            let Some(RecordSpec::Fixed { bool_words, .. }) =
                frame_wire::canvas2d::record_spec(opcode)
            else {
                continue;
            };
            let body = function_body(&name).expect("a declared function has a body");
            for index in bool_words {
                let write = format!("_u32[base + {index}] =");
                let at = body.find(&write).unwrap_or_else(|| {
                    panic!("{name} never writes word {index}, which the spec says is a bool")
                });
                let line = body[at..].lines().next().expect("a line");
                assert!(
                    line.contains("? 1 : 0"),
                    "{name} writes word {index} as `{}`; the spec says it is a bool and \
                     the validator refuses anything but 0 or 1",
                    line.trim()
                );
            }
        }
    }
}

/// The Canvas2D facade's ordering barrier, checked rather than remembered.
///
/// 2D and GL records share one buffer, so between two encoded commands there is
/// nothing to do. What still needs saying is the boundary between an encoded
/// command and one that crosses as an op: text, gradients, patterns, dash
/// arrays, images and the synchronous reads carry strings or variable-length
/// arrays, have no record shape, and go straight to the collector. An op that
/// runs while records sit unsubmitted in the buffer reaches the collector first,
/// and the frame draws in the wrong order.
///
/// **The rule is total on purpose.** Flushing before an op is never wrong -- on
/// an empty buffer it is one comparison -- so there is no list of ops that are
/// allowed to skip it, and therefore no list to argue with when a new one is
/// added. A rule with exceptions is a rule whose exceptions grow.
mod canvas2d_ordering {
    const JS: &str = include_str!("02_2d_context.js");

    /// Not a function opening, however much the line looks like a call.
    const KEYWORDS: &[&str] = &["if", "for", "while", "switch", "catch", "return", "else"];

    /// Byte ranges of every function-like body: named functions, class methods,
    /// property accessors, and the object-literal methods a closure is handed
    /// back in.
    ///
    /// The *innermost* enclosing body is what a barrier has to be in. A barrier
    /// in the function that builds a closure says nothing about when the closure
    /// runs, and the one in this file runs on a property read, arbitrarily later.
    fn function_bodies() -> Vec<(usize, usize)> {
        let bytes = JS.as_bytes();
        let mut bodies = Vec::new();
        let mut offset = 0usize;
        for line in JS.split_inclusive('\n') {
            let trimmed = line.trim();
            // An arrow body is a function body: the barrier that holds for it is
            // the one inside it, not the one in the function that built it.
            let arrow = trimmed.ends_with('{') && trimmed.contains("=> {");
            let opens_body = arrow
                || trimmed.ends_with('{')
                    && !trimmed.starts_with("//")
                    && trimmed.split(['(', ' ']).next().is_some_and(|word| {
                        !KEYWORDS.contains(&word.trim_start_matches("function "))
                    })
                    && (trimmed.starts_with("function ")
                        || trimmed
                            .trim_start_matches("get ")
                            .trim_start_matches("set ")
                            .trim_start_matches("async ")
                            .split('(')
                            .next()
                            .is_some_and(|name| {
                                !name.is_empty()
                                    && !KEYWORDS.contains(&name)
                                    && name
                                        .chars()
                                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                            }))
                    && trimmed.contains('(');
            if opens_body {
                let open = offset + line.find('{').expect("checked it ends with a brace");
                let mut depth = 0usize;
                for (at, byte) in bytes[open..].iter().enumerate() {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                bodies.push((open, open + at));
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            offset += line.len();
        }
        bodies
    }

    /// Every `op_name(` call, as a byte offset and a name.
    fn op_calls() -> Vec<(usize, String)> {
        let bytes = JS.as_bytes();
        let mut calls = Vec::new();
        let mut search = 0usize;
        while let Some(found) = JS[search..].find("op_") {
            let at = search + found;
            search = at + 3;
            // An identifier boundary, so `_migo_op_x` and `key.op_y` do not count.
            if at > 0
                && (bytes[at - 1].is_ascii_alphanumeric() || matches!(bytes[at - 1], b'_' | b'.'))
            {
                continue;
            }
            let name: String = JS[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !JS[at + name.len()..].starts_with('(') {
                continue;
            }
            calls.push((at, name));
        }
        calls
    }

    fn line_of(offset: usize) -> usize {
        JS[..offset].lines().count()
    }

    #[test]
    fn every_op_in_the_2d_facade_is_ordered() {
        let bodies = function_bodies();
        assert!(
            bodies.len() >= 40,
            "only {} function bodies parsed out of the 2D facade; the scan no \
             longer matches",
            bodies.len()
        );
        let calls = op_calls();
        assert!(
            calls.len() >= 20,
            "only {} op calls found in the 2D facade; the scan no longer matches",
            calls.len()
        );

        let mut unordered = Vec::new();
        for (at, name) in &calls {
            let enclosing = bodies
                .iter()
                .filter(|(start, end)| start < at && at < end)
                .max_by_key(|(start, _)| *start);
            let Some((start, _)) = enclosing else {
                unordered.push(format!(
                    "  02_2d_context.js:{}  {name}() is not inside any function body",
                    line_of(*at)
                ));
                continue;
            };
            let before = &JS[*start..*at];
            let barriered =
                before.contains("_barrier()") || before.contains("flushRenderCommandStream()");
            if !barriered {
                unordered.push(format!("  02_2d_context.js:{}  {name}()", line_of(*at)));
            }
        }

        assert!(
            unordered.is_empty(),
            "these ops reach the collector with records still unsubmitted in the \
             stream buffer, so they overtake the commands issued before them:\n{}",
            unordered.join("\n")
        );
    }
}

/// The two named-colour tables are one table.
///
/// The encoder answers named colours itself, and it may only do that while its
/// table says exactly what the Rust one says. A name in one table and not the
/// other is not a crash: the encoder abstains, the Rust parser reads it as
/// black, and the shape is painted the wrong colour with nothing logged.
///
/// The corpus case next door catches this only for names someone thought to
/// write down. This compares the tables themselves, so a name added to either
/// side is checked whether or not anyone remembers it -- which is how
/// `transparent` was found: the JavaScript table had it and the Rust one did
/// not, so `fillStyle = "transparent"` painted opaque black.
mod canvas2d_colour_table {
    use crate::rendering::webgl::context2d::NAMED_COLORS;

    const JS: &str = include_str!("02_2d_context.js");

    /// `'name': [r, g, b, a]`, as the JavaScript table declares them.
    fn javascript_table() -> Vec<(String, [u8; 4])> {
        let mut found = Vec::new();
        for entry in JS.split('\'').skip(1).collect::<Vec<_>>().chunks(2) {
            let [name, rest] = entry else { continue };
            if !name.chars().all(|c| c.is_ascii_lowercase()) || name.is_empty() {
                continue;
            }
            let Some(open) = rest.find('[') else { continue };
            if !rest[..open].trim().starts_with(':') {
                continue;
            }
            let Some(close) = rest[open..].find(']') else {
                continue;
            };
            let channels: Vec<u8> = rest[open + 1..open + close]
                .split(',')
                .filter_map(|value| value.trim().parse().ok())
                .collect();
            if let Ok(channels) = <[u8; 4]>::try_from(channels.as_slice()) {
                found.push((name.to_string(), channels));
            }
        }
        found
    }

    #[test]
    fn the_named_colours_agree_name_for_name() {
        let javascript = javascript_table();
        assert!(
            javascript.len() >= 140,
            "only {} named colours parsed out of the JavaScript; the pattern no \
             longer matches",
            javascript.len()
        );
        assert_eq!(
            javascript.len(),
            NAMED_COLORS.len(),
            "the JavaScript table has {} names and the Rust table {}",
            javascript.len(),
            NAMED_COLORS.len()
        );

        for (name, channels) in &javascript {
            let rust = NAMED_COLORS
                .get(name.as_str())
                .unwrap_or_else(|| panic!("the Rust table has no {name}"));
            let js = shared::protocol::color::Color::rgbai(
                channels[0],
                channels[1],
                channels[2],
                channels[3],
            );
            assert_eq!(
                (rust.r, rust.g, rust.b, rust.a),
                (js.r, js.g, js.b, js.a),
                "{name} is {js:?} in JavaScript and {rust:?} in Rust"
            );
        }
    }
}
