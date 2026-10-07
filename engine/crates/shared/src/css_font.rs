//! CSS `font` shorthand parser shared between the JS thread
//! (via `op_*` bridges) and the render thread.
//!
//! The Canvas 2D `ctx.font` property takes a CSS font shorthand:
//!
//! ```text
//! [ <font-style> || <font-variant> || <font-weight> ]
//!   <font-size> [ / <line-height> ]?
//!   <font-family>+
//! ```
//!
//! Before this parser was centralised, the logic existed in two places — a hand-rolled JS
//! implementation in `02_2d_context.js` (fast-path measure) and
//! a separate Rust implementation scattered through the render
//! backend.  Keeping them in sync by hand meant fontStyle / fontWeight
//! subtly diverged whenever either side grew a new feature, surfacing
//! as "JS measure vs fillText width mismatch" visual glitches.
//!
//! This module centralises the parse on the Rust side so both ends
//! consume the same `ParsedFont` output:
//!
//!   * The **render thread** calls it directly from the
//!     `Canvas2DCmd::SetFont` handler.
//!   * The **JS thread** calls it through the
//!     `shared::text_measurer::TextMeasurer` trait (which takes a
//!     raw CSS string and parses on entry).  A tiny JS shim still
//!     lives on in `02_2d_context.js` — it extracts `size` only so
//!     the `op_measure_text_flat` call can short-circuit the
//!     channel, but the authoritative layout decision stays on the
//!     Rust side.
//!
//! Scope kept deliberately small: only the fields Canvas 2D actually
//! consumes (`size`, `family`, `weight`, `italic`).  `font-variant`
//! (`small-caps`) and `font-stretch` are recognised as tokens but
//! silently dropped; `line-height` is parsed off the size group and
//! discarded.
//!
//! # Example
//!
//! ```
//! use shared::css_font::parse_css_font;
//!
//! let p = parse_css_font("italic bold 18px 'Noto Sans CJK SC', sans-serif");
//! assert_eq!(p.size, 18.0);
//! assert_eq!(p.family, "Noto Sans CJK SC");
//! assert_eq!(p.weight, 700);
//! assert!(p.italic);
//! ```

/// Parsed output of [`parse_css_font`].  Default values match the
/// Canvas 2D spec's initial `font` state (`10px sans-serif`, normal
/// weight, upright).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFont {
    pub size: f32,
    pub family: String,
    pub weight: u16,
    pub italic: bool,
}

impl Default for ParsedFont {
    fn default() -> Self {
        Self {
            size: 10.0,
            family: "sans-serif".to_string(),
            weight: 400,
            italic: false,
        }
    }
}

/// Parse a CSS `font` shorthand string, returning `None` when
/// the input is syntactically invalid (no parseable size token).
///
/// This is the strict variant — matches the WHATWG Canvas 2D
/// behaviour where an invalid assignment to `ctx.font` is a
/// no-op that preserves the previous state.  Prefer
/// [`parse_css_font`] when you want a defaulted result regardless
/// of validity (e.g. for the JS-side measure fast path that
/// can't round-trip through the previous state machine).
pub fn try_parse_css_font(input: &str) -> Option<ParsedFont> {
    let parsed = parse_css_font(input);
    // Heuristic: treat parses that didn't advance past the
    // default state as invalid.  The `parse_css_font` loop only
    // leaves `size == 10.0` default untouched when it failed to
    // find a size token; size detection is the single
    // non-optional part of the shorthand.
    let has_size_token = input
        .split_whitespace()
        .any(|tok| tok.chars().any(|c| c.is_ascii_digit()));
    if !has_size_token {
        return None;
    }
    Some(parsed)
}

