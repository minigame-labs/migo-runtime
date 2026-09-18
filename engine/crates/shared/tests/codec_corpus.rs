//! The codec corpus, through the implementation the host runs.
//!
//! The producer converts text too -- `encodeMultiFormats` is synchronous and
//! takes no host resource, so the op boundary answers it locally -- and the two
//! agreeing is not something either can check. This prints one line per case;
//! `scripts/test-text-codec-agreement.sh` diffs it against the producer's.
//!
//! `#[ignore]` because it writes a file for a shell script to compare.

use std::fs;

use shared::codec::{decode_bytes, encode_string};

#[test]
#[ignore = "prints the corpus for scripts/test-text-codec-agreement.sh"]
fn print_the_corpus_answers() {
    let corpus = std::env::var("MIGO_CODEC_CORPUS").expect("MIGO_CODEC_CORPUS names the corpus");
    let output = std::env::var("MIGO_CODEC_OUT").expect("MIGO_CODEC_OUT names the file to write");
    let text = fs::read_to_string(&corpus).expect("the corpus file");

    let mut lines = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let direction = fields.next().unwrap_or_default();
        // The op's own normalisation, which is the behaviour the producer
        // implements: `op_encode_multi_formats` maps an empty coding to utf8
        // before the codec sees it, so a corpus of the op's answers has to.
        let encoding = match fields.next().unwrap_or_default() {
            "" => "utf8",
            named => named,
        };
        let value = fields.next().unwrap_or_default();
        let answer = if direction == "enc" {
            let input = json_unescape(value);
            match encode_string(&input, encoding) {
                Ok(bytes) => format!("ok\t{}", hex(&bytes)),
                Err(message) => format!("refused\t{message}"),
            }
        } else {
            let bytes = unhex(value);
            match decode_bytes(&bytes, encoding) {
                Ok(string) => format!("ok\t{}", json_string(&string)),
                Err(message) => format!("refused\t{message}"),
            }
        };
        lines.push(format!("{line}\t{answer}"));
    }
    let count = lines.len();
    fs::write(&output, format!("{}\n", lines.join("\n"))).expect("writing the answers");
    println!("wrote {count} codec answers to {output}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("ascii"), 16).expect("hex"))
        .collect()
}

/// `JSON.parse` for the corpus's quoted strings: enough of it for `\uXXXX`,
/// which is how the corpus writes anything outside ASCII.
fn json_unescape(value: &str) -> String {
    let inner = value.trim_matches('"');
    let mut out = String::new();
    let mut chars = inner.chars();
    let mut pending_high: Option<u16> = None;
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            if let Some(high) = pending_high.take() {
                out.push(char::REPLACEMENT_CHARACTER);
                let _ = high;
            }
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let digits: String = (&mut chars).take(4).collect();
                let unit = u16::from_str_radix(&digits, 16).expect("a \\u escape");
                match pending_high.take() {
                    Some(high) => {
                        let combined = String::from_utf16(&[high, unit]).expect("a surrogate pair");
                        out.push_str(&combined);
                    }
                    None if (0xD800..0xDC00).contains(&unit) => pending_high = Some(unit),
                    None => out.push(char::from_u32(u32::from(unit)).expect("a code point")),
                }
            }
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// `JSON.stringify` for a string, which is what the producer's half prints.
fn json_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
