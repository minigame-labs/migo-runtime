//! Tagged values: the arguments a service request carries and the answers that
//! come back.
//!
//! A frame record is positional -- `H C U I I` -- because both ends know every
//! opcode's shape and a draw call is on the path this lane exists to shorten.
//! The service stream is the other kind of traffic: a file read, a storage
//! write, an image load, an audio graph edit. None of it is per draw, and each
//! op's arguments are whatever its Rust signature says. So every value says
//! what it is, and a handler that expected a string and was sent a number
//! refuses the call by name instead of reading four bytes of a length as text.
//!
//! See *Service values* in `contracts/frame-wire/wire-v1.md`, which this
//! implements, and `platforms/apple/WebContent/PerformancePlus/src/service-value.mjs`,
//! the producer's half. The two are checked against each other through the
//! interop corpus, not by reading both.
//!
//! # Layout
//!
//! Every value starts with a tag word: the tag in the low eight bits and zero
//! above it. Everything is little-endian and every value ends on a four-byte
//! boundary; a variable-length value is followed by zero padding to one, and
//! the padding must be zero.
//!
//! ```text
//! NULL    0   --
//! FALSE   1   --
//! TRUE    2   --
//! U32     3   u32
//! I32     4   i32
//! F64     5   f64 (8 bytes)
//! U64     6   u64 (8 bytes)
//! I64     7   i64 (8 bytes)
//! STRING  8   byte_length u32, UTF-8, pad
//! BYTES   9   byte_length u32, bytes, pad
//! JSON    10  byte_length u32, UTF-8 JSON text, pad
//! ARRAY   11  count u32, then `count` values
//! ```
//!
//! # What the reader refuses
//!
//! Everything a writer following the layout cannot produce: an unknown tag,
//! bits above the tag, a length past the end, non-zero padding, a string that
//! is not UTF-8, arrays nested deeper than [`MAX_DEPTH`]. The reader validates
//! a whole run before handing any of it out, so a refused message is refused
//! in full and nothing in it was acted on.

use core::fmt;

pub const TAG_NULL: u32 = 0;
pub const TAG_FALSE: u32 = 1;
pub const TAG_TRUE: u32 = 2;
pub const TAG_U32: u32 = 3;
pub const TAG_I32: u32 = 4;
pub const TAG_F64: u32 = 5;
pub const TAG_U64: u32 = 6;
pub const TAG_I64: u32 = 7;
pub const TAG_STRING: u32 = 8;
pub const TAG_BYTES: u32 = 9;
pub const TAG_JSON: u32 = 10;
pub const TAG_ARRAY: u32 = 11;

/// How deeply arrays may nest.
///
/// A bound rather than a stack: the reader recurses, and a producer that could
/// choose the depth could choose to overflow the host's stack. No op's
/// arguments or answer nest more than three deep -- `op_load_image` answers
/// `[id, [width, height]]` -- so sixteen is room, not a limit anyone meets.
pub const MAX_DEPTH: usize = 16;

