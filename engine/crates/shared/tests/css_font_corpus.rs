//! The `ctx.font` corpus, through the parser the host runs.
//!
//! The producer parses the same shorthand -- it has to, because `ctx.font =`
//! answers whether it parsed and the host's answer is a frame away -- and the
//! two parsers agreeing is not something either one can check. This prints one
//! line per corpus entry; `scripts/test-css-font-agreement.sh` diffs it against
//! the producer's and fails on any difference.
//!
//! `#[ignore]` because it writes a file for a shell script to compare, which is
//! not what a `cargo test` run is for. The gate is what guarantees it runs.

use std::fs;

use shared::css_font_shorthand::parse_font_shorthand;

#[test]
#[ignore = "prints the corpus for scripts/test-css-font-agreement.sh"]
fn print_the_corpus_verdicts() {
    let corpus = std::env::var("MIGO_CSS_FONT_CORPUS")
        .expect("MIGO_CSS_FONT_CORPUS must name the shared corpus file");
    let output =
        std::env::var("MIGO_CSS_FONT_OUT").expect("MIGO_CSS_FONT_OUT must name the file to write");
    let text = fs::read_to_string(&corpus).expect("the corpus file");

    let mut lines = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // JSON's escaping for the input, so a line with a quote or a tab in it
        // is one field on both sides.
        let quoted = json_string(line);
        match parse_font_shorthand(line) {
            None => lines.push(format!("{quoted}\trefused")),
            Some(font) => lines.push(format!(
                "{quoted}\t{:.3}\t{}\t{}\t{}",
                font.size_px,
                font.weight,
                font.italic,
                font.families.join("|")
            )),
        }
    }
    let count = lines.len();
    fs::write(&output, format!("{}\n", lines.join("\n"))).expect("writing the verdicts");
    println!("wrote {count} font verdicts to {output}");
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
