use base64::{Engine as _, engine::general_purpose};
#[cfg(feature = "codec-gbk")]
use encoding_rs::GBK;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    // `ucs2` / `ucs-2` too: the reference reads them little-endian, as `utf16le`.
    Utf16Le,
    Ascii,
    Latin1,
    Base64,
    Hex,
    #[cfg(feature = "codec-gbk")]
    Gbk,
    Unsupported,
}

#[inline(always)]
fn normalize_encoding(input: &str) -> Encoding {
    let s = input.trim();
    if s.eq_ignore_ascii_case("utf8") || s.eq_ignore_ascii_case("utf-8") {
        Encoding::Utf8
    } else if s.eq_ignore_ascii_case("utf16le")
        || s.eq_ignore_ascii_case("utf-16le")
        || s.eq_ignore_ascii_case("ucs2")
        || s.eq_ignore_ascii_case("ucs-2")
    {
        Encoding::Utf16Le
    } else if s.eq_ignore_ascii_case("ascii") {
        Encoding::Ascii
    } else if s.eq_ignore_ascii_case("latin1") || s.eq_ignore_ascii_case("binary") {
        Encoding::Latin1
    } else if s.eq_ignore_ascii_case("base64") {
        Encoding::Base64
    } else if s.eq_ignore_ascii_case("hex") {
        Encoding::Hex
    } else {
        #[cfg(feature = "codec-gbk")]
        if s.eq_ignore_ascii_case("gbk") {
            return Encoding::Gbk;
        }
        Encoding::Unsupported
    }
}

#[inline(always)]
fn strip_data_url_base64(s: &str) -> &str {
    // Only strip when it looks like a data URL with base64.
    // Example: "data:image/png;base64,AAAA..."
    let lower = s.as_bytes();
    if lower.len() >= 5 && &lower[..5].to_ascii_lowercase() == b"data:" {
        if let Some(idx) = s.find(";base64,") {
            return &s[(idx + ";base64,".len())..];
        }
    }
    s
}

#[inline(always)]
pub fn encode_string(data: &str, encoding: &str) -> Result<Vec<u8>, String> {
    match normalize_encoding(encoding) {
        Encoding::Utf8 => Ok(data.as_bytes().to_vec()),

        Encoding::Ascii => {
            // Match Node-style behavior: mask to 7-bit for each char.
            // For non-ASCII chars, take the low 7 bits of the code point.
            Ok(data.chars().map(|c| (c as u32 as u8) & 0x7F).collect())
        }

        Encoding::Latin1 => {
            // Only allow U+0000..U+00FF
            if data.chars().all(|c| (c as u32) <= 0xFF) {
                Ok(data.chars().map(|c| c as u8).collect())
            } else {
                Err("Latin1 encode error: out of range".to_string())
            }
        }

        Encoding::Base64 => {
            let pure = strip_data_url_base64(data);
            general_purpose::STANDARD
                .decode(pure)
                .map_err(|_| "Base64 decode error".to_string())
        }

        Encoding::Hex => hex::decode(data).map_err(|_| "Hex decode error".to_string()),

        Encoding::Utf16Le => {
            let mut buf = Vec::with_capacity(data.len() * 2);
            for c in data.encode_utf16() {
                buf.extend_from_slice(&c.to_le_bytes());
            }
            Ok(buf)
        }

        #[cfg(feature = "codec-gbk")]
        Encoding::Gbk => {
            let (bytes, _, had_errors) = GBK.encode(data);
            if had_errors {
                Err("GBK encode error".to_string())
            } else {
                Ok(bytes.into_owned())
            }
        }

        Encoding::Unsupported => Err(format!("Unsupported encoding: {encoding}")),
    }
}

#[inline(always)]
pub fn decode_bytes(data: &[u8], encoding: &str) -> Result<String, String> {
    match normalize_encoding(encoding) {
        Encoding::Utf8 => std::str::from_utf8(data)
            .map(str::to_owned)
            .map_err(|_| "UTF-8 decode error".to_string()),

        Encoding::Ascii => {
            // Node-style: bytes & 0x7F -> char
            Ok(data.iter().map(|&b| (b & 0x7F) as char).collect())
        }

        Encoding::Latin1 => Ok(data.iter().map(|&b| b as char).collect()),

        Encoding::Base64 => Ok(general_purpose::STANDARD.encode(data)),

        Encoding::Hex => Ok(hex::encode(data)),

        Encoding::Utf16Le => {
            if data.len() % 2 != 0 {
                return Err("UTF-16LE decode error: odd number of bytes".to_string());
            }
            let utf16: Vec<u16> = data
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16(&utf16).map_err(|_| "UTF-16LE decode error".to_string())
        }

        #[cfg(feature = "codec-gbk")]
        Encoding::Gbk => {
            let (s, _, had_errors) = GBK.decode(data);
            if had_errors {
                Err("GBK decode error".to_string())
            } else {
                Ok(s.into_owned())
            }
        }

        Encoding::Unsupported => Err(format!("Unsupported encoding: {encoding}")),
    }
}
