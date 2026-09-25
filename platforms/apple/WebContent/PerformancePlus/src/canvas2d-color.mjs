// A CSS colour string, as the host reads it.
//
// The engine's own Canvas2D code parses the forms it is sure of -- hex, strict
// `rgb()`/`rgba()`, and every name in its table -- and encodes those itself. It
// abstains from the rest and calls `op_set_fill_style` with the string, and the
// op's Rust body (`context2d.rs::parse_color_string`) is then the authority on
// what it means. That includes colours a game really writes: `rgb( 1 , 2 , 3 )`
// has spaces the engine's strict reader will not guess at, and Rust trims each
// channel, so it is a real colour that only this path can set.
//
// So this is that authority, restated: the same branches in the same order,
// with the same fallbacks -- an unparseable channel is zero, an unparseable
// colour is black. `test/canvas2d-color.test.mjs` requires the same answer as
// the Rust parser for every string in a corpus it generates.
//
// NAMES ARE NOT HERE, deliberately. A name the engine's table knows never
// reaches this op, and a name it does not know is black in both tables --
// `the_named_colours_agree_name_for_name` is what keeps the two tables
// identical, so a miss here is a miss there. A third copy of 148 names would be
// a third thing to keep in step for no answer it could give.

const BLACK = [0, 0, 0, 255];

/** One hex digit, as `u8_from_hex_char` reads it: anything else is zero. */
function hexDigit(code) {
  if (code >= 48 && code <= 57) return code - 48; // 0-9
  if (code >= 97 && code <= 102) return code - 97 + 10; // a-f
  if (code >= 65 && code <= 70) return code - 65 + 10; // A-F
  return 0;
}

const hexPair = (text, at) => (hexDigit(text.charCodeAt(at)) << 4) | hexDigit(text.charCodeAt(at + 1));

/** `Color::hex`: 3, 4, 6 or 8 digits, and any other length is black. */
function fromHex(text) {
  const hex = text.startsWith("#") ? text.slice(1) : text;
  const digit = (at) => {
    const value = hexDigit(hex.charCodeAt(at));
    return (value << 4) | value;
  };
  switch (hex.length) {
    case 3:
      return [digit(0), digit(1), digit(2), 255];
    case 4:
      return [digit(0), digit(1), digit(2), digit(3)];
    case 6:
      return [hexPair(hex, 0), hexPair(hex, 2), hexPair(hex, 4), 255];
    case 8:
      return [hexPair(hex, 0), hexPair(hex, 2), hexPair(hex, 4), hexPair(hex, 6)];
    default:
      return BLACK;
  }
}

/** `str::parse::<u8>`: an optional `+`, decimal digits, and in range. */
function channel(text) {
  let at = 0;
  if (text.charCodeAt(0) === 43) at = 1; // '+'
  if (at === text.length) return 0;
  let value = 0;
  for (; at < text.length; at += 1) {
    const code = text.charCodeAt(at);
    if (code < 48 || code > 57) return 0;
    value = value * 10 + (code - 48);
    if (value > 255) return 0;
  }
  return value;
}

/**
 * The alpha channel: `str::parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0)`, then
 * `* 255.0` and `as u8`.
 *
 * Two things here are the arithmetic's, not the colour's. The parse is `f32`
 * and so is the multiply, so the value is narrowed twice before it truncates --
 * an `f64` multiply lands on the other side of an integer for literals like
 * `0.02745098` and answers one lower. And a float that parses as NaN is not a
 * parse failure: it clamps to NaN and `as u8` saturates it to zero, where a
 * string Rust cannot parse at all falls back to 1.0 and is opaque.
 */
function alphaChannel(text) {
  const parsed = rustFloat(text.trim());
  const alpha = parsed === null ? 1 : Math.fround(parsed);
  if (Number.isNaN(alpha)) return 0;
  const clamped = Math.min(Math.max(alpha, 0), 1);
  return Math.trunc(Math.fround(Math.fround(clamped) * 255));
}

/**
 * A float exactly as Rust's `f32::from_str` takes it, or null where it fails.
 *
 * Rust's grammar is narrower than `Number`'s in the forms CSS never writes --
 * no hexadecimal, no separators, no empty string -- and wider in two it does
 * not either: `inf` and `nan` without their JavaScript spelling.
 */
function rustFloat(text) {
  const special = /^([+-]?)(inf(inity)?|nan)$/i.exec(text);
  if (special !== null) {
    if (special[2].toLowerCase() === "nan") return Number.NaN;
    return special[1] === "-" ? -Infinity : Infinity;
  }
  if (!/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(text)) return null;
  return Number(text);
}

/** The parts of `rgb(...)`, trimmed, or null when there are not that many. */
function parts(inner, wanted) {
  const found = inner.split(",");
  // Rust's fixed four-slot split signals overflow by counting past it, which
  // reads as "not the number wanted" for every caller here.
  if (found.length !== wanted) return null;
  return found.map((part) => part.trim());
}

/**
 * What the host will make of `text`, as `[r, g, b, a]` bytes.
 *
 * Every branch has a total answer: this is a setter with no way to report a
 * failure, and the platform's answer for a colour it cannot read is black.
 */
export function parseColorString(text) {
  const trimmed = text.trim();

  if (trimmed.startsWith("#")) return fromHex(trimmed);

  const lower = trimmed.toLowerCase();
  if (lower.startsWith("rgba(") && trimmed.endsWith(")")) {
    const found = parts(trimmed.slice(5, -1), 4);
    if (found === null) return BLACK;
    return [channel(found[0]), channel(found[1]), channel(found[2]), alphaChannel(found[3])];
  }
  if (lower.startsWith("rgb(") && trimmed.endsWith(")")) {
    const found = parts(trimmed.slice(4, -1), 3);
    if (found === null) return BLACK;
    return [channel(found[0]), channel(found[1]), channel(found[2]), 255];
  }

  // A name, which is black here for the reason the module note gives -- and so
  // is anything past the 24 bytes the Rust parser will look one up in.
  return BLACK;
}

// One scratch view, reused: a colour is set as often as a shape is drawn.
const FLOAT = new Float32Array(4);
const BITS = new Uint32Array(FLOAT.buffer);

/**
 * The colour as the record carries it: four `f32`s in 0..1, as their bits.
 *
 * Bits rather than Numbers because that is what a record word is, and the
 * channels are derived here rather than converted from an op argument -- the op
 * takes the string, and this is what the host would have made of it.
 */
export function colorRecordWords(text) {
  const rgba = parseColorString(text);
  for (let index = 0; index < 4; index += 1) FLOAT[index] = rgba[index] / 255;
  return BITS;
}
