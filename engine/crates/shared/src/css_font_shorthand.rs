//! Tiny CSS `font` shorthand parser tuned for Canvas 2D.
//!
//! WHATWG Canvas 2D defines `CanvasRenderingContext2D.font` as "a CSS
//! `<'font'>` value", with the grammar reduced from the full CSS
//! shorthand: `<font-style> <font-variant> <font-weight> <font-stretch>
//! <font-size> [/ <line-height>]? <font-family>`.  Only the size and
//! family are required; others may be in any order before the size.
//!
//! For the embedded game runtime we implement a pragmatic subset that
//! covers 99% of real small-game inputs:
//!
//!   * `font-size`: `<number>(px|pt|em|rem|%)` — required; `pt`/`em`/
//!     `rem`/`%` are resolved against a CSS default of 16px so the
//!     result is always a pixel-space float.
//!   * `font-family`: comma-separated list of families, each either a
//!     bare identifier run (`sans-serif`) or a double/single-quoted
//!     string (`"Noto Sans CJK SC"`).  Required — if the parse reaches
//!     the end without a family list we fall back to `sans-serif`.
//!   * `font-weight`: `normal | bold | lighter | bolder | 100..=1000`.
//!   * `font-style`: `normal | italic | oblique[ <angle>]?` (we map
//!     `oblique` to `italic` because Skia's `Slant` is binary).
//!   * `font-variant` / `font-stretch` / line-height: accepted and
//!     ignored — tokens are skipped so they don't derail the parse.
//!
//! The parser is strict about size; if size parsing fails we return
//! `None` and the caller keeps the previous font (this matches browser
//! behaviour: a syntactically invalid `ctx.font` assignment is a no-op).

/// Parsed result of a `font` shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFont {
    /// Resolved font size in CSS pixels (same unit Skia expects).
    pub size_px: f32,
    /// CSS font-weight `1..=1000`; `400` = normal, `700` = bold.
    pub weight: u16,
    /// `true` for `italic` or `oblique`.
    pub italic: bool,
    /// Family list, head-first.  Always contains at least one entry —
    /// the final fallback is synthesised as `sans-serif` if the
    /// shorthand had nothing.
    pub families: Vec<String>,
}

impl ParsedFont {
    /// Tolerant defaults mirroring Canvas 2D's bootstrap state
    /// (`"10px sans-serif"`).
    pub fn canvas_default() -> Self {
        Self {
            size_px: 10.0,
            weight: 400,
            italic: false,
            families: vec!["sans-serif".to_string()],
        }
    }
}