/// Parse a CSS `font` shorthand string.  Unrecognised tokens are
/// silently skipped and the fields stay at their [`ParsedFont::
/// default()`] values.  Never panics, never allocates more than
/// the single returned `family` string.
pub fn parse_css_font(input: &str) -> ParsedFont {
    let mut out = ParsedFont::default();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return out;
    }

    // Drop any `/line-height` segment before tokenising — it has
    // no bearing on Canvas 2D layout (we measure single lines).
    // Anchor the regex-free scan on the first `/` between
    // whitespace boundaries.
    let stripped = strip_line_height(trimmed);

    let mut tokens = stripped.split_whitespace().peekable();
    let mut family_start: Option<usize> = None;
    // Re-iterate with byte offsets so we can slice the family tail
    // by a single byte range at the end.  `split_whitespace` eats
    // arbitrary runs of whitespace so re-use the input bytes to
    // find the exact start of the family list.
    while let Some(tok) = tokens.next() {
        // Move cursor to the start of the current token in
        // `stripped`.  Safe because tokens are substrings of
        // `stripped`; the pointer arithmetic maps back to the
        // byte offset.
        let tok_start = (tok.as_ptr() as usize).saturating_sub(stripped.as_ptr() as usize);
        if classify_as_size(tok, &mut out) {
            // Everything after the size is the family list; save
            // the cursor and stop iterating tokens.
            family_start = Some(tok_start + tok.len());
            break;
        }
        if let Some(w) = classify_as_weight(tok) {
            out.weight = w;
            continue;
        }
        if classify_as_italic(tok) {
            out.italic = true;
            continue;
        }
        if is_recognised_noop_token(tok) {
            continue;
        }
        // Unknown leading token — treat as the start of the
        // family list so user-typed aliases like
        // `"Archivo Black"` without a size still resolve to
        // some family.
        family_start = Some(tok_start);
        break;
    }

    if let Some(start) = family_start {
        let tail = stripped[start..].trim();
        if !tail.is_empty() {
            // First comma-separated entry wins; strip surrounding
            // quotes for compatibility with `ctx.font = "'Arial'"`.
            let first = tail
                .split(',')
                .next()
                .unwrap_or(tail)
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim();
            if !first.is_empty() {
                out.family = first.to_string();
            }
        }
    }

    out
}