/// A value read from a run, borrowing the bytes it came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    U32(u32),
    I32(i32),
    F64(f64),
    U64(u64),
    I64(i64),
    Str(&'a str),
    Bytes(&'a [u8]),
    /// JSON text, validated as UTF-8 and not parsed: the handler that wants a
    /// structure parses it into its own type, and one that only forwards it
    /// never pays for a parse.
    Json(&'a str),
    Array(Vec<Value<'a>>),
}

/// The same values, owning their bytes, for a call that outlives the message
/// it arrived in.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedValue {
    Null,
    Bool(bool),
    U32(u32),
    I32(i32),
    F64(f64),
    U64(u64),
    I64(i64),
    Str(String),
    Bytes(Vec<u8>),
    Json(String),
    Array(Vec<OwnedValue>),
}

impl Value<'_> {
    /// Copy what this borrows.
    pub fn to_owned_value(&self) -> OwnedValue {
        match self {
            Value::Null => OwnedValue::Null,
            Value::Bool(value) => OwnedValue::Bool(*value),
            Value::U32(value) => OwnedValue::U32(*value),
            Value::I32(value) => OwnedValue::I32(*value),
            Value::F64(value) => OwnedValue::F64(*value),
            Value::U64(value) => OwnedValue::U64(*value),
            Value::I64(value) => OwnedValue::I64(*value),
            Value::Str(value) => OwnedValue::Str((*value).to_owned()),
            Value::Bytes(value) => OwnedValue::Bytes(value.to_vec()),
            Value::Json(value) => OwnedValue::Json((*value).to_owned()),
            Value::Array(values) => {
                OwnedValue::Array(values.iter().map(Value::to_owned_value).collect())
            }
        }
    }
}

impl OwnedValue {
    /// What the value is, as a handler's refusal names it.
    pub fn kind_name(&self) -> &'static str {
        match self {
            OwnedValue::Null => "null",
            OwnedValue::Bool(_) => "boolean",
            OwnedValue::U32(_) => "u32",
            OwnedValue::I32(_) => "i32",
            OwnedValue::F64(_) => "f64",
            OwnedValue::U64(_) => "u64",
            OwnedValue::I64(_) => "i64",
            OwnedValue::Str(_) => "string",
            OwnedValue::Bytes(_) => "bytes",
            OwnedValue::Json(_) => "json",
            OwnedValue::Array(_) => "array",
        }
    }

    /// Append this value to `writer`.
    pub fn write_to(&self, writer: &mut ValueWriter) {
        match self {
            OwnedValue::Null => writer.null(),
            OwnedValue::Bool(value) => writer.bool(*value),
            OwnedValue::U32(value) => writer.u32(*value),
            OwnedValue::I32(value) => writer.i32(*value),
            OwnedValue::F64(value) => writer.f64(*value),
            OwnedValue::U64(value) => writer.u64(*value),
            OwnedValue::I64(value) => writer.i64(*value),
            OwnedValue::Str(value) => writer.str(value),
            OwnedValue::Bytes(value) => writer.bytes(value),
            OwnedValue::Json(value) => writer.json(value),
            OwnedValue::Array(values) => {
                writer.array(values.len() as u32);
                for value in values {
                    value.write_to(writer);
                }
            }
        }
    }
}

/// Why a run of values was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueError {
    /// The run is not a whole number of words.
    NotWordAligned,
    /// A value, or a length, runs past the end of the run.
    Truncated,
    /// A tag this version does not define, or bits set above the tag.
    UnknownTag(u32),
    /// Padding after a variable-length value is not zero.
    PaddingNotZero,
    /// A `STRING` or `JSON` value is not UTF-8.
    NotUtf8,
    /// Arrays nested deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::NotWordAligned => f.write_str("the values are not a whole number of words"),
            ValueError::Truncated => f.write_str("a value runs past the end of the message"),
            ValueError::UnknownTag(tag) => write!(f, "tag word {tag:#x} is not a value tag"),
            ValueError::PaddingNotZero => f.write_str("padding after a value is not zero"),
            ValueError::NotUtf8 => f.write_str("a string value is not UTF-8"),
            ValueError::TooDeep => write!(f, "arrays nest deeper than {MAX_DEPTH}"),
        }
    }
}

/// Read every value in `bytes`, which must end exactly where the last one does.
///
/// Allocates one `Vec` per array actually read, sized by the values parsed
/// rather than by the count the array claims: a count is producer-written, and
/// `with_capacity(count)` would let a four-byte claim allocate gigabytes.
pub fn read_values(bytes: &[u8]) -> Result<Vec<Value<'_>>, ValueError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(ValueError::NotWordAligned);
    }
    let mut reader = Reader { bytes, at: 0 };
    let mut values = Vec::new();
    while reader.at < bytes.len() {
        values.push(reader.value(0)?);
    }
    Ok(values)
}