/// Parse a CSS `font` shorthand.  Returns `None` if the input is
/// syntactically invalid (no parseable size).  Callers should keep the
/// previous `font` on `None` to mirror browser behaviour.
///
/// A second CSS-font parser lives in
/// [`crate::css_font::parse_css_font`] for the JS-thread
/// `measureText` fast path.  The two parsers are kept in
/// sync via [`tests::shared_and_render_agree_on_canonical_inputs`]
/// which pins the matrix of inputs where the two must produce
/// equivalent outputs.  We kept two implementations rather than
/// one because `shared` cannot depend on `graphics` (where the
/// `Option`-semantics wrapper naturally lives) and this module's
/// parser carries a larger state machine (multi-family list, size
/// validation) that would inflate the `shared` crate beyond its
/// no-GL remit.
pub fn parse_font_shorthand(input: &str) -> Option<ParsedFont> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 1) Split off the family list at the first size token's boundary.
    //    We scan left-to-right collecting non-family tokens until we
    //    hit something that looks like a size (contains a digit and
    //    ends with a CSS length unit or `%`).  Whatever follows is the
    //    family list.
    let mut cursor = 0usize;
    let bytes = trimmed.as_bytes();
    let mut weight: u16 = 400;
    let mut italic = false;
    let mut size_px: Option<f32> = None;

    while cursor < bytes.len() {
        cursor = skip_whitespace(trimmed, cursor);
        if cursor >= bytes.len() {
            break;
        }
        let tok_end = next_token_end(trimmed, cursor);
        let tok = &trimmed[cursor..tok_end];

        // Classification: a token containing a digit is either a size
        // (has a length unit or is the only numeric token left) or a
        // numeric weight (bare 100..=1000 followed by more tokens
        // including an actual size).  We disambiguate with a cheap
        // lookahead: if the remainder contains another token ending
        // in a CSS length unit, the current bare-digit token is a
        // weight; otherwise it's the size.
        if tok.as_bytes().iter().any(|b| b.is_ascii_digit()) {
            let has_length_unit = token_has_length_unit(tok);
            if !has_length_unit {
                // Could be numeric weight or bare-number size — decide
                // by scanning ahead for a unit-bearing token.
                let tail = &trimmed[tok_end..];
                if any_later_token_has_length_unit(tail) {
                    // This is a weight; continue parsing.
                    if let Ok(n) = tok.parse::<u16>() {
                        if (1..=1000).contains(&n) {
                            weight = n;
                        }
                    }
                    cursor = tok_end;
                    continue;
                }
            }
            if let Some(px) = parse_size(tok) {
                size_px = Some(px);
                cursor = tok_end;
                cursor = skip_whitespace(trimmed, cursor);
                // Skip an optional `/line-height` fragment attached
                // after the size token (e.g. `16px /1.5` or
                // `16px/1.5`; the latter is consumed by parse_size).
                if cursor < bytes.len() && bytes[cursor] == b'/' {
                    cursor += 1;
                    cursor = skip_whitespace(trimmed, cursor);
                    cursor = next_token_end(trimmed, cursor);
                    cursor = skip_whitespace(trimmed, cursor);
                }
                break;
            }
            // Digit-containing token that isn't a valid size → abort.
            return None;
        }

        // Non-size token: classify as weight / style / ignorable.
        match tok {
            "normal" => {}
            "italic" | "oblique" => italic = true,
            "bold" => weight = 700,
            "bolder" => weight = weight.saturating_add(100).min(1000),
            "lighter" => weight = weight.saturating_sub(100).max(100),
            // CSS font-stretch / font-variant keywords we tolerate but
            // don't map to Skia state.
            "ultra-condensed" | "extra-condensed" | "condensed" | "semi-condensed"
            | "semi-expanded" | "expanded" | "extra-expanded" | "ultra-expanded" | "small-caps"
            | "all-small-caps" | "petite-caps" | "all-petite-caps" | "unicase" | "titling-caps" => {
            }
            _ => {
                // Unknown keyword — ignore rather than abort so new
                // CSS additions don't silently break `ctx.font`.
            }
        }
        cursor = tok_end;
    }

    let size_px = size_px?;
    if !size_px.is_finite() || size_px <= 0.0 {
        return None;
    }

    // 2) Everything remaining is the comma-separated family list.
    let rest = trimmed[cursor..].trim();
    let families = if rest.is_empty() {
        vec!["sans-serif".to_string()]
    } else {
        parse_family_list(rest)
    };
    if families.is_empty() {
        return None;
    }

    Some(ParsedFont {
        size_px,
        weight,
        italic,
        families,
    })
}

/// Does the token end with a CSS length unit (px / pt / em / rem / %)?
///
/// Used to disambiguate "bare number is weight" vs "bare number is
/// size" without a full lookahead parse.
fn token_has_length_unit(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    let stripped = lower.split('/').next().unwrap_or(lower.as_str());
    stripped.ends_with("px")
        || stripped.ends_with("pt")
        || stripped.ends_with("em")
        || stripped.ends_with("rem")
        || stripped.ends_with('%')
}

/// Scan the remainder of the shorthand and return `true` if any
/// whitespace-delimited token has a CSS length unit.  Used only for
/// the size/weight disambiguation; must stop at comma (family list
/// start) to avoid false positives inside a quoted family.
fn any_later_token_has_length_unit(tail: &str) -> bool {
    let mut i = 0;
    let b = tail.as_bytes();
    while i < b.len() {
        // Stop scanning once we enter the family list.
        if b[i] == b',' {
            break;
        }
        i = skip_whitespace(tail, i);
        if i >= b.len() || b[i] == b',' {
            break;
        }
        let end = next_token_end(tail, i);
        let tok = &tail[i..end];
        if tok.as_bytes().iter().any(|c| c.is_ascii_digit()) && token_has_length_unit(tok) {
            return true;
        }
        i = end;
    }
    false
}

fn skip_whitespace(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\n' || b[i] == b'\r') {
        i += 1;
    }
    i
}

/// Read the next whitespace-delimited token, respecting `"..."` and
/// `'...'` string quoting.  Returns the byte offset one past the
/// token.  Stops at a comma (so quoted families with spaces inside
/// are handled correctly by the family-list parser).
fn next_token_end(s: &str, start: usize) -> usize {
    let b = s.as_bytes();
    let mut i = start;
    if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
        let quote = b[i];
        i += 1;
        while i < b.len() && b[i] != quote {
            i += 1;
        }
        if i < b.len() {
            i += 1; // consume closing quote
        }
        return i;
    }
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == b',' {
            break;
        }
        i += 1;
    }
    i
}