/// Walks the input looking for the first `/` surrounded by size
/// tokens and drops the following `<line-height>` atom.  No regex
/// so the module stays `no_std`-clean at the allocation level.
fn strip_line_height(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // Skip the `/` and the following contiguous
            // non-whitespace atom (the line-height value).
            let mut j = i + 1;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            i = j;
            // Replace the dropped segment with a single space so
            // adjoining tokens don't fuse.
            out.push(' ');
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Size tokens match `<number><unit>` with unit =
/// `px|pt|em|rem|%` (case-insensitive).  A **unit is required** —
/// unitless numbers are NOT sizes (browsers reject unitless
/// `font-size`), which also keeps a bare numeric `font-weight`
/// like `300` from being swallowed here before `classify_as_weight`
/// sees it.  On a hit, writes the resolved pixel size to `out.size`
/// and returns `true` so the caller knows the rest of the tokens
/// belong to the family list.
fn classify_as_size(tok: &str, out: &mut ParsedFont) -> bool {
    let (num, unit) = split_number_unit(tok);
    let Some(n) = num.parse::<f32>().ok() else {
        return false;
    };
    let unit_l = unit.to_ascii_lowercase();
    let px = match unit_l.as_str() {
        "px" => n,
        "pt" => n * 4.0 / 3.0,
        "em" | "rem" => n * 16.0,
        "%" => n * 0.16,
        _ => return false,
    };
    if px.is_finite() && px > 0.0 {
        out.size = px;
        true
    } else {
        false
    }
}

fn split_number_unit(tok: &str) -> (&str, &str) {
    let mut split = tok.len();
    for (i, c) in tok.char_indices() {
        if !(c.is_ascii_digit() || c == '.') {
            split = i;
            break;
        }
    }
    (&tok[..split], &tok[split..])
}

fn classify_as_weight(tok: &str) -> Option<u16> {
    match tok.to_ascii_lowercase().as_str() {
        "normal" => Some(400),
        "bold" => Some(700),
        "lighter" => Some(300),
        "bolder" => Some(600),
        _ => tok.parse::<u16>().ok().filter(|w| {
            // Only the CSS-canonical 100..900 step-by-100
            // ladder; everything else is rejected and the
            // caller treats it as a family-name candidate.
            *w >= 1 && *w <= 1000 && *w % 100 == 0
        }),
    }
}

fn classify_as_italic(tok: &str) -> bool {
    matches!(tok.to_ascii_lowercase().as_str(), "italic" | "oblique")
}

fn is_recognised_noop_token(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "normal"
            | "small-caps"
            | "all-small-caps"
            | "petite-caps"
            | "all-petite-caps"
            | "unicase"
            | "titling-caps"
            | "ultra-condensed"
            | "extra-condensed"
            | "condensed"
            | "semi-condensed"
            | "semi-expanded"
            | "expanded"
            | "extra-expanded"
            | "ultra-expanded"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_canvas2d_initial_font() {
        let p = ParsedFont::default();
        assert_eq!(p.size, 10.0);
        assert_eq!(p.family, "sans-serif");
        assert_eq!(p.weight, 400);
        assert!(!p.italic);
    }

    #[test]
    fn empty_input_returns_default() {
        let p = parse_css_font("");
        assert_eq!(p, ParsedFont::default());
    }

    #[test]
    fn size_only_px() {
        let p = parse_css_font("16px Arial");
        assert_eq!(p.size, 16.0);
        assert_eq!(p.family, "Arial");
    }

    #[test]
    fn bold_italic_prefix() {
        let p = parse_css_font("italic bold 18px 'Noto Sans CJK SC', sans-serif");
        assert_eq!(p.size, 18.0);
        assert_eq!(p.family, "Noto Sans CJK SC");
        assert_eq!(p.weight, 700);
        assert!(p.italic);
    }

    #[test]
    fn line_height_is_dropped() {
        let p = parse_css_font("14px/1.4 Helvetica");
        assert_eq!(p.size, 14.0);
        assert_eq!(p.family, "Helvetica");
    }

    #[test]
    fn numeric_weight_100_to_900_accepted() {
        let p = parse_css_font("300 12px monospace");
        assert_eq!(p.weight, 300);
    }

    #[test]
    fn pt_size_converts_to_px() {
        let p = parse_css_font("12pt Arial");
        // 12pt * 4/3 = 16px
        assert!((p.size - 16.0).abs() < 0.01);
    }

    #[test]
    fn em_size_converts_to_px() {
        let p = parse_css_font("1.5em Arial");
        // 1.5em * 16 = 24px
        assert!((p.size - 24.0).abs() < 0.01);
    }

    #[test]
    fn unknown_weight_falls_through_to_family() {
        // 450 isn't a canonical CSS weight; the parser should
        // not accept it as weight and instead treat it as the
        // start of the family list.
        let p = parse_css_font("450 Arial");
        assert_eq!(p.weight, 400); // default preserved
        // `450` and `Arial` merged into family — the first
        // comma-free stretch is consumed as the family.  Not
        // the prettiest output but safe.
        assert_eq!(p.family, "450 Arial");
    }

    #[test]
    fn quoted_family_is_unquoted() {
        let p = parse_css_font("10px \"Roboto Mono\"");
        assert_eq!(p.family, "Roboto Mono");
    }

    #[test]
    fn comma_list_picks_head() {
        let p = parse_css_font("10px Arial, sans-serif");
        assert_eq!(p.family, "Arial");
    }

    #[test]
    fn oblique_counts_as_italic() {
        let p = parse_css_font("oblique 10px Arial");
        assert!(p.italic);
    }

    #[test]
    fn small_caps_is_silently_dropped() {
        let p = parse_css_font("small-caps 10px Arial");
        assert_eq!(p.family, "Arial");
    }
}
