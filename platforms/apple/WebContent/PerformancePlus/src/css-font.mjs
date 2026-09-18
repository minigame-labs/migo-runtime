// The CSS `font` shorthand, parsed where the answer is needed.
//
// `ctx.font = "italic bold 16px 'Noto Sans', sans-serif"` returns whether the
// shorthand parsed: the op it stands in for answers `false` and keeps the
// previous font, which is what a browser does with an invalid assignment. On
// this lane the host's answer arrives a frame later and the assignment has to
// be answered now, so the contract gives `op_set_font` a **local answer** -- and
// a local answer means a parser here.
//
// THIS IS A PORT, NOT A SECOND OPINION. `shared::css_font_shorthand` is the
// parser the host runs, and `scripts/test-css-font-agreement.sh` puts a corpus
// through both and requires the same verdict and the same parse. A shorthand
// this accepts and the host rejects is a font silently not applied; one this
// rejects and the host accepts is a `ctx.font =` that content sees fail while
// the frame draws it. Both are invisible until a screenshot.
//
// The subset is the one Canvas 2D needs and the Rust file documents: a required
// size with px/pt/em/rem/% (resolved against 16px), a family list, weight and
// style keywords, and variant/stretch/line-height accepted and ignored.

const DEFAULT_ROOT_PX = 16;

/** Whitespace, as the Rust parser counts it: the four ASCII ones. */
function isSpace(code) {
  return code === 0x20 || code === 0x09 || code === 0x0a || code === 0x0d;
}

function isDigit(code) {
  return code >= 0x30 && code <= 0x39;
}

function skipWhitespace(text, index) {
  let at = index;
  while (at < text.length && isSpace(text.charCodeAt(at))) at += 1;
  return at;
}

/**
 * One whitespace-delimited token, quotes respected, stopping at a comma so a
 * quoted family with spaces reaches the family-list parser whole.
 */
function nextTokenEnd(text, start) {
  let at = start;
  const first = text.charCodeAt(at);
  if (at < text.length && (first === 0x22 || first === 0x27)) {
    at += 1;
    while (at < text.length && text.charCodeAt(at) !== first) at += 1;
    if (at < text.length) at += 1;
    return at;
  }
  while (at < text.length) {
    const code = text.charCodeAt(at);
    if (isSpace(code) || code === 0x2c) break;
    at += 1;
  }
  return at;
}

/** Does the token end in a CSS length unit? The size/weight disambiguator. */
function hasLengthUnit(token) {
  const lower = token.toLowerCase().split("/")[0];
  return (
    lower.endsWith("px") ||
    lower.endsWith("pt") ||
    lower.endsWith("em") ||
    lower.endsWith("rem") ||
    lower.endsWith("%")
  );
}

/** Any later token with a unit, stopping at the family list. */
function anyLaterTokenHasLengthUnit(tail) {
  let at = 0;
  while (at < tail.length) {
    if (tail.charCodeAt(at) === 0x2c) break;
    at = skipWhitespace(tail, at);
    if (at >= tail.length || tail.charCodeAt(at) === 0x2c) break;
    const end = nextTokenEnd(tail, at);
    const token = tail.slice(at, end);
    if (/[0-9]/.test(token) && hasLengthUnit(token)) return true;
    at = end;
  }
  return false;
}

/** The numeric prefix and the unit suffix, or null when there is no number. */
function splitLength(text) {
  let numberEnd = 0;
  for (let at = 0; at < text.length; at += 1) {
    const ch = text[at];
    if (ch === "+" || ch === "-" || ch === "." || isDigit(text.charCodeAt(at))) {
      numberEnd = at + 1;
    } else {
      break;
    }
  }
  if (numberEnd === 0) return null;
  return [text.slice(0, numberEnd), text.slice(numberEnd).trim()];
}

/**
 * A CSS length in pixels. `pt`, `em`, `rem` and `%` resolve against the CSS
 * default root size; a bare number is pixels, which some game code relies on.
 */