/// Parse a CSS length token into CSS pixels.  Accepts
/// `<number>(px|pt|em|rem|%)`; `em`, `rem` and `%` are resolved
/// against the CSS default root size of 16 px.
fn parse_size(tok: &str) -> Option<f32> {
    // Strip an optional `/line-height` fragment — Skia doesn't use it
    // and we don't want the slash to trip the length parser.
    let size_part = tok.split('/').next().unwrap_or(tok);
    let (num_str, unit) = split_length(size_part)?;
    let n: f32 = num_str.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    let px = match unit.to_ascii_lowercase().as_str() {
        "px" => n,
        "pt" => n * 96.0 / 72.0,
        "em" | "rem" => n * 16.0,
        "%" => n * 16.0 / 100.0,
        // Bare number — treat as px (permissive; some game code omits).
        "" => n,
        _ => return None,
    };
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    Some(px)
}

fn split_length(s: &str) -> Option<(&str, &str)> {
    // Find the split between numeric prefix and unit suffix.  We allow
    // a leading sign and a decimal point but not scientific notation
    // because CSS doesn't either.
    let mut num_end = 0;
    for (i, ch) in s.char_indices() {
        if ch == '+' || ch == '-' || ch == '.' || ch.is_ascii_digit() {
            num_end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    if num_end == 0 {
        return None;
    }
    Some((&s[..num_end], s[num_end..].trim()))
}

/// Parse the comma-separated family list.  Each family may be a bare
/// identifier run or a quoted string.  Whitespace inside bare
/// identifiers (e.g. `Noto Sans CJK SC`) is preserved per CSS.
fn parse_family_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in s.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip matching outer quotes.
        let family = if (trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        {
            if trimmed.len() >= 2 {
                trimmed[1..trimmed.len() - 1].to_string()
            } else {
                continue;
            }
        } else {
            trimmed.to_string()
        };
        if !family.is_empty() {
            out.push(family);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_minimal_canvas_default() {
        let f = parse_font_shorthand("10px sans-serif").unwrap();
        assert_eq!(f.size_px, 10.0);
        assert_eq!(f.weight, 400);
        assert!(!f.italic);
        assert_eq!(f.families, fam(&["sans-serif"]));
    }

    #[test]
    fn parses_size_and_family_only() {
        let f = parse_font_shorthand("16px Arial").unwrap();
        assert_eq!(f.size_px, 16.0);
        assert_eq!(f.families, fam(&["Arial"]));
    }

    #[test]
    fn parses_weight_style_size_family() {
        let f = parse_font_shorthand("italic 700 24px 'Noto Sans CJK SC'").unwrap();
        assert_eq!(f.size_px, 24.0);
        assert_eq!(f.weight, 700);
        assert!(f.italic);
        assert_eq!(f.families, fam(&["Noto Sans CJK SC"]));
    }

    #[test]
    fn parses_bold_keyword() {
        let f = parse_font_shorthand("bold 14px Helvetica").unwrap();
        assert_eq!(f.weight, 700);
    }

    #[test]
    fn parses_comma_separated_family_list() {
        let f = parse_font_shorthand("14px \"Noto Sans\", Arial, sans-serif").unwrap();
        assert_eq!(f.families, fam(&["Noto Sans", "Arial", "sans-serif"]));
    }

    #[test]
    fn parses_pt_converts_to_px() {
        let f = parse_font_shorthand("12pt serif").unwrap();
        // 12pt == 16px at 96dpi.
        assert!((f.size_px - 16.0).abs() < 1e-3);
    }

    #[test]
    fn parses_em_resolves_against_16px_default() {
        let f = parse_font_shorthand("1.5em monospace").unwrap();
        assert!((f.size_px - 24.0).abs() < 1e-3);
    }

    #[test]
    fn parses_percent_resolves_against_16px_default() {
        let f = parse_font_shorthand("75% serif").unwrap();
        assert!((f.size_px - 12.0).abs() < 1e-3);
    }

    #[test]
    fn parses_size_slash_lineheight_ignores_lineheight() {
        // `16px/1.5` must still give 16px; line-height is irrelevant to
        // Canvas 2D (single-line paint only).
        let f = parse_font_shorthand("16px/1.5 Arial").unwrap();
        assert_eq!(f.size_px, 16.0);
        assert_eq!(f.families, fam(&["Arial"]));
    }

    #[test]
    fn parses_oblique_as_italic() {
        let f = parse_font_shorthand("oblique 20px serif").unwrap();
        assert!(f.italic);
    }

    #[test]
    fn parses_numeric_weight() {
        let f = parse_font_shorthand("300 18px Roboto").unwrap();
        assert_eq!(f.weight, 300);
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_font_shorthand("").is_none());
        assert!(parse_font_shorthand("   ").is_none());
    }

    #[test]
    fn rejects_no_size() {
        // No numeric token at all → invalid.
        assert!(parse_font_shorthand("bold serif").is_none());
    }

    #[test]
    fn rejects_zero_or_negative_size() {
        assert!(parse_font_shorthand("0px Arial").is_none());
        assert!(parse_font_shorthand("-5px Arial").is_none());
    }

    #[test]
    fn permits_bare_number_as_px_fallback() {
        // CSS strictly requires a unit, but many engines accept bare
        // numbers; we do too, matching Blink's permissive behaviour.
        let f = parse_font_shorthand("14 Arial").unwrap();
        assert_eq!(f.size_px, 14.0);
    }

    #[test]
    fn tolerates_extra_whitespace() {
        let f = parse_font_shorthand("  italic   bold   22px   Arial  ").unwrap();
        assert_eq!(f.size_px, 22.0);
        assert_eq!(f.weight, 700);
        assert!(f.italic);
        assert_eq!(f.families, fam(&["Arial"]));
    }

    #[test]
    fn tolerates_unknown_keywords_without_aborting() {
        // `small-caps` is font-variant; we skip it gracefully.
        let f = parse_font_shorthand("small-caps 18px serif").unwrap();
        assert_eq!(f.size_px, 18.0);
        assert_eq!(f.families, fam(&["serif"]));
    }

    #[test]
    fn family_list_unquotes_single_and_double_quotes() {
        let f = parse_font_shorthand("12px 'Helvetica Neue', \"Noto Sans\", sans-serif").unwrap();
        assert_eq!(
            f.families,
            fam(&["Helvetica Neue", "Noto Sans", "sans-serif"])
        );
    }

    #[test]
    fn preserves_family_case_sensitivity() {
        // Family names are case-sensitive per CSS; we must not
        // lowercase them.
        let f = parse_font_shorthand("12px \"PingFang SC\"").unwrap();
        assert_eq!(f.families, fam(&["PingFang SC"]));
    }

    /// The render-side parser (`font_parse::parse_font_shorthand`)
    /// and the JS-thread fast path (`shared::css_font::
    /// parse_css_font`) must agree on the matrix of real-world
    /// CSS font shorthands users write in canvas games.  A mismatch
    /// would show up as a "measureText width differs from paint
    /// width for the exact same `ctx.font`" UX regression.  This
    /// test pins the equivalence via canonical inputs.
    #[test]
    fn shared_and_render_agree_on_canonical_inputs() {
        const CASES: &[&str] = &[
            "10px sans-serif",
            "16px Arial",
            "italic 700 24px 'Noto Sans CJK SC'",
            "bold 14px Helvetica",
            "14px \"Noto Sans\", Arial, sans-serif",
            "12pt serif",
            "1.5em monospace",
            "16px/1.5 Arial",
            "oblique 20px serif",
            "300 18px Roboto",
            "small-caps 18px serif",
        ];
        for input in CASES {
            let render = match parse_font_shorthand(input) {
                Some(p) => p,
                None => panic!("render parser rejected canonical input: {input:?}"),
            };
            let shared_out = crate::css_font::parse_css_font(input);
            assert!(
                (render.size_px - shared_out.size).abs() < 0.05,
                "size disagreement on {input:?}: render={} shared={}",
                render.size_px,
                shared_out.size,
            );
            assert_eq!(
                render.weight, shared_out.weight,
                "weight disagreement on {input:?}",
            );
            assert_eq!(
                render.italic, shared_out.italic,
                "italic disagreement on {input:?}",
            );
            // Shared parser only surfaces the head family; render
            // parser retains the full list.  Agreement here is
            // head-only.
            assert_eq!(
                render.families.first().map(|s| s.as_str()),
                Some(shared_out.family.as_str()),
                "family-head disagreement on {input:?}",
            );
        }
    }
}