/// Read exactly one value that fills `bytes`.
pub fn read_value(bytes: &[u8]) -> Result<Value<'_>, ValueError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(ValueError::NotWordAligned);
    }
    let mut reader = Reader { bytes, at: 0 };
    let value = reader.value(0)?;
    if reader.at != bytes.len() {
        // More after the one value: a writer that meant two values meant a
        // different message, and reading only the first would drop the rest.
        return Err(ValueError::Truncated);
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ValueError> {
        let end = self.at.checked_add(count).ok_or(ValueError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(ValueError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn word(&mut self) -> Result<u32, ValueError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn eight(&mut self) -> Result<[u8; 8], ValueError> {
        let bytes = self.take(8)?;
        let mut out = [0u8; 8];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// A length-prefixed run of bytes and its zero padding.
    fn sized(&mut self) -> Result<&'a [u8], ValueError> {
        let length = self.word()? as usize;
        let body = self.take(length)?;
        let pad = (4 - length % 4) % 4;
        if self.take(pad)?.iter().any(|&byte| byte != 0) {
            return Err(ValueError::PaddingNotZero);
        }
        Ok(body)
    }

    fn text(&mut self) -> Result<&'a str, ValueError> {
        core::str::from_utf8(self.sized()?).map_err(|_| ValueError::NotUtf8)
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, ValueError> {
        let tag = self.word()?;
        Ok(match tag {
            TAG_NULL => Value::Null,
            TAG_FALSE => Value::Bool(false),
            TAG_TRUE => Value::Bool(true),
            TAG_U32 => Value::U32(self.word()?),
            TAG_I32 => Value::I32(self.word()? as i32),
            TAG_F64 => Value::F64(f64::from_le_bytes(self.eight()?)),
            TAG_U64 => Value::U64(u64::from_le_bytes(self.eight()?)),
            TAG_I64 => Value::I64(i64::from_le_bytes(self.eight()?)),
            TAG_STRING => Value::Str(self.text()?),
            TAG_BYTES => Value::Bytes(self.sized()?),
            TAG_JSON => Value::Json(self.text()?),
            TAG_ARRAY => {
                if depth + 1 >= MAX_DEPTH {
                    return Err(ValueError::TooDeep);
                }
                let count = self.word()?;
                // Every value is at least one word, so a count the remaining
                // bytes cannot hold is refused before any is read.
                if count as usize > (self.bytes.len() - self.at) / 4 {
                    return Err(ValueError::Truncated);
                }
                let mut values = Vec::new();
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                Value::Array(values)
            }
            other => return Err(ValueError::UnknownTag(other)),
        })
    }
}

/// Builds a run of values.
///
/// The host writes answers with it; the tests write requests with it, which is
/// what the producer's JavaScript writer is checked against.
#[derive(Clone, Debug, Default)]
pub struct ValueWriter {
    bytes: Vec<u8>,
}

impl ValueWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append to an existing buffer, which must already end on a word.
    pub fn over(bytes: Vec<u8>) -> Self {
        debug_assert!(bytes.len().is_multiple_of(4));
        Self { bytes }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn word(&mut self, word: u32) {
        self.bytes.extend_from_slice(&word.to_le_bytes());
    }

    fn sized(&mut self, tag: u32, body: &[u8]) {
        self.word(tag);
        // A value longer than a u32 can name is not one this format carries;
        // every caller bounds its payload far below that first.
        self.word(u32::try_from(body.len()).expect("a value payload fits in a u32 length"));
        self.bytes.extend_from_slice(body);
        let pad = (4 - body.len() % 4) % 4;
        self.bytes.extend(core::iter::repeat_n(0u8, pad));
    }

    pub fn null(&mut self) {
        self.word(TAG_NULL);
    }

    pub fn bool(&mut self, value: bool) {
        self.word(if value { TAG_TRUE } else { TAG_FALSE });
    }

    pub fn u32(&mut self, value: u32) {
        self.word(TAG_U32);
        self.word(value);
    }

    pub fn i32(&mut self, value: i32) {
        self.word(TAG_I32);
        self.word(value as u32);
    }

    pub fn f64(&mut self, value: f64) {
        self.word(TAG_F64);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.word(TAG_U64);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.word(TAG_I64);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn str(&mut self, value: &str) {
        self.sized(TAG_STRING, value.as_bytes());
    }

    pub fn bytes(&mut self, value: &[u8]) {
        self.sized(TAG_BYTES, value);
    }

    pub fn json(&mut self, value: &str) {
        self.sized(TAG_JSON, value.as_bytes());
    }

    /// Start an array of `count` values; the caller writes them next.
    pub fn array(&mut self, count: u32) {
        self.word(TAG_ARRAY);
        self.word(count);
    }

    /// Append a value that was read elsewhere, unchanged.
    pub fn value(&mut self, value: &Value<'_>) {
        match value {
            Value::Null => self.null(),
            Value::Bool(value) => self.bool(*value),
            Value::U32(value) => self.u32(*value),
            Value::I32(value) => self.i32(*value),
            Value::F64(value) => self.f64(*value),
            Value::U64(value) => self.u64(*value),
            Value::I64(value) => self.i64(*value),
            Value::Str(value) => self.str(value),
            Value::Bytes(value) => self.bytes(value),
            Value::Json(value) => self.json(value),
            Value::Array(values) => {
                self.array(values.len() as u32);
                for value in values {
                    self.value(value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind(writer: &mut ValueWriter) {
        writer.null();
        writer.bool(false);
        writer.bool(true);
        writer.u32(u32::MAX);
        writer.i32(-7);
        writer.f64(-0.5);
        // Past 2^53 on purpose: the producer writes these from a BigInt, and a
        // Number anywhere on its path would round this one.
        writer.u64((1 << 53) + 1);
        writer.i64(i64::MIN);
        writer.str("héllo");
        writer.bytes(&[1, 2, 3]);
        writer.json("{\"a\":1}");
        writer.array(2);
        writer.u32(1);
        writer.array(2);
        writer.u32(640);
        writer.u32(480);
    }

    #[test]
    fn every_kind_survives_the_round_trip() {
        let mut writer = ValueWriter::new();
        every_kind(&mut writer);
        let bytes = writer.into_bytes();
        let values = read_values(&bytes).expect("a run this module wrote");
        assert_eq!(
            values,
            vec![
                Value::Null,
                Value::Bool(false),
                Value::Bool(true),
                Value::U32(u32::MAX),
                Value::I32(-7),
                Value::F64(-0.5),
                Value::U64((1 << 53) + 1),
                Value::I64(i64::MIN),
                Value::Str("héllo"),
                Value::Bytes(&[1, 2, 3]),
                Value::Json("{\"a\":1}"),
                Value::Array(vec![
                    Value::U32(1),
                    Value::Array(vec![Value::U32(640), Value::U32(480)])
                ]),
            ]
        );
        // And back out unchanged, through the owned form.
        let mut again = ValueWriter::new();
        for value in &values {
            value.to_owned_value().write_to(&mut again);
        }
        assert_eq!(again.into_bytes(), bytes);
    }

    #[test]
    fn an_empty_string_is_a_length_and_no_padding() {
        let mut writer = ValueWriter::new();
        writer.str("");
        assert_eq!(writer.into_bytes(), [8, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn padding_that_is_not_zero_is_refused() {
        let mut writer = ValueWriter::new();
        writer.str("a");
        let mut bytes = writer.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = 1;
        assert_eq!(read_values(&bytes), Err(ValueError::PaddingNotZero));
    }

    #[test]
    fn bits_above_the_tag_are_refused() {
        let bytes = (TAG_NULL | 0x100).to_le_bytes();
        assert_eq!(
            read_values(&bytes),
            Err(ValueError::UnknownTag(0x100)),
            "a tag word with high bits set is a later version's, not this one's"
        );
    }

    #[test]
    fn a_length_past_the_end_is_refused_without_reading_it() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&TAG_BYTES.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_values(&bytes), Err(ValueError::Truncated));
    }

    #[test]
    fn an_array_count_the_bytes_cannot_hold_is_refused_before_allocating() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&TAG_ARRAY.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_values(&bytes), Err(ValueError::Truncated));
    }

    #[test]
    fn text_that_is_not_utf8_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&TAG_STRING.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0, 0, 0]);
        assert_eq!(read_values(&bytes), Err(ValueError::NotUtf8));
    }

    #[test]
    fn nesting_is_bounded() {
        let mut bytes = Vec::new();
        for _ in 0..MAX_DEPTH {
            bytes.extend_from_slice(&TAG_ARRAY.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
        }
        bytes.extend_from_slice(&TAG_NULL.to_le_bytes());
        assert_eq!(read_values(&bytes), Err(ValueError::TooDeep));
    }

    #[test]
    fn one_value_means_exactly_one() {
        let mut writer = ValueWriter::new();
        writer.u32(1);
        writer.u32(2);
        assert_eq!(
            read_value(&writer.into_bytes()),
            Err(ValueError::Truncated),
            "a second value after the one asked for is not silently dropped"
        );
    }

    #[test]
    fn a_run_that_is_not_whole_words_is_refused() {
        assert_eq!(read_values(&[0, 0, 0]), Err(ValueError::NotWordAligned));
    }
}