function parseSize(token) {
  const sizePart = token.split("/")[0];
  const split = splitLength(sizePart);
  if (split === null) return null;
  const [numberText, unit] = split;
  // Rust's `f32::from_str`: a trailing `+`/`-`/`.` alone is not a number, and
  // `Number("")` is 0, which would pass a size nothing wrote.
  if (!/^[+-]?(\d+\.?\d*|\.\d+)$/.test(numberText)) return null;
  const value = Number(numberText);
  if (!Number.isFinite(value)) return null;
  let px;
  switch (unit.toLowerCase()) {
    case "px":
      px = value;
      break;
    case "pt":
      px = (value * 96) / 72;
      break;
    case "em":
    case "rem":
      px = value * DEFAULT_ROOT_PX;
      break;
    case "%":
      px = (value * DEFAULT_ROOT_PX) / 100;
      break;
    case "":
      px = value;
      break;
    default:
      return null;
  }
  if (!Number.isFinite(px) || px <= 0) return null;
  return px;
}

function parseFamilyList(text) {
  const families = [];
  for (const raw of text.split(",")) {
    const trimmed = raw.trim();
    if (trimmed.length === 0) continue;
    let family = trimmed;
    const first = trimmed[0];
    if ((first === '"' || first === "'") && trimmed.endsWith(first) && trimmed.length >= 2) {
      family = trimmed.slice(1, -1);
    }
    if (family.length > 0) families.push(family);
  }
  return families;
}

/** The keywords that are not a size: weight, style, and the tolerated rest. */
const IGNORED_KEYWORDS = new Set([
  "normal",
  "ultra-condensed",
  "extra-condensed",
  "condensed",
  "semi-condensed",
  "semi-expanded",
  "expanded",
  "extra-expanded",
  "ultra-expanded",
  "small-caps",
  "all-small-caps",
  "petite-caps",
  "all-petite-caps",
  "unicase",
  "titling-caps",
]);

/**
 * Parse a CSS `font` shorthand.
 *
 * `null` when it is syntactically invalid -- which for this grammar means
 * "no parseable size" -- and the caller keeps the previous font, as a browser
 * does. Otherwise `{ sizePx, weight, italic, families }`.
 */
export function parseFontShorthand(input) {
  const text = String(input).trim();
  if (text.length === 0) return null;

  let cursor = 0;
  let weight = 400;
  let italic = false;
  let sizePx = null;

  while (cursor < text.length) {
    cursor = skipWhitespace(text, cursor);
    if (cursor >= text.length) break;
    const tokenEnd = nextTokenEnd(text, cursor);
    const token = text.slice(cursor, tokenEnd);

    if (/[0-9]/.test(token)) {
      if (!hasLengthUnit(token) && anyLaterTokenHasLengthUnit(text.slice(tokenEnd))) {
        // A bare number followed by a real size is a numeric weight.
        // Rust's `u16::from_str`, which accepts a leading `+` and refuses
        // anything else that is not digits.
        const numeric = /^\+?\d+$/.test(token) ? Number(token) : Number.NaN;
        if (Number.isInteger(numeric) && numeric >= 1 && numeric <= 1000) weight = numeric;
        cursor = tokenEnd;
        continue;
      }
      const px = parseSize(token);
      if (px === null) return null;
      sizePx = px;
      cursor = skipWhitespace(text, tokenEnd);
      // An optional `/line-height`, which Skia does not use: `16px /1.5`, or
      // `16px/1.5` which the size token already swallowed.
      if (cursor < text.length && text[cursor] === "/") {
        cursor = skipWhitespace(text, cursor + 1);
        cursor = skipWhitespace(text, nextTokenEnd(text, cursor));
      }
      break;
    }

    if (token === "italic" || token === "oblique") italic = true;
    else if (token === "bold") weight = 700;
    else if (token === "bolder") weight = Math.min(1000, weight + 100);
    else if (token === "lighter") weight = Math.max(100, weight - 100);
    else if (!IGNORED_KEYWORDS.has(token)) {
      // An unknown keyword is ignored rather than fatal, so a CSS addition does
      // not silently break `ctx.font`.
    }
    cursor = tokenEnd;
  }

  if (sizePx === null) return null;

  const rest = text.slice(cursor).trim();
  const families = rest.length === 0 ? ["sans-serif"] : parseFamilyList(rest);
  if (families.length === 0) return null;

  return { sizePx, weight, italic, families };
}
